"""异常 → JSON-RPC 错误码映射（sdk-plugins.md §1.5 / protocol-v1.md §4）。

序列化错误对象时硬校验 code ∈ 标准集 ∪ {-32001…-32005}，越界即 SDK 内部错误。
"""

from __future__ import annotations

from typing import Any, Optional

# 允许的错误码全集：5 个 JSON-RPC 标准码 + 5 个协议自定义码（protocol-v1.md §4.1/§4.2）。
ALLOWED_ERROR_CODES = frozenset(
    {
        -32700,  # Parse error
        -32600,  # Invalid Request
        -32601,  # Method not found
        -32602,  # Invalid params
        -32603,  # Internal error
        -32001,  # plugin_busy
        -32002,  # file_load_failed
        -32003,  # parse_failed
        -32004,  # cancelled
        -32005,  # unsupported_in_v1
    }
)


class SdkInternalError(RuntimeError):
    """SDK 自身错误（如错误码越界硬校验失败），不属于协议层异常。"""


class AnalysisBuddyError(Exception):
    """协议层异常基类；code 即 JSON-RPC 错误码（§4）。"""

    code: int = -32603
    message: str = "Internal error"

    def __init__(self, message: str, data: Any = None) -> None:
        super().__init__(message)
        self.message = message
        self.data = data


class PluginBusyError(AnalysisBuddyError):
    code = -32001
    message = "plugin busy"

    def __init__(self, message: str = "plugin busy", data: Any = None) -> None:
        super().__init__(message, data)


class FileLoadFailedError(AnalysisBuddyError):
    code = -32002
    message = "file load failed"

    def __init__(self, message: str = "file load failed", data: Any = None) -> None:
        super().__init__(message, data)


class ParseFailedError(AnalysisBuddyError):
    code = -32003
    message = "parse failed"

    def __init__(self, message: str = "parse failed", data: Any = None) -> None:
        super().__init__(message, data)


class CancelledError(AnalysisBuddyError):
    code = -32004
    message = "cancelled"

    def __init__(self, message: str = "parse cancelled by host", data: Any = None) -> None:
        super().__init__(message, data)


class UnsupportedInV1Error(AnalysisBuddyError):
    code = -32005
    message = "unsupported in v1"

    def __init__(self, message: str = "unsupported in v1", data: Any = None) -> None:
        super().__init__(message, data)


class InvalidParamsError(AnalysisBuddyError):
    code = -32602
    message = "Invalid params"

    def __init__(self, message: str = "Invalid params", data: Any = None) -> None:
        super().__init__(message, data)


def build_error(code: int, message: str, data: Any = None) -> dict:
    """构造错误对象 ``{"code", "message", "data"?}``。

    硬校验 code 集合（sdk-plugins.md §1.5）：越界抛 :class:`SdkInternalError`，
    由序列化方转为 -32603 兜底。``data`` 为 None 时省略键（skip-if-empty）。
    """
    if code not in ALLOWED_ERROR_CODES:
        raise SdkInternalError(
            "error code {0} is outside the allowed set "
            "(standard set + -32001..-32005)".format(code)
        )
    if not isinstance(message, str) or not message:
        raise SdkInternalError("error message must be a non-empty string")
    error: dict = {"code": code, "message": message}
    if data is not None:
        error["data"] = data
    return error


def map_exception_code(exc: BaseException) -> int:
    """异常 → 错误码（§1.5 表）；未知异常兜底 -32603。"""
    if isinstance(exc, AnalysisBuddyError):
        return exc.code
    return -32603
