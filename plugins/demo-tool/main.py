# demo-tool — 模拟游戏测试工具日志插件（dogfood analysisbuddy-sdk，sdk-plugins.md §4）。
#
# 仅经 analysisbuddy-sdk 公共 API（AnalysisBuddyPlugin 子类 + serve()），
# 不触碰 SDK 内部模块。仓库根即插件目录（plugin.json §4.5），clone 即用：
#   pip install analysisbuddy-sdk   # 开发机一次安装
#   python main.py                  # 宿主以 plugin.json entry 拉起
#
# 指标（§4.2）：fps(last) / frame_time(avg) / cpu_temp(max)，每条 FRAME 行产出 3 条 Record，
# tags.scene 取 ≤T 最新 STATE 场景（顺序扫描即得）。
# key_values（§4.3）：STATE 行二分索引 → ≤T 状态快照 + last_event。
# annotate（§4.4）：EVENT 行 → 标注事件，level 缺省 "info"。

import datetime
import os
from typing import Dict, List, Optional

from analysisbuddy import AnalysisBuddyPlugin

from parser import EventIndex, EventLine, FrameLine, StateIndex, StateLine, parse_line


class DemoToolPlugin(AnalysisBuddyPlugin):
    id = "demo-tool"
    name = "演示工具解析器"
    version = "0.1.0"

    def __init__(self) -> None:
        super().__init__()
        # file_id -> 已驻留数据（§2.3：load 时读取并驻留原始数据）
        self._files: Dict[str, Dict] = {}

    # ---- 生命周期 ------------------------------------------------------

    def on_initialize(self, params: dict) -> dict:
        result = super().on_initialize(params)
        # annotate 能力由 SDK 自动探测（覆写 on_annotate 即 true，§1.2）
        return result

    def on_can_handle(self, p: dict) -> dict:
        ext = p.get("ext", "")
        head = (p.get("head_sample") or "").lower()
        fingerprint_hit = ("frame fps=" in head) or ("state scene=" in head)
        can = ext in ("log", "txt") and fingerprint_hit
        return {"can_handle": can, "confidence": 0.9 if can else 0.0,
                "reason": "FRAME/STATE/EVENT log detected" if can else None}

    def on_load_file(self, p: dict) -> dict:
        path = p["path"]
        if not os.path.exists(path):
            from analysisbuddy import FileLoadFailedError
            raise FileLoadFailedError(f"file not found: {path}", data={"path": path})

        states = StateIndex()
        events = EventIndex()
        frame_count = 0
        unknown_count = 0
        first_ts: Optional[int] = None
        last_ts: Optional[int] = None

        with open(path, "r", encoding="utf-8", errors="replace") as f:
            for raw in f:
                parsed = parse_line(raw)
                if parsed is None:
                    unknown_count += 1
                    continue
                if first_ts is None:
                    first_ts = parsed.ts_ms
                last_ts = parsed.ts_ms
                if isinstance(parsed, StateLine):
                    states.add(parsed)
                elif isinstance(parsed, EventLine):
                    events.add(parsed)
                else:  # FrameLine
                    frame_count += 1

        if unknown_count:
            self.log("WARN", f"{path}: skipped {unknown_count} unrecognized line(s)")

        self._files[p["file_id"]] = {
            "path": path,
            "states": states,
            "events": events,
            "frame_count": frame_count,
            "unknown_count": unknown_count,
        }
        summary = {"record_count_hint": frame_count}
        if first_ts is not None:
            summary["time_range"] = {"start_ms": first_ts, "end_ms": last_ts}
        summary["note"] = f"demo-tool: {frame_count} FRAME rows, {unknown_count} unknown lines"
        return summary

    def on_parse(self, file_id: str, options: Optional[dict], ctx) -> int:
        data = self._files[file_id]
        path = data["path"]
        total = 0
        current_scene: Optional[str] = None
        line_no = 0
        file_bytes = os.path.getsize(path)
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            for raw in f:
                ctx.check_cancelled()
                line_no += 1
                parsed = parse_line(raw)
                if parsed is None:
                    continue
                if isinstance(parsed, StateLine):
                    scene = parsed.values.get("scene")
                    if isinstance(scene, str):
                        current_scene = scene
                    continue
                if isinstance(parsed, EventLine):
                    continue
                # FrameLine → 3 条 Record（同 timestamp，不同 metric）
                records = [
                    {"timestamp": parsed.ts_ms, "metric": "fps", "value": parsed.fps},
                    {"timestamp": parsed.ts_ms, "metric": "frame_time", "value": parsed.frame_ms},
                    {"timestamp": parsed.ts_ms, "metric": "cpu_temp", "value": parsed.cpu_temp},
                ]
                if current_scene is not None:
                    for r in records:
                        r["tags"] = {"scene": current_scene}
                ctx.emit_records(records)
                total += 3
                if line_no % 20000 == 0:
                    ctx.progress(percent=None, bytes_read=None)
        # 收尾 progress（percent 按字节估算）；serve() 期间 SDK 另有 2s 心跳兜底
        ctx.progress(percent=100.0, bytes_read=file_bytes)
        return total

    def on_schema(self) -> dict:
        return {"metrics": [
            {"id": "fps", "name": "帧率", "unit": "fps", "aggregation": "last"},
            {"id": "frame_time", "name": "帧耗时", "unit": "ms", "aggregation": "avg"},
            {"id": "cpu_temp", "name": "CPU 温度", "unit": "°C", "aggregation": "max"},
        ]}

    # ---- key_values（§4.3） ---------------------------------------------

    def on_key_values(self, file_id: str, timestamp_ms: int) -> dict:
        data = self._files.get(file_id)
        if data is None:
            return {"entries": []}
        snapshot = data["states"].snapshot_at(timestamp_ms)
        entries: List[dict] = []
        for key, value in snapshot.items():
            entry = {"key": key, "value": value}
            if key in ("hero_hp", "player_hp"):
                entry["unit"] = "%"
            entries.append(entry)
        last_event = data["events"].last_at(timestamp_ms)
        if last_event is not None:
            entries.append({"key": "last_event",
                            "value": f"{last_event.label} @ {last_event.local_time}"})
        return {"entries": entries}

    # ---- annotate（§4.4） ------------------------------------------------

    def on_annotate(self, file_id: str, range: dict) -> dict:
        data = self._files.get(file_id)
        if data is None:
            return {"events": []}
        start_ms = range["start_ms"]
        end_ms = range["end_ms"]
        events = data["events"].in_range(start_ms, end_ms)
        return {"events": [
            {"timestamp_ms": e.ts_ms, "label": e.label, "level": e.level}
            for e in events
        ]}

    def on_unload_file(self, file_id: str) -> None:
        self._files.pop(file_id, None)


if __name__ == "__main__":
    DemoToolPlugin().serve()
