"""NDJSON 帧层（protocol-v1.md §1.2/§1.3）。

- 编码：UTF-8 无 BOM，行尾仅 ``\\n``；帧尾出现 ``\\r``（即 CRLF）视为协议错；
- 单行上限 8MB（8 × 1024 × 1024），**长度先于内容校验**：增量读字节、边读边累计
  长度，超限即中止，避免驻留超过 8MB 内存；
- stdout 写：整行原子写出（单次 write + flush），杜绝半行 JSON。
"""

from __future__ import annotations

import json
import sys
from typing import BinaryIO, Optional

MAX_LINE_BYTES = 8 * 1024 * 1024  # 8,388,608（§1.3）
_READ_CHUNK = 64 * 1024


class ProtocolError(Exception):
    """帧层协议错误：超 8MB / 孤立 CR / 非法 UTF-8。宿主判定协议错后会 kill 会话。"""


class NdjsonReader:
    """stdin 逐行读取器：按字节增量读，长度先于内容校验（§1.3）。"""

    def __init__(self, stream: BinaryIO) -> None:
        self._stream = stream
        self._buffer = b""

    def read_message(self) -> Optional[str]:
        """读下一帧；EOF 返回 None；帧层违规抛 :class:`ProtocolError`。"""
        while True:
            nl = self._buffer.find(b"\n")
            if nl >= 0:
                # 长度先于内容：已找到帧尾，但帧长超限 → 直接中止，不驻留整行。
                if nl > MAX_LINE_BYTES:
                    raise ProtocolError("stdin line exceeds 8MB limit")
                line, self._buffer = self._buffer[:nl], self._buffer[nl + 1 :]
                return self._finalize(line)
            if len(self._buffer) > MAX_LINE_BYTES:
                raise ProtocolError("stdin line exceeds 8MB limit")
            chunk = self._stream.read(_READ_CHUNK)
            if not chunk:
                if not self._buffer:
                    return None
                if len(self._buffer) > MAX_LINE_BYTES:
                    raise ProtocolError("stdin line exceeds 8MB limit")
                line, self._buffer = self._buffer, b""
                return self._finalize(line)
            # 长度先于内容：本 chunk 不含换行且叠加后超限 → 直接中止，不驻留整行。
            if b"\n" not in chunk and len(self._buffer) + len(chunk) > MAX_LINE_BYTES:
                raise ProtocolError("stdin line exceeds 8MB limit")
            self._buffer += chunk

    def _finalize(self, line: bytes) -> str:
        if line.endswith(b"\r"):
            raise ProtocolError("stray CR at frame boundary (CRLF line ending rejected)")
        try:
            return line.decode("utf-8")
        except UnicodeDecodeError:
            raise ProtocolError("invalid UTF-8 on stdin")


class NdjsonWriter:
    """stdout 帧写入器：UTF-8 无 BOM、LF 行尾、整行一次 write + flush。

    调用方需保证并发写互斥（serve() 的发送锁）；本类不做内部加锁。
    """

    def __init__(self, stream: BinaryIO) -> None:
        self._stream = stream

    def write_response(self, req_id: int, result: dict) -> None:
        self._write({"jsonrpc": "2.0", "id": req_id, "result": result})

    def write_error(self, req_id: Optional[int], error: dict) -> None:
        self._write({"jsonrpc": "2.0", "id": req_id, "error": error})

    def write_notification(self, method: str, params: dict) -> None:
        self._write({"jsonrpc": "2.0", "method": method, "params": params})

    def flush(self) -> None:
        self._stream.flush()

    def _write(self, frame: dict) -> None:
        line = json.dumps(frame, ensure_ascii=False, separators=(",", ":"))
        self._stream.write(line.encode("utf-8") + b"\n")
        self._stream.flush()


def default_stdin() -> BinaryIO:
    return sys.stdin.buffer


def default_stdout() -> BinaryIO:
    return sys.stdout.buffer
