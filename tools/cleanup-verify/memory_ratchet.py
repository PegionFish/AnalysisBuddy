#!/usr/bin/env python3
"""I5（M3 附属）：内存棘轮回归——三个「导入→卸载」周期下，同负载周期峰值
不应单调抬升（棘轮 = 泄漏/累积；卸载后 RSS 不立即回落属 glibc 分配器驻留，
非失败判据）。机器可判 JSON。

用法（部署主机本机）：
  memory_ratchet.py --base http://127.0.0.1:8601 --files 12 --rss-tolerance-pct 25
"""
from __future__ import annotations

import argparse
import io
import json
import pathlib
import sys
import time
import urllib.error
import urllib.request


def http(base, method, path, data=None, headers=None, jar=None):
    req = urllib.request.Request(base + path, data=data, method=method)
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    if jar:
        req.add_header("Cookie", f"ab_sid={jar}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            cookie = resp.headers.get("Set-Cookie", "")
            sid = cookie.split("ab_sid=")[1].split(";")[0] if "ab_sid=" in cookie else None
            return resp.status, resp.read(), sid
    except urllib.error.HTTPError as e:
        return e.code, e.read(), None


def upload(base, sid, filename, content):
    boundary = f"ratchet{int(time.time() * 1000)}"
    body = io.BytesIO()
    body.write(f"--{boundary}\r\n".encode())
    body.write(f'Content-Disposition: form-data; name="file"; filename="{filename}"\r\n\r\n'.encode())
    body.write(content)
    body.write(f"\r\n--{boundary}--\r\n".encode())
    st, resp, _ = http(base, "POST", "/api/v1/imports/upload", data=body.getvalue(),
                       headers={"Content-Type": f"multipart/form-data; boundary={boundary}"},
                       jar=sid)
    assert st == 202, f"upload {st}: {resp[:200]}"
    return json.loads(resp)


def rss_kb(pid):
    try:
        for line in pathlib.Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    except OSError:
        pass
    return 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8601")
    ap.add_argument("--files", type=int, default=12)
    ap.add_argument("--cycles", type=int, default=3)
    ap.add_argument("--rss-tolerance-pct", type=float, default=25.0)
    ap.add_argument("--report", default=None)
    args = ap.parse_args()

    _, _, sid = http(args.base, "GET", "/api/v1/metrics")
    assert sid, "未能取得会话（ab_sid）"

    rows = "\n".join(f"{1785600000000 + i * 100},{59.5 + i % 7},{16.6}" for i in range(200))
    csv = ("timestamp_ms,fps,frame_ms\n" + rows + "\n").encode()

    pid = None
    for entry in pathlib.Path("/proc").iterdir():
        if entry.name.isdigit():
            try:
                argv = (entry / "cmdline").read_bytes().split(b"\0")
            except OSError:
                continue
            if argv and argv[0].decode("utf-8", "replace").endswith("ab-server") and sid in argv[0].decode("utf-8", "replace"):
                pid = int(entry.name)
                break
            if any(sid in a.decode("utf-8", "replace") for a in argv):
                pid = int(entry.name)
                break
    if not pid:
        print("[ratchet] FAIL: 未找到本会话 ab-server 进程（需在宿主本机跑）")
        return 1

    baseline = rss_kb(pid)
    job_ids: list[str] = []
    file_ids: list[str] = []
    samples: list[dict] = []
    failures: list[str] = []
    cycle_peaks: list[int] = []

    def wait_terminal(job_id):
        for _ in range(60):
            st, resp, _ = http(args.base, "GET", f"/api/v1/imports/{job_id}", jar=sid)
            if st == 200 and json.loads(resp).get("state") in ("completed", "failed", "cancelled"):
                return json.loads(resp)
            time.sleep(0.05)
        return {}

    for cycle in range(1, args.cycles + 1):
        peak = 0
        for i in range(args.files):
            job = upload(args.base, sid, f"ratchet-c{cycle}-{i}.csv", csv)
            job_ids.append(job["job_id"])
            wait_terminal(job["job_id"])
            peak = max(peak, rss_kb(pid))
        samples.append({"cycle": cycle, "phase": "loaded", "rss_kb": peak})
        # 全量卸载
        for jid in job_ids:
            st, resp, _ = http(args.base, "GET", f"/api/v1/imports/{jid}", jar=sid)
            if st == 200:
                for f in json.loads(resp).get("files", []):
                    fid = f.get("file_id")
                    if fid and fid not in file_ids:
                        file_ids.append(fid)
                        http(args.base, "DELETE", f"/api/v1/files/{fid}", jar=sid)
        time.sleep(2)

        # 后周期峰值 vs 前周期峰值（I-5 棘轮判据）
        if cycle_peaks:
            growth = (peak - cycle_peaks[-1]) / max(cycle_peaks[-1], 1) * 100
            if growth > args.rss_tolerance_pct:
                failures.append(
                    f"跨周期棘轮: 峰值 {cycle_peaks[-1]}KB -> {peak}KB (+{growth:.0f}% > {args.rss_tolerance_pct}%)")
        cycle_peaks.append(peak)

    report = {
        "tool": "memory_ratchet",
        "files_per_cycle": args.files,
        "cycles": args.cycles,
        "baseline_kb": baseline,
        "cycle_peaks_kb": cycle_peaks,
        "unloaded_files": len(file_ids),
        "failures": failures,
        "clean": not failures,
    }
    if args.report:
        pathlib.Path(args.report).write_text(json.dumps(report, ensure_ascii=False, indent=2))
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main())
