"""serve() 主循环单测：九条行为契约（sdk-plugins.md §1.3）+ 错误码映射（§1.5）。"""

import io
import json
import os
import subprocess
import sys
import threading
import time

import pytest

from analysisbuddy import (
    AnalysisBuddyPlugin,
    CancelledError,
    FileLoadFailedError,
    InvalidParamsError,
    ParseFailedError,
    PluginBusyError,
    UnsupportedInV1Error,
)
from analysisbuddy.errors import SdkInternalError, build_error
from conftest import req, run_serve

FID = "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c"


class DummyPlugin(AnalysisBuddyPlugin):
    id, name, version = "dummy", "Dummy", "0.1.0"

    def __init__(self, **kw):
        super().__init__(**kw)
        self.events = []
        self.parse_sleep = 0.0
        self.parse_emit = 0
        self.raise_in = None
        self.load_paths = {}

    def on_load_file(self, p):
        self.events.append(("load", p["file_id"]))
        if self.raise_in == "load":
            raise FileLoadFailedError("file not found", data={"path": p["path"]})
        self.load_paths[p["file_id"]] = p["path"]
        return {}

    def on_unload_file(self, file_id):
        self.events.append(("unload", file_id))
        self.load_paths.pop(file_id, None)

    def on_parse(self, file_id, options, ctx):
        self.events.append(("parse", file_id))
        if self.raise_in == "parse":
            raise ParseFailedError("boom", data={"line": 42})
        if self.raise_in == "generic":
            raise RuntimeError("something exploded")
        total = 0
        for i in range(self.parse_emit):
            ctx.check_cancelled()
            ctx.emit_records([{"timestamp": i, "metric": "fps", "value": 60.0}])
            total += 1
            if self.parse_sleep:
                time.sleep(self.parse_sleep)
        return total


def error_of(frames, rid):
    for f in frames:
        if f.get("id") == rid and "error" in f:
            return f["error"]
    return None


def result_of(frames, rid):
    for f in frames:
        if f.get("id") == rid and "result" in f:
            return f["result"]
    return None


# ----------------------------------------------------------------------
# 路由与元数据
# ----------------------------------------------------------------------


def test_initialize_returns_metadata_and_capabilities():
    plugin = DummyPlugin()
    frames, _ = run_serve(plugin, [req("initialize", {"protocol_version": 1,
                                                       "host_info": {"name": "AnalysisBuddy",
                                                                     "version": "0.1.0"}}, rid=1)])
    r = result_of(frames, 1)
    assert r["id"] == "dummy"
    assert r["name"] == "Dummy"
    assert r["version"] == "0.1.0"
    assert r["capabilities"] == {"annotate": False, "subscribe": False, "binary_sidecar": False}


def test_initialize_annotate_capability_auto_detected():
    class Annotating(DummyPlugin):
        def on_annotate(self, file_id, range_):
            return {"events": []}

    frames, _ = run_serve(Annotating(), [req("initialize", {}, rid=1)])
    assert result_of(frames, 1)["capabilities"]["annotate"] is True


def test_decorator_handler_equivalent_to_override():
    plugin = DummyPlugin()

    @plugin.handler("can_handle")
    def can_handle(params):
        return {"can_handle": True, "confidence": 0.8}

    frames, _ = run_serve(plugin, [req("can_handle", {"path": "a.log", "name": "a.log",
                                                      "ext": "log", "size_bytes": 10,
                                                      "head_sample": "x"}, rid=1)])
    assert result_of(frames, 1) == {"can_handle": True, "confidence": 0.8}


def test_unknown_method_returns_neg_32601():
    frames, _ = run_serve(DummyPlugin(), [req("subscribe", {}, rid=1)])
    assert error_of(frames, 1)["code"] == -32601


def test_malformed_json_returns_neg_32600():
    frames, _ = run_serve(DummyPlugin(), ["this is not json"])
    assert error_of(frames, 1) is None or error_of(frames, 1)["code"] == -32600
    assert frames[0]["error"]["code"] == -32600


def test_missing_id_or_method_returns_neg_32600():
    frames, _ = run_serve(DummyPlugin(), ['{"jsonrpc":"2.0","method":"schema"}',
                                          '{"jsonrpc":"2.0","id":1}'])
    assert frames[0]["error"]["code"] == -32600
    assert frames[1]["error"]["code"] == -32600


def test_invalid_params_unloaded_file_neg_32602():
    frames, _ = run_serve(DummyPlugin(), [req("parse", {"file_id": FID}, rid=1),
                                          req("key_values", {"file_id": FID,
                                                             "timestamp_ms": 5}, rid=2)])
    assert error_of(frames, 1)["code"] == -32602
    assert error_of(frames, 2)["code"] == -32602


def test_invalid_params_missing_fields_neg_32602():
    frames, _ = run_serve(DummyPlugin(), [req("load_file", {}, rid=1),
                                          req("key_values", {"file_id": FID}, rid=2),
                                          req("can_handle", {"ext": "log"}, rid=3)])
    for rid in (1, 2, 3):
        assert error_of(frames, rid)["code"] == -32602


def test_shutdown_returns_empty_and_exits():
    plugin = DummyPlugin()
    frames, _ = run_serve(plugin, [req("shutdown", {}, rid=1)])
    assert result_of(frames, 1) == {}
    assert "on_shutdown" not in plugin.events  # SDK 自动应答，无需作者实现


def test_eof_returns_without_error_and_exit_code_zero():
    plugin = DummyPlugin()
    frames, _ = run_serve(plugin, [req("initialize", {}, rid=1)])
    assert result_of(frames, 1)["id"] == "dummy"
    # 子进程级断言：stdin 立即 EOF → 退出码 0（§9 约定 5）。
    sdk_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    src = (
        "from analysisbuddy import AnalysisBuddyPlugin\n"
        "class P(AnalysisBuddyPlugin):\n"
        "    id='t'; name='T'; version='1.0.0'\n"
        "P().serve()\n"
    )
    env = dict(os.environ, PYTHONPATH=sdk_dir)
    proc = subprocess.run([sys.executable, "-c", src], input=b"", capture_output=True,
                          env=env, timeout=30)
    assert proc.returncode == 0


# ----------------------------------------------------------------------
# parse 并发 / 取消 / 幂等
# ----------------------------------------------------------------------


def test_concurrent_parse_same_file_returns_neg_32001():
    plugin = DummyPlugin()
    plugin.parse_sleep = 0.05
    plugin.parse_emit = 5
    frames, _ = run_serve(plugin, [
        req("load_file", {"file_id": FID, "path": "a.csv"}, rid=1),
        req("parse", {"file_id": FID}, rid=2),
        req("parse", {"file_id": FID}, rid=3),
    ])
    assert error_of(frames, 3)["code"] == -32001
    assert result_of(frames, 2)["records_total"] == 5


def test_parse_of_different_file_while_busy_returns_neg_32001():
    plugin = DummyPlugin()
    plugin.parse_sleep = 0.05
    plugin.parse_emit = 5
    frames, _ = run_serve(plugin, [
        req("load_file", {"file_id": FID, "path": "a.csv"}, rid=1),
        req("load_file", {"file_id": FID + "x", "path": "b.csv"}, rid=2),
        req("parse", {"file_id": FID}, rid=3),
        req("parse", {"file_id": FID + "x"}, rid=4),
    ])
    assert error_of(frames, 4)["code"] == -32001
    assert result_of(frames, 3)["records_total"] == 5


def test_cancel_parse_returns_neg_32004():
    plugin = DummyPlugin()
    plugin.parse_sleep = 0.05
    plugin.parse_emit = 100  # ~5s，保证 cancel 落在 parse 执行期间
    frames, _ = run_serve(plugin, [
        req("load_file", {"file_id": FID, "path": "a.csv"}, rid=1),
        req("parse", {"file_id": FID}, rid=2),
        req("cancel_parse", {"file_id": FID}, rid=3),
    ])
    assert result_of(frames, 3) == {}
    err = error_of(frames, 2)
    assert err is not None
    assert err["code"] == -32004


def test_cancel_parse_idempotent_when_not_parsing():
    frames, _ = run_serve(DummyPlugin(), [req("cancel_parse", {"file_id": FID}, rid=1)])
    assert result_of(frames, 1) == {}


def test_reload_same_file_is_idempotent_unload_then_load():
    plugin = DummyPlugin()
    frames, _ = run_serve(plugin, [
        req("load_file", {"file_id": FID, "path": "a.csv"}, rid=1),
        req("load_file", {"file_id": FID, "path": "a.csv"}, rid=2),
        req("parse", {"file_id": FID}, rid=3),
    ])
    assert result_of(frames, 3)["records_total"] == 0
    assert plugin.events == [("load", FID), ("unload", FID), ("load", FID), ("parse", FID)]


def test_unload_unknown_file_id_is_success():
    frames, _ = run_serve(DummyPlugin(), [req("unload_file", {"file_id": FID}, rid=1)])
    assert result_of(frames, 1) == {}


def test_load_failure_maps_to_neg_32002():
    plugin = DummyPlugin()
    plugin.raise_in = "load"
    frames, _ = run_serve(plugin, [req("load_file", {"file_id": FID, "path": "missing.csv"},
                                       rid=1)])
    err = error_of(frames, 1)
    assert err["code"] == -32002
    assert err["data"] == {"path": "missing.csv"}
    # 失败后文件未登记为 loaded：随后 parse 回 -32602 而非启动。
    plugin2 = DummyPlugin()
    plugin2.raise_in = "load"
    frames2, _ = run_serve(plugin2, [
        req("load_file", {"file_id": FID, "path": "missing.csv"}, rid=1),
        req("parse", {"file_id": FID}, rid=2),
    ])
    assert error_of(frames2, 2)["code"] == -32602


# ----------------------------------------------------------------------
# 错误码映射（§1.5）
# ----------------------------------------------------------------------


def test_seven_exception_mappings():
    def raise_in_load(exc):
        class _P(DummyPlugin):
            def on_load_file(self, p):
                raise exc

        return _P()

    cases = [
        (raise_in_load(FileLoadFailedError("file load failed", data={"path": "a.csv"})), -32002),
        (raise_in_load(ParseFailedError("parse failed", data={"line": 7})), -32003),
        (raise_in_load(PluginBusyError()), -32001),
        (raise_in_load(UnsupportedInV1Error()), -32005),
        (raise_in_load(InvalidParamsError()), -32602),
        (raise_in_load(ValueError("boom")), -32603),
    ]

    for plugin, expected in cases:
        frames, stderr = run_serve(plugin, [req("load_file", {"file_id": FID,
                                                              "path": "a.csv"}, rid=1)])
        err = error_of(frames, 1)
        assert err is not None, "plugin={0}".format(type(plugin).__name__)
        assert err["code"] == expected, "plugin={0}".format(type(plugin).__name__)

    # ParseFailedError.data 进 error.data。
    p = raise_in_load(ParseFailedError("parse failed", data={"line": 7}))
    frames, _ = run_serve(p, [req("load_file", {"file_id": FID, "path": "a.csv"}, rid=1)])
    assert error_of(frames, 1)["data"] == {"line": 7}

    # 未知异常 → -32603 且 stderr 带 traceback。
    p = raise_in_load(ValueError("boom"))
    frames, stderr = run_serve(p, [req("load_file", {"file_id": FID, "path": "a.csv"}, rid=1)])
    assert "Traceback" in stderr


def test_error_code_hard_validation():
    with pytest.raises(SdkInternalError):
        build_error(-31999, "custom code forbidden")
    with pytest.raises(SdkInternalError):
        build_error(1, "positive also forbidden")
    ok = build_error(-32004, "cancelled", data={"file_id": "f"})
    assert ok == {"code": -32004, "message": "cancelled", "data": {"file_id": "f"}}
    assert build_error(-32601, "Method not found") == {"code": -32601, "message": "Method not found"}


def test_annotate_unimplemented_returns_neg_32005():
    frames, _ = run_serve(DummyPlugin(), [
        req("load_file", {"file_id": FID, "path": "a.csv"}, rid=1),
        req("annotate", {"file_id": FID, "range": {"start_ms": 0, "end_ms": 100}}, rid=2),
    ])
    assert error_of(frames, 2)["code"] == -32005


# ----------------------------------------------------------------------
# 流式输出：批量 / 心跳 / skip-if-empty 集成
# ----------------------------------------------------------------------


def test_parse_streams_batches_done_and_records_total():
    plugin = DummyPlugin()
    plugin.parse_emit = 5000
    frames, _ = run_serve(plugin, [
        req("load_file", {"file_id": FID, "path": "a.csv"}, rid=1),
        req("parse", {"file_id": FID}, rid=2),
    ])
    batches = [f["params"] for f in frames if f.get("method") == "RecordBatch"]
    assert [b["seq"] for b in batches] == [0, 1]
    assert all(b["file_id"] == FID for b in batches)
    assert batches[0]["done"] is False
    assert batches[1]["done"] is True
    assert sum(len(b["records"]) for b in batches) == 5000
    assert result_of(frames, 2)["records_total"] == 5000


def test_heartbeat_sent_during_silent_parse():
    class SlowPlugin(DummyPlugin):
        def on_parse(self, file_id, options, ctx):
            time.sleep(2.3)  # 静默 ≥2s，心跳守护应自动发 progress
            return 0

    frames, _ = run_serve(SlowPlugin(), [
        req("load_file", {"file_id": FID, "path": "a.csv"}, rid=1),
        req("parse", {"file_id": FID}, rid=2),
    ])
    progresses = [f["params"] for f in frames if f.get("method") == "progress"]
    # 首条为 parse 起始 progress，其后 ≥1 条为 2s 心跳。
    assert len(progresses) >= 2
    assert all(p["records_so_far"] == 0 for p in progresses)
    assert result_of(frames, 2)["records_total"] == 0


def test_parse_error_midway_maps_to_neg_32003():
    plugin = DummyPlugin()
    plugin.raise_in = "parse"
    frames, _ = run_serve(plugin, [
        req("load_file", {"file_id": FID, "path": "a.csv"}, rid=1),
        req("parse", {"file_id": FID}, rid=2),
    ])
    assert error_of(frames, 2)["code"] == -32003
    assert error_of(frames, 2)["data"] == {"line": 42}
