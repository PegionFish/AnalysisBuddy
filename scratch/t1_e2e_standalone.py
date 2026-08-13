# T1 standalone e2e (mirrors tests/test_e2e.py, no pytest needed)
import json
import os
import subprocess
import sys
import time

ROOT = os.path.abspath("plugins/demo-tool")
FIXTURE = os.path.join(ROOT, "tests", "fixtures", "small_txt.log")

with open(FIXTURE, encoding="utf-8") as f:
    frame_rows = sum(1 for line in f if " FRAME " in line)

proc = subprocess.Popen(
    [sys.executable, "main.py"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    cwd=ROOT,
)
assert proc.stdin is not None and proc.stdout is not None

def readline():
    raw = proc.stdout.readline()
    if not raw:
        raise AssertionError("stdout closed unexpectedly; stderr=" + proc.stderr.read().decode(errors="replace"))
    return raw.decode("utf-8", errors="replace")

def send_and_read(req_id, method, params=None):
    msg = {"jsonrpc": "2.0", "id": req_id, "method": method}
    if params is not None:
        msg["params"] = params
    proc.stdin.write((json.dumps(msg, ensure_ascii=False) + "\n").encode("utf-8"))
    proc.stdin.flush()
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        frame = json.loads(readline())
        if frame.get("method") == "progress":
            continue
        return frame
    raise AssertionError(f"timeout waiting for {method}")

init = send_and_read(1, "initialize")
assert init["result"]["id"] == "demo-tool", init
assert init["result"]["capabilities"]["annotate"] is True

can = send_and_read(2, "can_handle", {
    "path": FIXTURE, "name": "small_txt.log", "ext": "log",
    "size_bytes": os.path.getsize(FIXTURE),
    "head_sample": open(FIXTURE, encoding="utf-8").read(1024),
})
assert can["result"]["can_handle"] is True and can["result"]["confidence"] > 0, can

load = send_and_read(3, "load_file", {"file_id": "f1", "path": FIXTURE})
assert load["result"]["record_count_hint"] == frame_rows, load

proc.stdin.write((json.dumps({"jsonrpc": "2.0", "id": 4, "method": "parse", "params": {"file_id": "f1"}}) + "\n").encode("utf-8"))
proc.stdin.flush()
parse = None
batches = []
deadline = time.monotonic() + 10
while parse is None:
    if time.monotonic() > deadline:
        raise AssertionError("timeout waiting for parse")
    frame = json.loads(readline())
    if frame.get("method") == "RecordBatch":
        batches.append(frame)
    elif frame.get("id") == 4:
        parse = frame
records = [r for b in batches for r in b["params"]["records"]]
assert len(records) == frame_rows * 3, (len(records), frame_rows)
assert parse["result"]["records_total"] == frame_rows * 3, parse
# 首条 FRAME 在首个 STATE 之前 → 无 scene tag；STATE scene=main_menu 之后的 FRAME 带 scene
assert "tags" not in records[0], records[0]
assert records[3]["tags"] == {"scene": "main_menu"}, records[3]

shutdown = send_and_read(5, "shutdown")
assert shutdown["result"] == {}
proc.stdin.close()
code = proc.wait(timeout=5)
assert code == 0, f"exit={code} stderr={proc.stderr.read().decode(errors='replace')}"
print(f"E2E_OK frames={frame_rows} records={len(records)} exit={code}")
