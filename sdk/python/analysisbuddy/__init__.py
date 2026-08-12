"""analysisbuddy-sdk —— AnalysisBuddy 插件 Python SDK（协议 v1）。

零第三方依赖（纯 stdlib，Python 3.10~3.14）。用法：

    1. 继承 :class:`AnalysisBuddyPlugin`，覆写 ``on_*`` handler；
    2. 入口 ``MyPlugin().serve()`` 即可跑起 NDJSON 主循环。

设计正本：AnalysisBuddy-devdocs/deep-dive/sdk-plugins.md §1；
协议：docs/spec/protocol-v1.md。
"""

from .context import EmitContext
from .errors import (
    AnalysisBuddyError,
    CancelledError,
    FileLoadFailedError,
    InvalidParamsError,
    ParseFailedError,
    PluginBusyError,
    SdkInternalError,
    UnsupportedInV1Error,
)
from .plugin import AnalysisBuddyPlugin

__version__ = "0.1.0"

# 协议版本常量（契约 C7）：单源 core/ab-protocol/src/lib.rs 的 PROTOCOL_VERSION（= 1）
# 与 docs/spec/plugin-manifest.schema.json 的 minimum: 1。SDK 零第三方依赖、不可
# 引用 ab-protocol crate，故在此固化，由 tests/test_protocol_version.py 断言防漂移。
PROTOCOL_VERSION = 1  # 本 SDK 实现/宿主支持的协议版本（max，对齐 ab-protocol）
MIN_PROTOCOL_VERSION = 1  # manifest min_protocol_version 最小允许值（schema minimum）

__all__ = [
    "AnalysisBuddyPlugin",
    "EmitContext",
    "AnalysisBuddyError",
    "PluginBusyError",
    "FileLoadFailedError",
    "ParseFailedError",
    "CancelledError",
    "UnsupportedInV1Error",
    "InvalidParamsError",
    "SdkInternalError",
    "PROTOCOL_VERSION",
    "MIN_PROTOCOL_VERSION",
]
