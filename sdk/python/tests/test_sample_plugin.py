"""样例插件合规冒烟测试（D1-02）：

- MAN-01：plugin.json 通过 docs/spec/plugin-manifest.schema.json 校验；
- MAN-02：id 与目录名一致；
- BEH-01/02/05/06 语义的子进程级断言：以子进程拉起 `python main.py`，
  按 E 路回放最小序列（initialize → schema → can_handle → load_file → parse
  → key_values → unload_file → shutdown），逐帧校验。
"""

import json
import os
import subprocess
import sys
from pathlib import Path

import jsonschema

REPO_ROOT = Path(__file__).resolve().parents[3]
SDK_DIR = Path(__file__).resolve().parents[1]
PLUGIN_DIR = SDK_DIR / "examples" / "sample-plugin"

MANIFEST_SCHEMA = REPO_ROOT / "docs" / "spec" / "plugin-manifest.schema.json"
FIXTURE = PLUGIN_DIR / "sample.log"

FID = "6f0c1d2a-4a01-4e7b-9b2c-3d0e5f6a7b8c"


def load_manifest():
    return json.loads((PLUGIN_DIR / "plugin.json").read_text(encoding="utf-8"))


def test_manifest_passes_contract_schema_and_dir_name_match():
    manifest = load_manifest()
    schema = json.loads(MANIFEST_SCHEMA.read_text(encoding="utf-8"))
    jsonschema.validate(manifest, schema)  # MAN-01：Schema 校验通过
    assert manifest["id"] == PLUGIN_DIR.name == "sample-plugin"  # MAN-02
    assert manifest["entry"]["command"] == "python"
    assert manifest["entry"]["args"] == ["main.py"]


def start_session():
    """子进程方式拉起样例插件。"""
    env = dict(os.environ, PYTHONPATH=str(SDK_DIR))
    proc = subprocess.Popen(
        [sys.executable, "main.py"],
        cwd=str(PLUGIN_DIR),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
    )
    return proc


def send(proc, request: str):
    proc.stdin.write((request + "\n").encode("utf-8"))
    proc.stdin.flush()


def recv(proc) -> dict:
    line = proc.stdout.readline()
    if not line:
        raise AssertionError("plugin exited prematurely (stdout EOF)")
    return json.loads(line.decode("utf-8"))


def recv_until_response(proc, rid: int):
    """收帧直到某请求的响应到达；收集期间的 progress/RecordBatch 通知。"""
    frames = []
    while True:
        frame = recv(proc)
        frames.append(frame)
        if "id" in frame and frame["id"] == rid:
            return frames


def req(method, params, rid):
    return json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})


def test_full_session_replay_beh_semantics():
    proc = start_session()
    try:
        # 按 E 路回放顺序（docs-validator.md §3.5）逐请求收发，parse 完成后再 shutdown。
        send(proc, req("initialize",
                       {"protocol_version": 1,
                        "host_info": {"name": "AnalysisBuddy", "version": "0.1.0"}}, 1))
        init_frame = recv_until_response(proc, 1)
        send(proc, req("schema", None, 2))
        schema_frame = recv_until_response(proc, 2)
        send(proc, req("can_handle",
                       {"path": str(FIXTURE), "name": "sample.log", "ext": "log",
                        "size_bytes": os.path.getsize(FIXTURE),
                        "head_sample": FIXTURE.read_text(encoding="utf-8")[:4096]}, 3))
        can_frame = recv_until_response(proc, 3)
        send(proc, req("load_file", {"file_id": FID, "path": str(FIXTURE)}, 4))
        load_frame = recv_until_response(proc, 4)
        send(proc, req("parse", {"file_id": FID}, 5))
        parse_frames = recv_until_response(proc, 5)  # 含 progress/RecordBatch + 最终响应
        # T = 2026-08-07T10:01:20Z，晚于夹具末行时间戳（10:00:08），取 ≤T 最新状态。
        send(proc, req("key_values", {"file_id": FID, "timestamp_ms": 1786096880000}, 6))
        kv_frame = recv_until_response(proc, 6)
        send(proc, req("unload_file", {"file_id": FID}, 7))
        unload_frame = recv_until_response(proc, 7)
        send(proc, req("shutdown", None, 8))
        shutdown_frame = recv_until_response(proc, 8)
        proc.stdin.close()
        rc = proc.wait(timeout=10)
    finally:
        if proc.poll() is None:
            proc.kill()
    assert rc == 0

    def result(frame):
        return frame["result"]

    # BEH-01：initialize 元数据齐全且 id 与 manifest 一致。
    init = result(init_frame[-1])
    assert init["id"] == load_manifest()["id"]
    for field in ("id", "name", "version", "capabilities"):
        assert field in init
    assert init["capabilities"]["annotate"] is False

    # BEH-02：响应 id 与请求匹配（recv_until_response 以 id 收敛即隐含）。

    # BEH-03 前置：必选方法均返回 result 而非 -32601。
    for frames in (init_frame, schema_frame, can_frame, load_frame, parse_frames,
                   kv_frame, unload_frame, shutdown_frame):
        assert "error" not in frames[-1], "frames={0}".format(frames)

    # can_handle 认领自带 fixture。
    can = result(can_frame[-1])
    assert can["can_handle"] is True
    assert 0.0 <= can["confidence"] <= 1.0

    # BEH-05 前置：schema 声明集合覆盖 parse 产出 metric。
    schema_ids = {m["id"] for m in result(schema_frame[-1])["metrics"]}
    assert schema_ids == {"fps", "frame_ms", "cpu_temp"}

    # BEH-05：Record 三必填字段 + metric ∈ schema；BEH-04：parse 期间有 progress。
    batches = [f["params"] for f in parse_frames if f.get("method") == "RecordBatch"]
    progresses = [f["params"] for f in parse_frames if f.get("method") == "progress"]
    assert batches, "no RecordBatch emitted"
    assert progresses, "no progress notification during parse"
    total = 0
    for b in batches:
        assert b["file_id"] == FID
        assert isinstance(b["done"], bool)
        for r in b["records"]:
            assert set(("timestamp", "metric", "value")) <= set(r)
            assert r["metric"] in schema_ids
            assert isinstance(r["value"], (int, float))
            assert not isinstance(r["value"], bool)
        total += len(b["records"])

    # BEH-06：seq 无缺号；records_total 等于各批之和。
    seqs = [b["seq"] for b in batches]
    assert seqs == list(range(len(seqs)))
    assert batches[-1]["done"] is True
    assert result(parse_frames[-1])["records_total"] == total
    # 12 行样例日志 → 9 条指标行。
    assert total == 9

    # BEH-07 前置：key_values 结构正确（scene 状态 ≤T 最新）。
    entries = result(kv_frame[-1])["entries"]
    scenes = {e["key"]: e["value"] for e in entries}
    assert scenes.get("scene") == "boss_fight"

    # BEH-10/12：shutdown 后进程退出，退出码 0（上面 rc 已断言）。
    assert result(unload_frame[-1]) == {}
    assert result(shutdown_frame[-1]) == {}

    # 协议流量纯净：stdout 每行都是合法 JSON-RPC 帧（BEH-09 语义）。
    all_frames = (init_frame + schema_frame + can_frame + load_frame + parse_frames
                  + kv_frame + unload_frame + shutdown_frame)
    assert all(f["jsonrpc"] == "2.0" for f in all_frames)


def test_load_missing_file_returns_neg_32002():
    proc = start_session()
    try:
        send(proc, req("load_file",
                       {"file_id": FID, "path": str(PLUGIN_DIR / "no-such.log")}, 1))
        frames = recv_until_response(proc, 1)
        err = frames[-1]["error"]
        send(proc, req("shutdown", None, 2))
        recv_until_response(proc, 2)
        proc.stdin.close()
        rc = proc.wait(timeout=10)
    finally:
        if proc.poll() is None:
            proc.kill()
    assert err["code"] == -32002
    assert rc == 0
