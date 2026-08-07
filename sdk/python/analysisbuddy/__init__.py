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
]
