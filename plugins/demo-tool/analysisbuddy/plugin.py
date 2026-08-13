"""AnalysisBuddyPlugin 与 serve() 主循环（sdk-plugins.md §1.2/§1.3）。

serve() 九条行为契约（对齐 protocol-v1.md §1/§9）：

1. stdin 逐行读：长度先于内容校验，超 8MB / 帧尾孤立 CR → stderr 日志后退出；
2. stdout 整行原子写出（缓冲后单次 write + flush），杜绝半行 JSON；
3. stderr 留日志：plugin.log(level, msg) → ``LEVEL|plugin_id|msg``；
4. stdin EOF → flush → 退出码 0（禁止孤儿进程）；
5. 1 读线程 + parse 专用线程 + 发送锁：parse 期间其余请求仍即时应答；
6. 10 method 路由：未知 -32601、结构非法 -32600、参数非法 -32602；
7. shutdown 自动回 {} 后退出；
8. cancel_parse → check_cancelled() 抛 CancelledError → 被取消的 parse 回 -32004；
9. 同 file_id 重新 load_file：先自动 on_unload_file 再 on_load_file（幂等重入）。
"""

from __future__ import annotations

import json
import sys
import threading
import traceback
from typing import BinaryIO, Callable, Optional, TextIO

from .context import EmitContext, DEFAULT_BATCH_SIZE
from .errors import (
    AnalysisBuddyError,
    CancelledError,
    InvalidParamsError,
    SdkInternalError,
    UnsupportedInV1Error,
    build_error,
)
from .transport import NdjsonReader, NdjsonWriter, ProtocolError

ERR_INVALID_REQUEST = -32600
ERR_METHOD_NOT_FOUND = -32601
ERR_INVALID_PARAMS = -32602
ERR_INTERNAL_ERROR = -32603

KNOWN_METHODS = frozenset(
    {
        "initialize",
        "can_handle",
        "load_file",
        "parse",
        "schema",
        "key_values",
        "annotate",
        "unload_file",
        "shutdown",
        "cancel_parse",
    }
)


class _ParseState:
    __slots__ = ("file_id", "ctx", "thread")

    def __init__(self, file_id: str, ctx: EmitContext) -> None:
        self.file_id = file_id
        self.ctx = ctx
        self.thread: Optional[threading.Thread] = None


class AnalysisBuddyPlugin:
    """插件基类：注册式 handler（子类覆写 ``on_*`` 或使用 ``@plugin.handler``）。"""

    id: str = ""
    name: str = ""
    version: str = "0.1.0"

    def __init__(self, id: Optional[str] = None, name: Optional[str] = None,
                 version: Optional[str] = None) -> None:
        self._handlers: dict = {}
        self._stderr: TextIO = sys.stderr
        if id is not None:
            self.id = id
        if name is not None:
            self.name = name
        if version is not None:
            self.version = version

    # ------------------------------------------------------------------
    # 注册式 handler（装饰器糖衣，与覆写 on_* 等价，sdk-plugins.md §1.2）
    # ------------------------------------------------------------------

    def handler(self, method: str):
        if method not in KNOWN_METHODS:
            raise ValueError("unknown method {0!r}".format(method))

        def register(fn: Callable):
            self._handlers[method] = fn
            return fn

        return register

    def _call_handler(self, method: str, *args):
        handlers = getattr(self, "_handlers", {})
        if method in handlers:
            return handlers[method](*args)
        return getattr(self, "on_" + method)(*args)

    def _annotate_implemented(self) -> bool:
        handlers = getattr(self, "_handlers", {})
        if "annotate" in handlers:
            return True
        return type(self).on_annotate is not AnalysisBuddyPlugin.on_annotate

    # ------------------------------------------------------------------
    # 八 handler 默认实现（§1.2）
    # ------------------------------------------------------------------

    def on_initialize(self, params: dict) -> dict:
        """默认实现：返回插件元数据 + 能力声明；annotate 能力自动探测。"""
        return {
            "id": self.id,
            "name": self.name,
            "version": self.version,
            "capabilities": {
                "annotate": self._annotate_implemented(),
                "subscribe": False,
                "binary_sidecar": False,
            },
        }

    def on_can_handle(self, params: dict) -> dict:
        """默认弃权。"""
        return {"can_handle": False, "confidence": 0.0}

    def on_load_file(self, params: dict) -> dict:
        """默认空摘要；文件不存在等请抛 FileLoadFailedError（→ -32002）。"""
        return {}

    def on_parse(self, file_id: str, options: Optional[dict], ctx: EmitContext) -> int:
        """默认占位实现：未覆写时抛 UnsupportedInV1Error → -32005 unsupported_in_v1。

        parse 是必选方法（protocol-v1.md §2/§2.4）：作者必须覆写，未实现 parse
        的插件不合规——规范 §4.1 对「未实现的非可选方法」定义的是 -32601。
        SDK 刻意不返回 -32601，而是回 -32005 优雅降级：这是 2026-08-10 记录的
        产品决策（Python/.NET 双 SDK 一致）——宿主按 capabilities/错误码静默降级，
        不因单个插件缺失 parse 而崩溃宿主会话；-32601 保留给运行时方法不存在场景。
        -32003 parse_failed 仅用于解析过程中的真实失败。
        """
        raise UnsupportedInV1Error(
            "parse not implemented by this plugin", data={"file_id": file_id}
        )

    def on_schema(self) -> dict:
        return {"metrics": []}

    def on_key_values(self, file_id: str, timestamp_ms: int) -> dict:
        return {"entries": []}

    def on_annotate(self, file_id: str, range: dict) -> dict:
        raise UnsupportedInV1Error("annotate is not supported by this plugin")

    def on_unload_file(self, file_id: str) -> None:
        """默认无操作；幂等由 SDK 保证。"""

    # ------------------------------------------------------------------
    # 日志（§1.3 第 3 条：stderr 留给日志，格式 LEVEL|plugin_id|msg）
    # ------------------------------------------------------------------

    def log(self, level: str, msg: str) -> None:
        stderr = getattr(self, "_stderr", None) or sys.stderr
        stderr.write("{0}|{1}|{2}\n".format(str(level).upper(), self.id, msg))
        stderr.flush()

    # ------------------------------------------------------------------
    # serve() 主循环（§1.3）
    # ------------------------------------------------------------------

    def serve(
        self,
        stdin: Optional[BinaryIO] = None,
        stdout: Optional[BinaryIO] = None,
        stderr: Optional[TextIO] = None,
    ) -> None:
        """阻塞运行主循环；stdin EOF 或 shutdown 后返回（退出码由调用方/入口决定）。

        :param stdin: 二进制输入流，缺省 ``sys.stdin.buffer``；
        :param stdout: 二进制输出流，缺省 ``sys.stdout.buffer``（协议流量）；
        :param stderr: 文本日志流，缺省 ``sys.stderr``。
        """
        if stdin is None:
            stdin = sys.stdin.buffer
        if stdout is None:
            stdout = sys.stdout.buffer
        self._stderr = stderr if stderr is not None else (sys.stderr)
        self._handlers = getattr(self, "_handlers", {})

        reader = NdjsonReader(stdin)
        writer = NdjsonWriter(stdout)
        send_lock = threading.Lock()

        def sender(method: str, params: dict) -> None:
            with send_lock:
                writer.write_notification(method, params)

        self._loaded = set()
        self._parse_lock = threading.Lock()
        self._parse_state: Optional[_ParseState] = None
        self._exit_event = threading.Event()
        self._eof = False

        def read_loop() -> None:
            while not self._exit_event.is_set():
                try:
                    raw = reader.read_message()
                except ProtocolError as exc:
                    self.log("ERROR", "protocol error on stdin: {0}".format(exc))
                    self._exit_event.set()
                    return
                if raw is None:
                    self._eof = True
                    self._exit_event.set()
                    return
                try:
                    self._dispatch(raw, sender, send_lock, writer)
                except Exception:
                    self.log("ERROR", "unhandled dispatch failure:\n{0}".format(
                        traceback.format_exc()))
                    self._exit_event.set()
                    return

        reader_thread = threading.Thread(target=read_loop, name="ab-reader", daemon=True)
        reader_thread.start()
        reader_thread.join()

        # EOF 后等待在途 parse 收尾（有超时兜底，防孤儿进程且测试可确定性断言）；
        # shutdown 路径不等待（协议要求 ≤3s 内退出，进程退出即终止 parse 线程）。
        if self._eof:
            with self._parse_lock:
                state = self._parse_state
            if state is not None:
                state.thread.join(timeout=15.0)
        with send_lock:
            writer.flush()

    # ------------------------------------------------------------------
    # 路由（§1.3 第 6 条）
    # ------------------------------------------------------------------

    def _dispatch(self, raw: str, sender: Callable, send_lock: threading.Lock,
                  writer: NdjsonWriter) -> None:
        try:
            msg = json.loads(raw)
        except (json.JSONDecodeError, UnicodeDecodeError):
            self._respond_error(None, ERR_INVALID_REQUEST, "Invalid Request: malformed JSON",
                                None, send_lock, writer)
            return
        if not isinstance(msg, dict):
            self._respond_error(None, ERR_INVALID_REQUEST, "Invalid Request: not an object",
                                None, send_lock, writer)
            return
        req_id = msg.get("id")
        method = msg.get("method")
        valid_id = isinstance(req_id, int) and not isinstance(req_id, bool)
        if not valid_id or not isinstance(method, str):
            self._respond_error(req_id if valid_id else None, ERR_INVALID_REQUEST,
                                "Invalid Request: id/method missing or wrong type",
                                None, send_lock, writer)
            return
        if msg.get("jsonrpc") != "2.0":
            self._respond_error(req_id, ERR_INVALID_REQUEST, "Invalid Request: jsonrpc must be 2.0",
                                None, send_lock, writer)
            return
        if method not in KNOWN_METHODS:
            self._respond_error(req_id, ERR_METHOD_NOT_FOUND, "Method not found",
                                None, send_lock, writer)
            return
        handler = getattr(self, "_handle_" + method)
        handler(msg, sender, send_lock, writer)

    # ------------------------------------------------------------------
    # 各 method 处理
    # ------------------------------------------------------------------

    def _handle_initialize(self, msg, sender, send_lock, writer) -> None:
        self._run(msg["id"], lambda: self._call_handler("initialize", self._params_of(msg)),
                  send_lock, writer)

    def _handle_can_handle(self, msg, sender, send_lock, writer) -> None:
        params = self._params_or_32602(msg, send_lock, writer)
        if params is None:
            return
        for field in ("path", "name", "ext", "size_bytes", "head_sample"):
            if field not in params:
                self._respond_error(msg["id"], ERR_INVALID_PARAMS,
                                    "Invalid params: missing {0}".format(field),
                                    None, send_lock, writer)
                return
        self._run(msg["id"], lambda: self._call_handler("can_handle", params), send_lock, writer)

    def _handle_load_file(self, msg, sender, send_lock, writer) -> None:
        params = self._params_or_32602(msg, send_lock, writer)
        if params is None:
            return
        file_id = params.get("file_id")
        path = params.get("path")
        if not isinstance(file_id, str) or not file_id or not isinstance(path, str) or not path:
            self._respond_error(msg["id"], ERR_INVALID_PARAMS,
                                "Invalid params: file_id and path (non-empty strings) required",
                                None, send_lock, writer)
            return
        if file_id in self._loaded:
            # 幂等重入（§9 约定 2）：同 file_id 重新 load = 先 unload 再 load。
            try:
                self._call_handler("unload_file", file_id)
            except Exception as exc:
                code, message, data = self._map_exception(exc)
                self._respond_error(msg["id"], code, message, data, send_lock, writer)
                return
            self._loaded.discard(file_id)
        try:
            result = self._call_handler("load_file", params)
        except Exception as exc:
            code, message, data = self._map_exception(exc)
            self._respond_error(msg["id"], code, message, data, send_lock, writer)
            return
        self._loaded.add(file_id)
        self._respond_result(msg["id"], result, send_lock, writer)

    def _handle_parse(self, msg, sender, send_lock, writer) -> None:
        params = self._params_or_32602(msg, send_lock, writer)
        if params is None:
            return
        file_id = params.get("file_id")
        if not isinstance(file_id, str) or not file_id:
            self._respond_error(msg["id"], ERR_INVALID_PARAMS,
                                "Invalid params: file_id required", None, send_lock, writer)
            return
        with self._parse_lock:
            if self._parse_state is not None:
                self._respond_error(msg["id"], -32001,
                                    "plugin busy: a parse is already running",
                                    {"file_id": file_id}, send_lock, writer)
                return
            if file_id not in self._loaded:
                self._respond_error(msg["id"], ERR_INVALID_PARAMS,
                                    "Invalid params: file_id not loaded",
                                    {"file_id": file_id}, send_lock, writer)
                return
            options = params.get("options")
            if options is not None and not isinstance(options, dict):
                self._respond_error(msg["id"], ERR_INVALID_PARAMS,
                                    "Invalid params: options must be an object",
                                    None, send_lock, writer)
                return
            ctx = EmitContext(file_id, sender, stderr=self._stderr)
            state = _ParseState(file_id, ctx)
            self._parse_state = state
        thread = threading.Thread(
            target=self._parse_runner,
            args=(msg["id"], file_id, options, ctx, state, send_lock, writer),
            name="ab-parse",
            daemon=True,
        )
        state.thread = thread
        thread.start()

    def _parse_runner(self, req_id, file_id, options, ctx, state, send_lock, writer) -> None:
        try:
            ctx.start()
            total = self._call_handler("parse", file_id, options, ctx)
            if isinstance(total, bool) or not isinstance(total, int):
                total = ctx.records_so_far
            ctx.finish()
            self._respond_result(req_id, {"records_total": total}, send_lock, writer)
        except CancelledError as exc:
            ctx.stop()
            self._respond_error(req_id, exc.code, exc.message, exc.data, send_lock, writer)
        except Exception as exc:
            ctx.stop()
            code, message, data = self._map_exception(exc)
            self._respond_error(req_id, code, message, data, send_lock, writer)
        finally:
            with self._parse_lock:
                if self._parse_state is state:
                    self._parse_state = None

    def _handle_schema(self, msg, sender, send_lock, writer) -> None:
        params = self._params_of(msg)
        if params:
            self._respond_error(msg["id"], ERR_INVALID_PARAMS,
                                "Invalid params: schema takes no params", None, send_lock, writer)
            return
        self._run(msg["id"], lambda: self._call_handler("schema"), send_lock, writer)

    def _handle_key_values(self, msg, sender, send_lock, writer) -> None:
        params = self._params_or_32602(msg, send_lock, writer)
        if params is None:
            return
        file_id = params.get("file_id")
        timestamp_ms = params.get("timestamp_ms")
        valid_ts = isinstance(timestamp_ms, int) and not isinstance(timestamp_ms, bool)
        if not isinstance(file_id, str) or not file_id or not valid_ts:
            self._respond_error(msg["id"], ERR_INVALID_PARAMS,
                                "Invalid params: file_id and timestamp_ms required",
                                None, send_lock, writer)
            return
        if file_id not in self._loaded:
            self._respond_error(msg["id"], ERR_INVALID_PARAMS,
                                "Invalid params: file_id not loaded",
                                {"file_id": file_id}, send_lock, writer)
            return
        self._run(msg["id"], lambda: self._call_handler("key_values", file_id, timestamp_ms),
                  send_lock, writer)

    def _handle_annotate(self, msg, sender, send_lock, writer) -> None:
        if not self._annotate_implemented():
            self._respond_error(msg["id"], -32005,
                                "annotate is not supported by this plugin", None,
                                send_lock, writer)
            return
        params = self._params_or_32602(msg, send_lock, writer)
        if params is None:
            return
        file_id = params.get("file_id")
        range_ = params.get("range")
        if not isinstance(file_id, str) or not file_id or not isinstance(range_, dict):
            self._respond_error(msg["id"], ERR_INVALID_PARAMS,
                                "Invalid params: file_id and range required", None,
                                send_lock, writer)
            return
        if not isinstance(range_.get("start_ms"), int) or not isinstance(range_.get("end_ms"), int):
            self._respond_error(msg["id"], ERR_INVALID_PARAMS,
                                "Invalid params: range.start_ms/end_ms integers required",
                                None, send_lock, writer)
            return
        if file_id not in self._loaded:
            self._respond_error(msg["id"], ERR_INVALID_PARAMS,
                                "Invalid params: file_id not loaded",
                                {"file_id": file_id}, send_lock, writer)
            return
        self._run(msg["id"], lambda: self._call_handler("annotate", file_id, range_),
                  send_lock, writer)

    def _handle_unload_file(self, msg, sender, send_lock, writer) -> None:
        params = self._params_or_32602(msg, send_lock, writer)
        if params is None:
            return
        file_id = params.get("file_id")
        if not isinstance(file_id, str) or not file_id:
            self._respond_error(msg["id"], ERR_INVALID_PARAMS,
                                "Invalid params: file_id required", None, send_lock, writer)
            return
        if file_id in self._loaded:
            try:
                self._call_handler("unload_file", file_id)
            except Exception as exc:
                code, message, data = self._map_exception(exc)
                self._respond_error(msg["id"], code, message, data, send_lock, writer)
                return
            self._loaded.discard(file_id)
        self._respond_result(msg["id"], {}, send_lock, writer)

    def _handle_shutdown(self, msg, sender, send_lock, writer) -> None:
        params = self._params_of(msg)
        if params:
            self._respond_error(msg["id"], ERR_INVALID_PARAMS,
                                "Invalid params: shutdown takes no params", None, send_lock, writer)
            return
        self._respond_result(msg["id"], {}, send_lock, writer)
        self._exit_event.set()

    def _handle_cancel_parse(self, msg, sender, send_lock, writer) -> None:
        params = self._params_or_32602(msg, send_lock, writer)
        if params is None:
            return
        file_id = params.get("file_id")
        if not isinstance(file_id, str) or not file_id:
            self._respond_error(msg["id"], ERR_INVALID_PARAMS,
                                "Invalid params: file_id required", None, send_lock, writer)
            return
        with self._parse_lock:
            state = self._parse_state
        if state is not None and state.file_id == file_id:
            state.ctx.cancel()
        self._respond_result(msg["id"], {}, send_lock, writer)

    # ------------------------------------------------------------------
    # 工具
    # ------------------------------------------------------------------

    def _params_of(self, msg: dict) -> dict:
        params = msg.get("params")
        if params is None:
            return {}
        if not isinstance(params, dict):
            raise InvalidParamsError("params must be an object")
        return params

    def _params_or_32602(self, msg, send_lock, writer) -> Optional[dict]:
        try:
            return self._params_of(msg)
        except InvalidParamsError as exc:
            self._respond_error(msg["id"], exc.code, exc.message, None, send_lock, writer)
            return None

    def _run(self, req_id, fn, send_lock, writer) -> None:
        try:
            result = fn()
        except Exception as exc:
            code, message, data = self._map_exception(exc)
            self._respond_error(req_id, code, message, data, send_lock, writer)
        else:
            self._respond_result(req_id, result, send_lock, writer)

    def _map_exception(self, exc: BaseException):
        """§1.5 映射表：七类异常 → 错误码；未分类兜底 -32603 + stderr traceback。"""
        if isinstance(exc, AnalysisBuddyError):
            return exc.code, exc.message, exc.data
        self.log("ERROR", "unhandled exception:\n{0}".format(traceback.format_exc()))
        return ERR_INTERNAL_ERROR, "Internal error", None

    def _respond_result(self, req_id, result, send_lock, writer) -> None:
        with send_lock:
            writer.write_response(req_id, result)

    def _respond_error(self, req_id, code, message, data, send_lock, writer) -> None:
        """错误码硬校验（§1.5）：越界即 SDK 内部错误，转为 -32603。"""
        try:
            error = build_error(code, message, data)
        except SdkInternalError as exc:
            self.log("ERROR", "invalid error code {0} ({1}); falling back to -32603".format(
                code, exc))
            error = {"code": ERR_INTERNAL_ERROR, "message": "Internal error"}
        with send_lock:
            writer.write_error(req_id, error)
