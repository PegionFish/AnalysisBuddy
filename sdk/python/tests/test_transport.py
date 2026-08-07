"""帧层单测（protocol-v1.md §1.2/§1.3）：UTF-8 无 BOM、LF、8MB 先行校验、孤立 \\r 拒绝。"""

import io

import pytest

from analysisbuddy.transport import MAX_LINE_BYTES, NdjsonReader, ProtocolError


def reader_of(data: bytes) -> NdjsonReader:
    return NdjsonReader(io.BytesIO(data))


def test_reads_lines_split_across_chunks():
    r = reader_of(b'{"a":1}\n{"b":2}\n')
    assert r.read_message() == '{"a":1}'
    assert r.read_message() == '{"b":2}'
    assert r.read_message() is None


def test_line_boundary_inside_read_chunk():
    chunk = b"x" * 1024 * 64  # 恰为一块整
    data = chunk + b"\n" + chunk + b"\n"
    r = reader_of(data)
    first = r.read_message()
    assert first == "x" * (1024 * 64)
    second = r.read_message()
    assert second == "x" * (1024 * 64)
    assert r.read_message() is None


def test_eof_without_trailing_newline():
    r = reader_of(b'{"a":1}')
    assert r.read_message() == '{"a":1}'
    assert r.read_message() is None


def test_empty_input_eof():
    r = reader_of(b"")
    assert r.read_message() is None


def test_crlf_line_ending_rejected():
    r = reader_of(b'{"a":1}\r\n')
    with pytest.raises(ProtocolError):
        r.read_message()


def test_stray_cr_before_lf_rejected():
    r = reader_of(b'{"a":1}\r')
    with pytest.raises(ProtocolError):
        r.read_message()


def test_oversized_line_rejected_before_content_read():
    # 超 8MB 单行：长度先于内容校验，须抛 ProtocolError 而非返回整行。
    oversized = b"y" * (MAX_LINE_BYTES + 1)
    r = reader_of(oversized + b"\n")
    with pytest.raises(ProtocolError):
        r.read_message()


def test_line_exactly_at_limit_is_accepted():
    exact = b"z" * MAX_LINE_BYTES
    r = reader_of(exact + b"\n")
    assert r.read_message() == "z" * MAX_LINE_BYTES


def test_invalid_utf8_rejected():
    r = reader_of(b"\xff\xfe\x00\x01\n")
    with pytest.raises(ProtocolError):
        r.read_message()


def test_utf8_multibyte_roundtrip():
    r = reader_of('{"备注":"中文"}\n'.encode("utf-8"))
    assert r.read_message() == '{"备注":"中文"}'
