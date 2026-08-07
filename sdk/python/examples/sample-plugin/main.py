"""SamplePlugin —— 用 analysisbuddy-sdk 写的最小合规样例插件（sdk-plugins.md §1.6）。

日志格式（sample.log）：每行 ``<ISO8601 ts> <metric> <value>``，
特殊行 ``state <key>=<value>`` 记录关键状态（key_values 数据源）。

本目录（examples/sample-plugin）即插件仓库根：复制本目录、改 plugin.json 的 id
与 display_name 即可作为新插件起步（详见 README.md）。
"""

import os
import re
from datetime import datetime

from analysisbuddy import AnalysisBuddyPlugin, FileLoadFailedError

_TS_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}")


class SamplePlugin(AnalysisBuddyPlugin):
    id, name, version = "sample-plugin", "Sample Plugin", "0.1.0"

    METRICS = [
        {"id": "fps", "name": "帧率", "unit": "fps", "aggregation": "avg",
         "description": "每秒帧数"},
        {"id": "frame_ms", "name": "帧耗时", "unit": "ms", "aggregation": "avg",
         "description": "单帧渲染耗时"},
        {"id": "cpu_temp", "name": "CPU 温度", "unit": "°C", "aggregation": "avg",
         "description": "CPU 温度"},
    ]

    def __init__(self):
        super().__init__()
        self._paths = {}
        self._states = {}

    # ---- 匹配与生命周期 ----

    def on_can_handle(self, p):
        """log/txt 且头部首行形如 ISO8601 时间戳 → 认领。"""
        if p["ext"] not in ("log", "txt"):
            return {"can_handle": False, "confidence": 0.0}
        head = p.get("head_sample", "")
        first_line = head.split("\n", 1)[0].strip()
        if _TS_RE.match(first_line):
            return {"can_handle": True, "confidence": 0.9,
                    "reason": "timestamp-prefixed lines detected"}
        return {"can_handle": False, "confidence": 0.0}

    def on_load_file(self, p):
        path = p["path"]
        if not os.path.exists(path):
            raise FileLoadFailedError("file not found", data={"path": path})
        self._paths[p["file_id"]] = path
        self._states[p["file_id"]] = []  # [(ts_ms, {"scene": ...}), ...]
        return {}

    def on_unload_file(self, file_id):
        self._paths.pop(file_id, None)
        self._states.pop(file_id, None)

    # ---- 解析 ----

    def on_parse(self, file_id, options, ctx):
        total = 0
        skipped = 0
        lines = open(self._paths[file_id], encoding="utf-8").read().splitlines()
        for i, line in enumerate(lines):
            ctx.check_cancelled()
            if not line.strip():
                continue
            try:
                ts = self._parse_ts(line.split(None, 1)[0])
            except ValueError:
                skipped += 1
                continue
            parts = line.split()
            if len(parts) < 3:
                skipped += 1
                continue
            metric, rest = parts[1], parts[2:]
            if metric == "state":
                # key_values 数据源：state scene=boss_fight
                kv = {}
                for token in rest:
                    if "=" in token:
                        k, v = token.split("=", 1)
                        kv[k] = v
                self._states[file_id].append((ts, kv))
                continue
            if metric not in {m["id"] for m in self.METRICS}:
                skipped += 1
                continue
            try:
                value = float(rest[0])
            except (IndexError, ValueError):
                skipped += 1
                continue
            ctx.emit_records([{"timestamp": ts, "metric": metric, "value": value}])
            total += 1
            if i % 1000 == 0:
                ctx.progress(percent=i / max(len(lines), 1) * 100)
        if skipped:
            self.log("WARN", "sample-plugin: skipped {0} unparseable line(s) in {1}".format(
                skipped, file_id))
        return total

    def _parse_ts(self, text):
        # 无时区视为 UTC（协议要求 UTC 毫秒）；fromisoformat 3.10 兼容。
        if "+" not in text and "z" not in text.lower() and "Z" not in text:
            text = text + "+00:00"
        return int(datetime.fromisoformat(text).timestamp() * 1000)

    # ---- 查询 ----

    def on_schema(self):
        return {"metrics": self.METRICS}

    def on_key_values(self, file_id, timestamp_ms):
        merged = {}
        for ts, kv in self._states.get(file_id, []):
            if ts <= timestamp_ms:
                merged.update(kv)
        return {"entries": [{"key": k, "value": v} for k, v in merged.items()]}


if __name__ == "__main__":
    SamplePlugin().serve()
