# demo-tool 端到端冒烟：以真实 SDK（若已安装）serve() 驱动完整 NDJSON 会话，
# 断言 10 method 路由、schema 3 指标、parse 记录数 == FRAME 行数 ×3、key_values、
# annotate、shutdown 后退出码 0。开发机已 `pip install -e sdk/python` 时执行。
#
# 时序按宿主行为：parse 响应（records_total）到达后才发后续请求与 shutdown
# （protocol-v1.md §2.9：shutdown 收到即退出，此时 parse 早已完成）。

import json
import os
import subprocess
import sys
import time

import pytest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIXTURE = os.path.join(ROOT, "tests", "fixtures", "small_txt.log")


def _count_frames(path):
    with open(path, encoding="utf-8") as f:
        return sum(1 for line in f if " FRAME " in line)


def _request(req_id, method, params=None):
    msg = {"jsonrpc": "2.0", "id": req_id, "method": method}
    if params is not None:
        msg["params"] = params
    return json.dumps(msg, ensure_ascii=False) + "\n"


def test_serve_end_to_end():
    frame_rows = _count_frames(FIXTURE)
    proc = subprocess.Popen(
        [sys.executable, "main.py"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=ROOT,
    )
    assert proc.stdin is not None and proc.stdout is not None

    frames = []

    def readline():
        # 协议帧只允许 LF（protocol-v1.md §1.2）；text 模式在 Windows 会把 \n 转成
        # \r\n 导致 SDK 判协议错，故用二进制模式逐行读并手动解码。
        raw = proc.stdout.readline()
        if not raw:
            raise AssertionError("stdout closed unexpectedly")
        return raw.decode("utf-8", errors="replace")

    def send_and_read(req_id, method, params=None):
        proc.stdin.write(_request(req_id, method, params).encode("utf-8"))
        proc.stdin.flush()
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            frame = json.loads(readline())
            if frame.get("method") == "progress":
                continue  # 心跳帧不参与断言
            frames.append(frame)
            return frame
        raise AssertionError(f"timeout waiting for response to {method}")

    init = send_and_read(1, "initialize")
    assert init["result"]["id"] == "demo-tool"
    assert init["result"]["capabilities"]["annotate"] is True

    schema = send_and_read(2, "schema")
    metrics = schema["result"]["metrics"]
    assert [m["id"] for m in metrics] == ["fps", "frame_time", "cpu_temp"]
    assert [m["aggregation"] for m in metrics] == ["last", "avg", "max"]

    can = send_and_read(3, "can_handle", {
        "path": FIXTURE, "name": "small_txt.log", "ext": "log",
        "size_bytes": os.path.getsize(FIXTURE),
        "head_sample": open(FIXTURE, encoding="utf-8").read(1024),
    })
    assert can["result"]["can_handle"] is True

    load = send_and_read(4, "load_file", {"file_id": "f1", "path": FIXTURE})
    assert load["result"]["record_count_hint"] == frame_rows

    # parse：发送一次请求，收集 RecordBatch 帧直到 records_total 响应
    proc.stdin.write(_request(5, "parse", {"file_id": "f1"}).encode("utf-8"))
    proc.stdin.flush()
    parse = None
    batches = []
    deadline = time.monotonic() + 10
    while parse is None:
        if time.monotonic() > deadline:
            raise AssertionError("timeout waiting for parse response")
        frame = json.loads(readline())
        if frame.get("method") == "RecordBatch":
            batches.append(frame)
        elif frame.get("id") == 5:
            parse = frame
    records = [r for b in batches for r in b["params"]["records"]]
    assert len(records) == frame_rows * 3
    assert parse["result"]["records_total"] == frame_rows * 3

    kv = send_and_read(6, "key_values", {"file_id": "f1", "timestamp_ms": 1786068015000})
    keys = {e["key"] for e in kv["result"]["entries"]}
    assert {"scene", "hero_hp", "last_event"} <= keys

    ann = send_and_read(7, "annotate", {
        "file_id": "f1", "range": {"start_ms": 1786068000000, "end_ms": 1786068012481},
    })
    assert len(ann["result"]["events"]) == 1
    assert ann["result"]["events"][0]["label"] == "GPU hang"

    unload = send_and_read(8, "unload_file", {"file_id": "f1"})
    assert unload["result"] == {}

    shutdown = send_and_read(9, "shutdown")
    assert shutdown["result"] == {}

    proc.stdin.close()
    code = proc.wait(timeout=5)
    assert code == 0, f"exit code {code}, stderr={proc.stderr.read().decode(errors='replace')}"
