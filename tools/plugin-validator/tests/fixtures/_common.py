"""_common.py —— validator 行为回放 fixture 共享的协议服务端循环（测试专用）。

实现 protocol-v1.md 最小合规子集：stdin 逐行读请求、stdout 整行 JSON 应答
（LF 行尾、每帧 flush）、stderr 记日志、shutdown 应答后退出、stdin EOF 退出码 0。
各 fixture 目录的 plugin.py 继承本模块的 Plugin 并覆写 on_* 方法以制造目标违规。

本文件不是交付物，仅供 tools/plugin-validator/tests/fixtures 使用。
"""

import json
import sys

FILE_ID = "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c"

METRICS = [
    {"id": "fps", "name": "FPS", "aggregation": "last"},
    {"id": "frame_ms", "name": "Frame ms", "aggregation": "avg"},
    {"id": "mem_mb", "name": "Memory MB", "aggregation": "max"},
]


class Plugin:
    """默认实现 = 完全合规的参考插件（good-plugin 直接继承即可）。"""

    plugin_id = "good-plugin"
    plugin_name = "Good Fixture"
    plugin_version = "0.1.0"

    def __init__(self):
        self.loaded = {}
        self._load_count = 0

    # ---- 帧输出（stdout 只准协议帧；二进制模式写 LF 行尾，避免 Windows
    #      文本模式把 \n 翻译成 \r\n） ----
    def send(self, obj):
        sys.stdout.buffer.write(
            json.dumps(obj, separators=(",", ":")).encode("utf-8") + b"\n"
        )
        sys.stdout.buffer.flush()

    def reply(self, req_id, result):
        self.send({"jsonrpc": "2.0", "id": req_id, "result": result})

    def error(self, req_id, code, message, data=None):
        err = {"code": code, "message": message}
        if data is not None:
            err["data"] = data
        self.send({"jsonrpc": "2.0", "id": req_id, "error": err})

    # ---- 方法钩子（fixture 覆写点；签名 = (req_id, params)） ----
    def on_initialize(self, req_id, params):
        self.reply(req_id, {
            "id": self.plugin_id,
            "name": self.plugin_name,
            "version": self.plugin_version,
            "capabilities": {"annotate": False, "subscribe": False, "binary_sidecar": False},
        })

    def on_schema(self, req_id, params):
        self.reply(req_id, {"metrics": METRICS})

    def on_can_handle(self, req_id, params):
        claimed = params.get("ext", "") == "csv"
        self.reply(req_id, {"can_handle": claimed, "confidence": 0.9,
                            "reason": "fixture default claim"})

    def on_load_file(self, req_id, params):
        self._load_count += 1
        self.loaded[params["file_id"]] = params["path"]
        self.reply(req_id, {})

    def on_parse(self, req_id, params):
        """默认实现：读取已加载的 CSV，每行 3 条记录（fps/frame_ms/mem_mb），
        分批（2 行/批）+ progress 心跳回传。"""
        path = self.loaded[params["file_id"]]
        with open(path, "r", encoding="utf-8") as fh:
            data_lines = [ln for ln in fh.read().splitlines() if ln]
        data_lines = [ln for ln in data_lines if not ln.startswith("timestamp")]
        total = 0
        seq = 0
        batch = []
        for ln in data_lines:
            cols = ln.split(",")
            ts = int(cols[0])
            batch.append({"timestamp": ts, "metric": "fps", "value": float(cols[1])})
            batch.append({"timestamp": ts, "metric": "frame_ms", "value": float(cols[2])})
            batch.append({"timestamp": ts, "metric": "mem_mb", "value": float(cols[3])})
            total += 3
            if len(batch) >= 6:
                self.send({"jsonrpc": "2.0", "method": "progress",
                           "params": {"file_id": params["file_id"], "records_so_far": total}})
                self.send({"jsonrpc": "2.0", "method": "RecordBatch",
                           "params": {"file_id": params["file_id"], "seq": seq,
                                      "records": batch, "done": False}})
                seq += 1
                batch = []
        self.send({"jsonrpc": "2.0", "method": "progress",
                   "params": {"file_id": params["file_id"], "records_so_far": total}})
        self.send({"jsonrpc": "2.0", "method": "RecordBatch",
                   "params": {"file_id": params["file_id"], "seq": seq,
                              "records": batch, "done": True}})
        self.reply(req_id, {"records_total": total})

    def on_key_values(self, req_id, params):
        self.reply(req_id, {"entries": [{"key": "scene", "value": "boss"}]})

    def on_unload_file(self, req_id, params):
        self.loaded.pop(params["file_id"], None)
        self.reply(req_id, {})

    def on_shutdown(self, req_id, params):
        self.reply(req_id, {})

    # ---- 主循环 ----
    def run(self):
        for raw in sys.stdin.buffer:
            line = raw.decode("utf-8", "replace").strip()
            if not line:
                continue
            try:
                req = json.loads(line)
            except Exception:
                continue
            method = req.get("method")
            rid = req.get("id")
            if rid is None:
                continue  # 宿主不应当发通知；忽略
            handler = getattr(self, "on_" + method, None)
            if handler is None:
                self.error(rid, -32601, "Method not found")
                continue
            handler(rid, req.get("params") or {})
            if method == "shutdown":
                break
        self.on_eof()

    def on_eof(self):
        # stdin EOF → 自行退出（protocol-v1.md §9 第 5 条）
        sys.stdout.flush()


def main(cls):
    cls().run()
