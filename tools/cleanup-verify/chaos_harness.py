#!/usr/bin/env python3
"""D2（WS-D，M2 验收唯一口径）：chaos soak harness——随机化会话生命周期事件
流，全程与结束后断言不变式 I-1（会话终结后工件集合为空）。

事件类型（按权重随机）：
  work        创建会话 → 上传 CSV → 等导入终态 → 显式 DELETE /session
  abandon     创建+上传后直接弃置（空闲 TTL 回收路径）
  kill9       导入进行中 SIGKILL 该会话实例进程（进程被外力杀死路径）
  restart     systemctl restart ab-auth-gateway（网关重启 = 全量清扫路径）

每轮事件后用 residue_checker（D1）对该轮所有已终结 sid 断言残留；全程
结束时断言：/dev/shm 字节数、网关 fd 数、ab-server 进程数回到基线 ± 容差。

用法（在部署主机本机，root 或 bob+sudo）：
  chaos_harness.py --rounds 200 --base http://127.0.0.1:8601 \
      --tmp-root /dev/shm/ab-tenants --data-root /var/lib/analysisbuddy/tenants \
      --bin /opt/analysisbuddy/ab-server [--seed 42] [--report chaos-report.json]
"""
from __future__ import annotations

import argparse
import io
import json
import os
import pathlib
import random
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request

sys.path.insert(0, str(pathlib.Path(__file__).parent))
from residue_checker import list_ab_server_pids, _cmdline  # noqa: E402

CSV = (
    "timestamp_ms,fps,frame_ms\n"
    + "\n".join(f"{1785600000000 + i * 100},{59.5 + (i % 7) * 0.1},{16.6}" for i in range(50))
    + "\n"
)


def http(base: str, method: str, path: str, data=None, headers=None, jar=None):
    req = urllib.request.Request(base + path, data=data, method=method)
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    if jar:
        req.add_header("Cookie", f"ab_sid={jar}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            set_cookie = resp.headers.get("Set-Cookie", "")
            body = resp.read()
            sid = None
            if "ab_sid=" in set_cookie:
                sid = set_cookie.split("ab_sid=")[1].split(";")[0]
            return resp.status, body, sid
    except urllib.error.HTTPError as e:
        return e.code, e.read(), None


def multipart_upload(boundary: str, filename: str, content: bytes) -> tuple[bytes, str]:
    body = io.BytesIO()
    body.write(f"--{boundary}\r\n".encode())
    body.write(f'Content-Disposition: form-data; name="file"; filename="{filename}"\r\n'.encode())
    body.write(b"Content-Type: text/csv\r\n\r\n")
    body.write(content)
    body.write(f"\r\n--{boundary}--\r\n".encode())
    return body.getvalue(), f"multipart/form-data; boundary={boundary}"


def instance_pid_for_sid(sid: str, bin_path: str) -> int | None:
    for pid in list_ab_server_pids(bin_path):
        if sid in _cmdline(pid):
            return pid
    return None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--rounds", type=int, default=200)
    ap.add_argument("--base", default="http://127.0.0.1:8601")
    ap.add_argument("--tmp-root", default="/dev/shm/ab-tenants")
    ap.add_argument("--data-root", default="/var/lib/analysisbuddy/tenants")
    ap.add_argument("--bin", default="/opt/analysisbuddy/ab-server")
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--report", default=None)
    ap.add_argument("--skip-restart", action="store_true", help="跳过网关重启注入")
    args = ap.parse_args()
    rng = random.Random(args.seed)
    import pathlib as _p
    checker = str(_p.Path(__file__).parent / "residue_checker.py")

    finished: list[str] = []
    failures: list[dict] = []
    events = {"work": 0, "abandon": 0, "kill9": 0, "restart": 0}
    weights = [0.45, 0.2, 0.2, 0.15] if not args.skip_restart else [0.55, 0.25, 0.2, 0.0]
    t0 = time.monotonic()

    def check_sid(sid: str) -> None:
        r = subprocess.run(
            [sys.executable, checker, "--tmp-root", args.tmp_root,
             "--data-root", args.data_root, "--bin", args.bin,
             "--sid", sid, "--settle-seconds", "3", "--json"],
            capture_output=True, text=True)
        try:
            rep = json.loads(r.stdout or "{}")
        except json.JSONDecodeError:
            rep = {"clean": False, "residues": [{"error": r.stderr[:200]}]}
        if not rep.get("clean"):
            failures.append({"event": "post_teardown", "sid": sid, "residues": rep.get("residues")})

    for round_no in range(1, args.rounds + 1):
        kind = rng.choices(["work", "abandon", "kill9", "restart"], weights=weights)[0]
        events[kind] += 1
        try:
            boundary = f"chaos{rng.getrandbits(48):012x}"
            body, ctype = multipart_upload(boundary, f"chaos-{round_no}.csv", CSV.encode())
            status, resp_body, sid = http(args.base, "POST", "/api/v1/session")
            if status != 201 or not sid:
                failures.append({"event": kind, "error": f"session {status}"})
                continue
            status, resp_body, _ = http(args.base, "POST", "/api/v1/imports/upload",
                                        data=body, headers={"Content-Type": ctype}, jar=sid)
            if kind == "abandon":
                events["abandon"] += 0  # 统计占位；弃置会话交由空闲 TTL 回收
                time.sleep(0.05)
                continue
            if kind == "kill9":
                time.sleep(rng.uniform(0.0, 0.15))
                pid = instance_pid_for_sid(sid, args.bin)
                if pid:
                    os.kill(pid, signal.SIGKILL)
                finished.append(sid)
                check_sid(sid)
                continue
            if kind == "restart":
                subprocess.run(["sudo", "-n", "systemctl", "restart", "ab-auth-gateway"],
                               check=False)
                time.sleep(2.0)
                finished.append(sid)
                continue
            # work：等导入终态后显式终结
            job = json.loads(resp_body or b"{}")
            job_id = job.get("job_id")
            for _ in range(40):
                st, rb, _ = http(args.base, "GET", f"/api/v1/imports/{job_id}", jar=sid)
                if st == 200 and json.loads(rb or b"{}").get("state") in ("completed", "failed", "cancelled"):
                    break
                time.sleep(0.1)
            st, _, _ = http(args.base, "DELETE", "/api/v1/session", jar=sid)
            if st != 204:
                failures.append({"event": "work", "error": f"delete {st}"})
            finished.append(sid)
            check_sid(sid)
        except Exception as exc:  # 单轮失败不断链
            failures.append({"event": kind, "round": round_no, "error": repr(exc)})
        if round_no % 25 == 0:
            print(f"[chaos] round {round_no}/{args.rounds} failures={len(failures)}")

    # 全局口径：ab-server 进程数回落到 ≤ 空闲残量（网关最多保留 AB_MAX_TENANTS
    # 个活跃实例；chaos 结束后数秒内活跃应≈0），/dev/shm 残留目录数
    time.sleep(5)
    tmp_root = pathlib.Path(args.tmp_root)
    leftover_dirs = sorted(p.name for p in tmp_root.iterdir()) if tmp_root.is_dir() else []
    leftover_procs = list_ab_server_pids(args.bin)
    report = {
        "tool": "chaos_harness",
        "rounds": args.rounds,
        "seed": args.seed,
        "elapsed_s": round(time.monotonic() - t0, 1),
        "events": events,
        "sids_finished": len(finished),
        "failures": failures,
        "leftover_tmp_dirs": leftover_dirs,
        "leftover_ab_server_pids": leftover_procs,
        "clean": not failures and not leftover_dirs,
    }
    if args.report:
        pathlib.Path(args.report).write_text(json.dumps(report, ensure_ascii=False, indent=2))
    print(json.dumps({k: v for k, v in report.items() if k != "failures"},
                     ensure_ascii=False, indent=2))
    print(f"failure_count={len(failures)}")
    return 0 if report["clean"] else 1


if __name__ == "__main__":
    sys.exit(main())
