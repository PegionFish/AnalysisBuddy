"""EmitContext —— parse 流式回传（sdk-plugins.md §1.4 / protocol-v1.md §3.1~§3.3）。

- 批量：`batch_size` 默认 4000、合法 1000~8000（构造校验，越界 ValueError）；
  单批序列化体积接近 1MB 提前 flush；`seq` 从 0 单调自增；
- 心跳：解析期间守护线程每 2s 检查一次，距上次发送（RecordBatch/progress）≥2s
  自动发一条 `progress`（`records_so_far` 保持累计值）；
- 序列化：可选字段（level/tags/raw_line）为空即省略键，禁止 null/空容器；
  `value` 为 NaN/±inf 时丢弃该记录并在 stderr 计数告警；
- 收尾：`finish()` flush 残余 + 发 `done:true` 末批（可为空数组）。
"""

from __future__ import annotations

import json
import math
import sys
import threading
import time
from typing import Callable, List, Optional

from .errors import CancelledError

DEFAULT_BATCH_SIZE = 4000
MIN_BATCH_SIZE = 1000
MAX_BATCH_SIZE = 8000
# 单批序列化体积接近 1MB（§1.3 建议上限）提前 flush 的阈值。
EARLY_FLUSH_BYTES = 900_000
_DEFAULT_HEARTBEAT_INTERVAL = 2.0


class EmitContext:
    def __init__(
        self,
        file_id: str,
        sender: Callable[[str, dict], None],
        batch_size: int = DEFAULT_BATCH_SIZE,
        heartbeat_interval: float = _DEFAULT_HEARTBEAT_INTERVAL,
        stderr=None,
    ) -> None:
        if isinstance(batch_size, bool) or not isinstance(batch_size, int):
            raise ValueError("batch_size must be an integer")
        if not (MIN_BATCH_SIZE <= batch_size <= MAX_BATCH_SIZE):
            raise ValueError(
                "batch_size must be in [{0}, {1}], got {2!r}".format(
                    MIN_BATCH_SIZE, MAX_BATCH_SIZE, batch_size
                )
            )
        self._file_id = file_id
        self._sender = sender
        self._batch_size = batch_size
        self._heartbeat_interval = float(heartbeat_interval)
        self._stderr = stderr if stderr is not None else sys.stderr
        self._lock = threading.Lock()
        self._buffer: List[dict] = []
        self._buffer_bytes = 0
        self._seq = 0
        self._records_so_far = 0
        self._dropped = 0
        self._cancel_event = threading.Event()
        self._active = False
        self._last_send = 0.0
        self._hb_stop = threading.Event()
        self._hb_thread: Optional[threading.Thread] = None

    # ------------------------------------------------------------------
    # serve() 生命周期钩子
    # ------------------------------------------------------------------

    def start(self) -> None:
        """parse 开始：置活跃标志、发首条 progress（records_so_far=0）、起心跳守护。"""
        with self._lock:
            if self._active:
                return
            self._active = True
            self._last_send = time.monotonic()
        self._send("progress", {"file_id": self._file_id, "records_so_far": 0})
        self._hb_thread = threading.Thread(
            target=self._heartbeat_loop, name="ab-heartbeat", daemon=True
        )
        self._hb_thread.start()

    def stop(self) -> None:
        """停止心跳（异常路径：取消/解析失败时不发 done 批）。"""
        self._hb_stop.set()
        with self._lock:
            self._active = False
        thread = self._hb_thread
        if thread is not None and thread.is_alive():
            thread.join(timeout=1.0)

    def finish(self) -> None:
        """parse 正常返回：停心跳 → flush 残余 → 发 done:true 末批。"""
        self.stop()
        self._flush(force_done=True)

    def cancel(self) -> None:
        """宿主 cancel_parse：置取消标志，作者循环内 check_cancelled() 抛错。"""
        self._cancel_event.set()

    # ------------------------------------------------------------------
    # 作者 API
    # ------------------------------------------------------------------

    @property
    def records_so_far(self) -> int:
        with self._lock:
            return self._records_so_far

    def emit_records(self, records: List[dict]) -> None:
        """批量入缓冲；满 batch_size 或接近 1MB 即 flush 成一条 RecordBatch。"""
        if not records:
            return
        for record in records:
            cleaned = self._sanitize(record)
            if cleaned is None:
                continue
            size = len(json.dumps(cleaned, ensure_ascii=False, separators=(",", ":")))
            with self._lock:
                self._buffer.append(cleaned)
                self._buffer_bytes += size
                self._records_so_far += 1
            if len(self._buffer) >= self._batch_size or self._buffer_bytes >= EARLY_FLUSH_BYTES:
                self._flush()

    def progress(self, percent: Optional[float] = None, bytes_read: Optional[int] = None) -> None:
        """作者主动进度上报；records_so_far 恒为当前累计值。"""
        if percent is not None:
            if isinstance(percent, bool) or not isinstance(percent, (int, float)):
                raise ValueError("percent must be a number")
            percent = float(percent)
            if not (0.0 <= percent <= 100.0):
                raise ValueError("percent must be in [0, 100]")
        if bytes_read is not None:
            if isinstance(bytes_read, bool) or not isinstance(bytes_read, int):
                raise ValueError("bytes_read must be an integer")
        params: dict = {"file_id": self._file_id, "records_so_far": self._records_so_far}
        if percent is not None:
            params["percent"] = percent
        if bytes_read is not None:
            params["bytes_read"] = bytes_read
        self._send("progress", params)

    def check_cancelled(self) -> None:
        """周期调用；宿主已取消则抛 CancelledError（→ -32004）。"""
        if self._cancel_event.is_set():
            raise CancelledError("parse cancelled by host")

    # ------------------------------------------------------------------
    # 内部
    # ------------------------------------------------------------------

    def _sanitize(self, record: dict) -> Optional[dict]:
        """校验 + 清洗单条 Record（§3.1）：

        - 三必填字段：timestamp 整数 / metric 非空串 / value 有限数；
        - 可选字段（level/tags/raw_line）为空即省略键；
        - value 为 NaN/±inf：丢弃该记录，stderr 计数告警。
        """
        if not isinstance(record, dict):
            raise ValueError("Record must be a dict, got {0!r}".format(type(record).__name__))
        timestamp = record.get("timestamp")
        metric = record.get("metric")
        value = record.get("value")
        if isinstance(timestamp, bool) or not isinstance(timestamp, int):
            raise ValueError("Record.timestamp must be an integer")
        if not isinstance(metric, str) or not metric:
            raise ValueError("Record.metric must be a non-empty string")
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise ValueError("Record.value must be a number (int/float)")
        if isinstance(value, float) and not math.isfinite(value):
            self._dropped += 1
            self._log(
                "WARN",
                "record dropped: non-finite value {0!r} (timestamp={1!r}, metric={2!r}); "
                "dropped so far: {3}".format(value, timestamp, metric, self._dropped),
            )
            return None
        cleaned = {"timestamp": timestamp, "metric": metric, "value": value}
        level = record.get("level")
        if isinstance(level, str) and level:
            cleaned["level"] = level
        tags = record.get("tags")
        if isinstance(tags, dict) and tags:
            cleaned["tags"] = {str(k): str(v) for k, v in tags.items()}
        raw_line = record.get("raw_line")
        if isinstance(raw_line, str) and raw_line:
            cleaned["raw_line"] = raw_line
        return cleaned

    def _flush(self, force_done: bool = False) -> None:
        with self._lock:
            records = self._buffer
            if not force_done and not records:
                return
            seq = self._seq
            self._buffer = []
            self._buffer_bytes = 0
            self._seq += 1
            params = {
                "file_id": self._file_id,
                "seq": seq,
                "records": records,
                "done": force_done,
            }
        self._send("RecordBatch", params)

    def _send(self, method: str, params: dict) -> None:
        self._sender(method, params)
        with self._lock:
            self._last_send = time.monotonic()

    def _heartbeat_loop(self) -> None:
        while not self._hb_stop.wait(0.2):
            now = time.monotonic()
            with self._lock:
                if not self._active:
                    return
                if now - self._last_send < self._heartbeat_interval:
                    continue
                params = {"file_id": self._file_id, "records_so_far": self._records_so_far}
            self._send("progress", params)

    def _log(self, level: str, msg: str) -> None:
        self._stderr.write("{0}|analysisbuddy-sdk|{1}\n".format(level, msg))
        self._stderr.flush()
