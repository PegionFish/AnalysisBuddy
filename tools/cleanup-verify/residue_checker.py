#!/usr/bin/env python3
"""D1（WS-D，不变式 I-1 验收工具）：会话残留检查器。

断言：给定网关主机上，「某 sid 已终结」或「全部会话已终结」后，该 sid（或
全体）的工件集合 —— ① ab-server 进程 ② /dev/shm 暂存目录 ③ 租户数据目录
—— 在容差窗口内为空。输出 JSON（机器可判），退出码 0=干净 / 1=有残留 /
2=用法错误。

用法（在 160 这类部署主机上直接跑）：
  residue_checker.py --tmp-root /dev/shm/ab-tenants \
                     --data-root /var/lib/analysisbuddy/tenants \
                     --bin /opt/analysisbuddy/ab-server \
                     [--sid <sid>] [--json]

被 chaos harness（D2）与发布验收清单消费；编排者也可单独手跑。
"""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import sys
import time


def list_ab_server_pids(bin_path: str) -> list[int]:
    """精确匹配 cmdline 首段 == bin_path 的进程（绝不模式匹配全命令行）。"""
    pids: list[int] = []
    proc = pathlib.Path("/proc")
    if not proc.is_dir():  # 非 Linux（如 macOS 开发机）：无 /proc，视为无进程
        return pids
    for entry in proc.iterdir():
        if not entry.name.isdigit():
            continue
        try:
            argv0 = (entry / "cmdline").read_bytes().split(b"\0")[0].decode(
                "utf-8", "replace"
            )
        except OSError:
            continue
        if argv0 == bin_path:
            pids.append(int(entry.name))
    return pids


def dir_exists_with_entries(root: pathlib.Path, sid: str) -> bool:
    d = root / sid
    return d.is_dir() and any(d.iterdir())


def main() -> int:
    ap = argparse.ArgumentParser(description="session residue checker (I-1)")
    ap.add_argument("--tmp-root", default="/dev/shm/ab-tenants")
    ap.add_argument("--data-root", default="/var/lib/analysisbuddy/tenants")
    ap.add_argument("--bin", default="/opt/analysisbuddy/ab-server")
    ap.add_argument("--sid", help="只检查该 sid；缺省=检查全体")
    ap.add_argument("--settle-seconds", type=float, default=5.0,
                    help="等待工单清理的容差窗口（I-1 = 终结后 60s 内为空；"
                         "本工具的默认观察窗 5s，宽窗口由调用方轮询）")
    ap.add_argument("--json", action="store_true", help="输出 JSON")
    args = ap.parse_args()

    tmp_root = pathlib.Path(args.tmp_root)
    data_root = pathlib.Path(args.data_root)

    def sids() -> list[str]:
        names: set[str] = set()
        for root in (tmp_root, data_root):
            if root.is_dir():
                names.update(p.name for p in root.iterdir())
        return sorted(names)

    deadline = time.monotonic() + args.settle_seconds
    report: dict = {
        "tool": "residue_checker",
        "checked_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "bin": args.bin,
        "scope": args.sid or "all",
        "clean": False,
        "residues": [],
    }
    while True:
        residues: list[dict] = []
        if args.sid:
            targets = [args.sid]
        else:
            targets = sids()
        pids = list_ab_server_pids(args.bin)
        if args.sid:
            # sid 级：只关心该 sid 的进程（cmdline 含 --sessions-dir <root>/<sid>）
            pids = [
                pid for pid in pids
                if args.sid in _cmdline(pid)
            ]
        if pids and args.sid is None:
            # 全体口径下仅当「无任何租户目录」仍存活进程才算孤儿；
            # 正常运行中的进程是预期的，不算残留 —— 由调用方（D2 chaos）
            # 在 teardown 流程内以 --sid 模式逐会话判定。
            pass
        for pid in pids:
            residues.append({"kind": "process", "pid": pid})
        for sid in targets:
            if dir_exists_with_entries(tmp_root, sid):
                residues.append({"kind": "tmp_dir", "sid": sid})
            if dir_exists_with_entries(data_root, sid):
                residues.append({"kind": "data_dir", "sid": sid})
        if not residues or time.monotonic() >= deadline:
            report["clean"] = not residues
            report["residues"] = residues
            break
        time.sleep(0.5)

    if args.json:
        print(json.dumps(report, ensure_ascii=False, indent=2))
    else:
        status = "CLEAN" if report["clean"] else "RESIDUE"
        print(f"[{status}] scope={report['scope']} residues={len(report['residues'])}")
        for r in report["residues"]:
            print(f"  - {r}")
    return 0 if report["clean"] else 1


def _cmdline(pid: int) -> str:
    try:
        return pathlib.Path(f"/proc/{pid}/cmdline").read_bytes().decode(
            "utf-8", "replace"
        )
    except OSError:
        return ""


if __name__ == "__main__":
    sys.exit(main())
