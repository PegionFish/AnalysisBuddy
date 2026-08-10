# FRAME/STATE/EVENT 行解析与索引单测（sdk-plugins.md §4.1/§4.3/§4.4）。

import bisect
import os
import time

import pytest

from parser import (
    EventIndex,
    EventLine,
    FrameLine,
    StateIndex,
    StateLine,
    parse_line,
)


class TestParseLine:
    def test_frame_line(self):
        line = "2026-08-07T10:00:00.123+08:00 FRAME fps=60.1 frame_ms=16.6 cpu_temp=63.2"
        parsed = parse_line(line)
        assert isinstance(parsed, FrameLine)
        assert parsed.fps == 60.1
        assert parsed.frame_ms == 16.6
        assert parsed.cpu_temp == 63.2
        # 2026-08-07T10:00:00.123+08:00 = UTC 1786068000123
        assert parsed.ts_ms == 1786068000123

    def test_state_line(self):
        line = "2026-08-07T10:00:05.000+08:00 STATE scene=boss_fight hero_hp=100 stamina=80"
        parsed = parse_line(line)
        assert isinstance(parsed, StateLine)
        assert parsed.values == {"scene": "boss_fight", "hero_hp": 100, "stamina": 80}

    def test_event_line_with_quoted_reason(self):
        line = '2026-08-07T10:00:12.481+08:00 EVENT crash_dump reason="GPU hang" level=error'
        parsed = parse_line(line)
        assert isinstance(parsed, EventLine)
        assert parsed.label == "GPU hang"
        assert parsed.level == "error"
        assert parsed.local_time == "10:00:12"

    def test_event_level_defaults_to_info(self):
        line = "2026-08-07T10:00:12.481+08:00 EVENT checkpoint reached=1"
        parsed = parse_line(line)
        assert isinstance(parsed, EventLine)
        assert parsed.level == "info"

    def test_unknown_line_returns_none(self):
        assert parse_line("garbage line") is None
        assert parse_line("2026-08-07T10:00:00.000+08:00 UNKNOWN foo=1") is None

    def test_frame_missing_keys_returns_none(self):
        assert parse_line("2026-08-07T10:00:00.000+08:00 FRAME fps=60.1") is None

    def test_bad_timestamp_returns_none(self):
        assert parse_line("not-a-time FRAME fps=1 frame_ms=2 cpu_temp=3") is None


class TestStateIndex:
    def test_snapshot_merges_with_later_overriding(self):
        idx = StateIndex()
        idx.add(StateLine(1000, {"scene": "main_menu"}))
        idx.add(StateLine(2000, {"scene": "boss_fight", "hero_hp": 100}))
        idx.add(StateLine(3000, {"hero_hp": 73, "stamina": 41}))

        assert idx.snapshot_at(999) == {}
        assert idx.snapshot_at(1000) == {"scene": "main_menu"}
        assert idx.snapshot_at(2500) == {"scene": "boss_fight", "hero_hp": 100}
        assert idx.snapshot_at(3000) == {"scene": "boss_fight", "hero_hp": 73, "stamina": 41}
        assert idx.snapshot_at(10**12) == {"scene": "boss_fight", "hero_hp": 73, "stamina": 41}

    def test_bisect_locates_in_log_n(self):
        idx = StateIndex()
        for i in range(100000):
            idx.add(StateLine(i * 10, {"scene": f"s{i}", "n": i}))
        t0 = time.perf_counter()
        snapshot = idx.snapshot_at(999990)
        elapsed = time.perf_counter() - t0
        assert snapshot["scene"] == "s99999"
        assert elapsed < 1.0  # bisect 定位 + 快照合并，远低于 10s 超时

    def test_big_file_query_well_below_10s_timeout(self):
        # DoD：100MB 级合成数据下 key_values 响应远低于 10s 超时（protocol.md §6）。
        # 100MB 日志 ≈ 200 万行；STATE 行为稀疏子集（此处直接压测 50 万 STATE 行，
        # 其查询成本已显著高于真实 100MB 文件里的 STATE 稀疏场景）。
        idx = StateIndex()
        n = 500_000
        for i in range(n):
            idx.add(StateLine(i * 10, {"scene": f"s{i % 100}", "value": i}))
        t0 = time.perf_counter()
        snapshot = idx.snapshot_at(n * 10 - 5)
        elapsed = time.perf_counter() - t0
        assert snapshot["value"] == n - 1
        assert elapsed < 10.0  # 远低于 key_values 10s 看门狗


class TestEventIndex:
    def test_last_at(self):
        idx = EventIndex()
        idx.add(EventLine(1000, "a", "info", "00:00:01"))
        idx.add(EventLine(3000, "b", "warn", "00:00:03"))
        assert idx.last_at(999) is None
        assert idx.last_at(1000).label == "a"
        assert idx.last_at(2999).label == "a"
        assert idx.last_at(3000).label == "b"

    def test_in_range_closed_interval(self):
        idx = EventIndex()
        idx.add(EventLine(1000, "a", "info", "t"))
        idx.add(EventLine(2000, "b", "warn", "t"))
        idx.add(EventLine(3000, "c", "error", "t"))
        assert [e.label for e in idx.in_range(1500, 2500)] == ["b"]
        assert [e.label for e in idx.in_range(2000, 3000)] == ["b", "c"]
        assert idx.in_range(4000, 5000) == []
