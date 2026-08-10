"""EmitContext 单测（sdk-plugins.md §1.4）：批量、seq、1MB 提前 flush、心跳、skip-if-empty。"""

import io
import json
import math
import time

import pytest

from analysisbuddy.context import (
    EARLY_FLUSH_BYTES,
    EmitContext,
    MAX_BATCH_SIZE,
    MIN_BATCH_SIZE,
)
from analysisbuddy.errors import CancelledError


def make_record(ts=1000, metric="fps", value=60.0, **extra):
    rec = {"timestamp": ts, "metric": metric, "value": value}
    rec.update(extra)
    return rec


class Recorder:
    """记录所有通知的假 sender。"""

    def __init__(self):
        self.notifications = []
        self.last_send = time.monotonic()

    def __call__(self, method, params):
        self.notifications.append((method, params))
        self.last_send = time.monotonic()


def make_ctx(batch_size=4000, sender=None, **kw):
    rec = sender if sender is not None else Recorder()
    ctx = EmitContext("f1", rec, batch_size=batch_size, stderr=io.StringIO(), **kw)
    return ctx, rec


def batches(rec):
    return [p for m, p in rec.notifications if m == "RecordBatch"]


def test_batch_size_out_of_range_raises_valueerror():
    with pytest.raises(ValueError):
        EmitContext("f1", Recorder(), batch_size=MIN_BATCH_SIZE - 1)
    with pytest.raises(ValueError):
        EmitContext("f1", Recorder(), batch_size=MAX_BATCH_SIZE + 1)
    with pytest.raises(ValueError):
        EmitContext("f1", Recorder(), batch_size="4000")


def test_default_batch_size_is_4000():
    ctx, _ = make_ctx()
    assert ctx._batch_size == 4000


def test_flush_at_batch_size_with_seq_from_zero():
    ctx, rec = make_ctx(batch_size=4000)
    ctx.emit_records([make_record(ts=i) for i in range(4000)])
    out = batches(rec)
    assert len(out) == 1
    assert out[0]["seq"] == 0
    assert out[0]["done"] is False
    assert len(out[0]["records"]) == 4000
    ctx.finish()
    assert batches(rec)[-1]["done"] is True
    assert batches(rec)[-1]["records"] == []


def test_seq_monotonic_no_gaps_and_total_count():
    ctx, rec = make_ctx(batch_size=4000)
    ctx.emit_records([make_record(ts=i) for i in range(8001)])
    ctx.finish()
    out = batches(rec)
    seqs = [b["seq"] for b in out]
    assert seqs == [0, 1, 2]
    assert sum(len(b["records"]) for b in out) == 8001
    assert out[2]["done"] is True
    assert len(out[2]["records"]) == 1
    assert ctx.records_so_far == 8001


def test_early_flush_near_one_megabyte():
    # 单条含 ~20KB raw_line：45 条即超阈值 → 远未到 4000 就提前 flush。
    ctx, rec = make_ctx(batch_size=4000)
    big_raw = "R" * (20 * 1024)
    ctx.emit_records([make_record(ts=i, raw_line=big_raw) for i in range(45)])
    out = batches(rec)
    assert len(out) >= 1
    assert len(out[0]["records"]) < 4000
    assert sum(len(json.dumps(r, ensure_ascii=False, separators=(",", ":")))
               for r in out[0]["records"]) >= EARLY_FLUSH_BYTES * 0.9


def test_skip_if_empty_optional_fields():
    ctx, rec = make_ctx()
    ctx.emit_records(
        [
            make_record(ts=1, level=None, tags={}, raw_line=""),
            make_record(ts=2, level="", tags={}, raw_line=""),
            make_record(ts=3, level="info", tags={"scene": "boss"}, raw_line="raw"),
        ]
    )
    ctx.finish()
    records = batches(rec)[0]["records"]
    for r in records[:2]:
        assert "level" not in r and "tags" not in r and "raw_line" not in r
    r = records[2]
    assert r["level"] == "info"
    assert r["tags"] == {"scene": "boss"}
    assert r["raw_line"] == "raw"


def test_non_finite_values_dropped_and_counted():
    ctx, rec = make_ctx()
    err = io.StringIO()
    ctx._stderr = err
    ctx.emit_records(
        [
            make_record(ts=1, value=float("nan")),
            make_record(ts=2, value=float("inf")),
            make_record(ts=3, value=float("-inf")),
            make_record(ts=4, value=1.5),
        ]
    )
    ctx.finish()
    records = batches(rec)[0]["records"]
    assert len(records) == 1
    assert records[0]["value"] == 1.5
    assert "non-finite" in err.getvalue()
    assert ctx._dropped == 3


def test_required_field_missing_raises():
    ctx, _ = make_ctx()
    with pytest.raises(ValueError):
        ctx.emit_records([{"timestamp": 1, "value": 1.0}])  # 缺 metric
    with pytest.raises(ValueError):
        ctx.emit_records([{"timestamp": "1", "metric": "fps", "value": 1.0}])
    with pytest.raises(ValueError):
        ctx.emit_records([{"timestamp": 1, "metric": "fps", "value": "1.0"}])


def test_progress_carries_records_so_far_and_validates_percent():
    ctx, rec = make_ctx()
    ctx.emit_records([make_record(ts=i) for i in range(10)])
    ctx.progress(percent=50.0, bytes_read=1024)
    ctx.progress(percent=100)
    ctx.progress()
    progresses = [p for m, p in rec.notifications if m == "progress"]
    assert progresses[0] == {"file_id": "f1", "records_so_far": 10, "percent": 50.0,
                             "bytes_read": 1024}
    assert progresses[1]["records_so_far"] == 10
    assert "percent" not in progresses[2]
    with pytest.raises(ValueError):
        ctx.progress(percent=150.0)


def test_check_cancelled_raises_after_cancel():
    ctx, _ = make_ctx()
    ctx.check_cancelled()  # 未取消不抛
    ctx.cancel()
    with pytest.raises(CancelledError):
        ctx.check_cancelled()


def test_finish_without_records_sends_empty_done_batch():
    ctx, rec = make_ctx()
    ctx.finish()
    out = batches(rec)
    assert len(out) == 1
    assert out[0]["done"] is True
    assert out[0]["records"] == []
    assert out[0]["seq"] == 0


def test_start_sends_initial_progress_with_zero():
    ctx, rec = make_ctx()
    ctx.start()
    ctx.stop()
    progresses = [p for m, p in rec.notifications if m == "progress"]
    assert progresses[0] == {"file_id": "f1", "records_so_far": 0}


def test_heartbeat_interval_silence_sends_progress():
    # 构造时用短心跳间隔（默认 2s 契约，测试用 0.1s 加速）；静默 ≥间隔自动发 progress，
    # records_so_far 保持累计值（§3.3「≤2s 一条」）。
    ctx, rec = make_ctx(heartbeat_interval=0.1)
    ctx.start()
    time.sleep(0.35)
    ctx.emit_records([make_record(ts=i) for i in range(5)])
    time.sleep(0.25)
    ctx.stop()
    progresses = [p for m, p in rec.notifications if m == "progress"]
    assert len(progresses) >= 3
    # 心跳期间 records_so_far 未变化（静默期）或等于累计值，绝不回退。
    counters = [p["records_so_far"] for p in progresses]
    assert counters == sorted(counters)
