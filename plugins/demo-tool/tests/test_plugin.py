# demo-tool 插件行为单测（DoD：schema 3 指标、FRAME→3 Record + tags.scene、
# key_values 覆盖语义与 last_event、annotate 两分支、plugin.json 逐字段一致）。
# EmitContext 按正式 SDK 契约构造（file_id + sender），收集通知断言。

import json
import os

import pytest

from analysisbuddy import EmitContext

FIXTURE = os.path.join(os.path.dirname(__file__), "fixtures", "small_txt.log")

FRAME_LINE = "2026-08-07T10:00:00.123+08:00 FRAME fps=60.1 frame_ms=16.6 cpu_temp=63.2\n"
STATE_MAIN = "2026-08-07T10:00:00.250+08:00 STATE scene=main_menu\n"
STATE_BOSS = "2026-08-07T10:00:05.000+08:00 STATE scene=boss_fight hero_hp=100 stamina=80\n"
EVENT_LINE = '2026-08-07T10:00:12.481+08:00 EVENT crash_dump reason="GPU hang" level=error\n'
UNKNOWN_LINE = "this is not a recognized line\n"


@pytest.fixture
def tmp_log(tmp_path):
    path = tmp_path / "input.log"
    # 先 STATE 后 FRAME：保证首条 FRAME 有 scene 标签
    path.write_text(
        STATE_MAIN + FRAME_LINE + FRAME_LINE.replace("10:00:00.123", "10:00:01.000")
        + STATE_BOSS + FRAME_LINE.replace("10:00:00.123", "10:00:05.500")
        + EVENT_LINE + UNKNOWN_LINE,
        encoding="utf-8",
    )
    return str(path)


@pytest.fixture
def emitted():
    """收集 SDK EmitContext 发往宿主的所有通知（RecordBatch/progress）。"""
    notifications = []

    def sender(method: str, params: dict):
        notifications.append((method, params))

    ctx = EmitContext("f1", sender)
    return notifications, ctx


def _finish(emitted):
    """宿主行为：on_parse 返回后 SDK flush 残余 + done:true 末批。"""
    emitted[1].finish()


def _records(emitted):
    """从 RecordBatch 通知中收集全部 Record。"""
    records = []
    for method, params in emitted[0]:
        if method == "RecordBatch":
            records.extend(params["records"])
    return records


def _load(plugin, path, file_id="f1"):
    return plugin.on_load_file({"file_id": file_id, "path": path})


class TestSchema:
    def test_schema_exactly_three_metrics(self, plugin):
        metrics = plugin.on_schema()["metrics"]
        assert len(metrics) == 3
        by_id = {m["id"]: m for m in metrics}
        assert by_id["fps"] == {"id": "fps", "name": "帧率", "unit": "fps", "aggregation": "last"}
        assert by_id["frame_time"] == {"id": "frame_time", "name": "帧耗时", "unit": "ms", "aggregation": "avg"}
        assert by_id["cpu_temp"] == {"id": "cpu_temp", "name": "CPU 温度", "unit": "°C", "aggregation": "max"}


class TestParse:
    def test_frame_produces_three_records_with_scene_tag(self, plugin, tmp_log, emitted):
        _load(plugin, tmp_log)
        total = plugin.on_parse("f1", None, emitted[1])
        _finish(emitted)
        records = _records(emitted)
        assert total == 9  # 3 FRAME × 3 records
        assert len(records) == 9

        # 第一条 FRAME 在 scene=main_menu 之后 → scene=main_menu
        first = records[0]
        assert first["metric"] == "fps" and first["value"] == 60.1
        assert first["tags"] == {"scene": "main_menu"}

        # 第三组 FRAME 在 scene=boss_fight 之后 → scene=boss_fight
        last = records[-1]
        assert last["metric"] == "cpu_temp" and last["value"] == 63.2
        assert last["tags"] == {"scene": "boss_fight"}

    def test_unknown_lines_skipped_without_interruption(self, plugin, tmp_log, emitted):
        summary = _load(plugin, tmp_log)
        assert "1 unknown lines" in summary["note"]
        total = plugin.on_parse("f1", None, emitted[1])
        _finish(emitted)
        assert total == 9
        assert len(_records(emitted)) == 9

    def test_parse_emits_progress_heartbeats(self, plugin, tmp_log, emitted):
        _load(plugin, tmp_log)
        plugin.on_parse("f1", None, emitted[1])
        methods = [m for m, _ in emitted[0]]
        assert "progress" in methods


class TestKeyValues:
    def test_snapshot_override_semantics_and_last_event(self, plugin, tmp_log):
        _load(plugin, tmp_log)
        # T 在 scene=boss_fight 之后、EVENT 之前：快照含 hero_hp=100，无 last_event
        entries = plugin.on_key_values("f1", 1786068006500)["entries"]
        by_key = {e["key"]: e for e in entries}
        assert by_key["scene"]["value"] == "boss_fight"
        assert by_key["hero_hp"]["value"] == 100
        assert by_key["hero_hp"]["unit"] == "%"
        assert by_key["stamina"]["value"] == 80
        assert "last_event" not in by_key  # 无 ≤T 事件时省略该 entry

        # T 在 EVENT 之后：附 last_event
        entries = plugin.on_key_values("f1", 1786068012481)["entries"]
        by_key = {e["key"]: e for e in entries}
        assert by_key["last_event"]["value"] == "GPU hang @ 10:00:12"

    def test_key_values_before_any_state_is_empty(self, plugin, tmp_log):
        _load(plugin, tmp_log)
        assert plugin.on_key_values("f1", 0) == {"entries": []}

    def test_key_values_unknown_file_id_returns_empty(self, plugin):
        assert plugin.on_key_values("nope", 123) == {"entries": []}


class TestAnnotate:
    def test_capability_annotate_true(self, plugin):
        caps = plugin.on_initialize({"protocol_version": 1})["capabilities"]
        assert caps["annotate"] is True

    def test_range_with_events(self, plugin, tmp_log):
        _load(plugin, tmp_log)
        events = plugin.on_annotate("f1", {"start_ms": 1786068000000, "end_ms": 1786068012481})["events"]
        assert len(events) == 1
        assert events[0]["label"] == "GPU hang"
        assert events[0]["level"] == "error"
        assert events[0]["timestamp_ms"] == 1786068012481

    def test_range_without_events_returns_empty(self, plugin, tmp_log):
        _load(plugin, tmp_log)
        assert plugin.on_annotate("f1", {"start_ms": 0, "end_ms": 1000}) == {"events": []}


class TestManifest:
    def test_plugin_json_matches_section_45(self):
        root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        with open(os.path.join(root, "plugin.json"), encoding="utf-8") as f:
            manifest = json.load(f)
        assert manifest == {
            "id": "demo-tool",
            "display_name": "演示工具解析器",
            "version": "0.1.0",
            "entry": {"command": "python", "args": ["main.py"]},
            "match": {
                "extensions": ["log", "txt"],
                "header_fingerprints": ["FRAME fps=", "STATE scene="],
            },
            "min_protocol_version": 1,
        }


class TestEndToEnd:
    def test_fixture_record_count_equals_frame_rows(self, plugin, emitted):
        assert os.path.exists(FIXTURE), f"missing fixture: {FIXTURE}"
        with open(FIXTURE, encoding="utf-8") as f:
            frame_rows = sum(1 for line in f if " FRAME " in line)
        summary = plugin.on_load_file({"file_id": "fx", "path": FIXTURE})
        total = plugin.on_parse("fx", None, emitted[1])
        _finish(emitted)
        records = _records(emitted)
        assert summary["record_count_hint"] == frame_rows
        assert total == frame_rows * 3
        assert len(records) == frame_rows * 3
