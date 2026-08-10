# demo-tool parser — FRAME/STATE/EVENT 三类行解析（纯 stdlib：re、bisect、datetime）。
#
# 输入行格式（sdk-plugins.md §4.1）：
#   2026-08-07T10:00:00.123+08:00 FRAME fps=60.1 frame_ms=16.6 cpu_temp=63.2
#   2026-08-07T10:00:05.000+08:00 STATE scene=boss_fight hero_hp=100 stamina=80
#   2026-08-07T10:00:12.481+08:00 EVENT crash_dump reason="GPU hang" level=error
#
# - FRAME：指标来源（fps / frame_ms / cpu_temp，数值）
# - STATE：状态变更，key_values 来源（值可为字符串或数值）
# - EVENT：annotate 来源（label 取 reason，level 缺省 "info"）
# - 无法识别的行返回 None，由调用方 stderr 告警并跳过计数
#
# key_values 语义（§4.3）：STATE 行按 timestamp 存为有序列表；查询时 bisect 二分
# 定位 ≤T 的最近一条，合并其前全部状态行（后者覆盖前者）得状态快照。

import bisect
import datetime
import re
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Union

# 键值对：key=value，value 可为双引号包裹（可含空格）或非空白串
_KV_RE = re.compile(r'([A-Za-z_][A-Za-z0-9_]*)=(?:"([^"]*)"|([^\s]+))')

# 行前缀：ISO8601 时间戳（含时区）+ 类型关键字
_LINE_RE = re.compile(r'^(\S+)\s+([A-Za-z_]+)\s+(.*)$')

StateValue = Union[str, int, float]


def parse_timestamp_ms(text: str) -> Optional[int]:
    """ISO8601（含时区）→ UTC 毫秒；解析失败返回 None。"""
    try:
        dt = datetime.datetime.fromisoformat(text)
    except ValueError:
        return None
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=datetime.timezone.utc)
    return int(dt.timestamp() * 1000)


def _coerce_state_value(raw: str) -> StateValue:
    """STATE 取值：数值字符串转 int/float，否则保留字符串。"""
    try:
        return int(raw)
    except ValueError:
        pass
    try:
        return float(raw)
    except ValueError:
        return raw


def _kv_pairs(rest: str) -> Dict[str, str]:
    return {k: (quoted if quoted else bare) for k, quoted, bare in _KV_RE.findall(rest)}


@dataclass(frozen=True)
class FrameLine:
    ts_ms: int
    fps: float
    frame_ms: float
    cpu_temp: float


@dataclass(frozen=True)
class StateLine:
    ts_ms: int
    values: Dict[str, StateValue] = field(default_factory=dict)


@dataclass(frozen=True)
class EventLine:
    ts_ms: int
    label: str
    level: str
    local_time: str  # 行内钟面时间 HH:MM:SS（供 last_event 展示）


ParsedLine = Union[FrameLine, StateLine, EventLine]


def parse_line(line: str) -> Optional[ParsedLine]:
    """解析一行日志；无法识别返回 None。"""
    line = line.rstrip('\n').rstrip('\r')
    m = _LINE_RE.match(line)
    if not m:
        return None
    ts_text, kind, rest = m.groups()
    ts_ms = parse_timestamp_ms(ts_text)
    if ts_ms is None:
        return None
    kvs = _kv_pairs(rest)

    if kind == 'FRAME':
        try:
            return FrameLine(
                ts_ms=ts_ms,
                fps=float(kvs['fps']),
                frame_ms=float(kvs['frame_ms']),
                cpu_temp=float(kvs['cpu_temp']),
            )
        except (KeyError, ValueError):
            return None
    if kind == 'STATE':
        values: Dict[str, StateValue] = {k: _coerce_state_value(v) for k, v in kvs.items()}
        return StateLine(ts_ms=ts_ms, values=values)
    if kind == 'EVENT':
        return EventLine(
            ts_ms=ts_ms,
            label=kvs.get('reason', kvs.get('label', '')),
            level=kvs.get('level', 'info'),
            local_time=ts_text[11:19],
        )
    return None


class StateIndex:
    """按 timestamp 有序的 STATE 行索引（二分定位，§4.3）。
    定位用 bisect O(log n)；快照合并其前全部状态行（后覆盖前）符合 §4.3 语义。"""

    def __init__(self) -> None:
        self._states: List[StateLine] = []
        self._ts: List[int] = []

    def add(self, line: StateLine) -> None:
        self._states.append(line)
        self._ts.append(line.ts_ms)

    def snapshot_at(self, timestamp_ms: int) -> Dict[str, StateValue]:
        """二分定位 ≤T 的最近一条，合并其前全部状态行（后覆盖前）得快照。"""
        idx = bisect.bisect_right(self._ts, timestamp_ms)
        merged: Dict[str, StateValue] = {}
        for state in self._states[:idx]:
            merged.update(state.values)
        return merged

    def __len__(self) -> int:
        return len(self._states)


class EventIndex:
    """按 timestamp 有序的 EVENT 行索引（annotate / last_event 用）。"""

    def __init__(self) -> None:
        self._events: List[EventLine] = []
        self._ts: List[int] = []

    def add(self, line: EventLine) -> None:
        self._events.append(line)
        self._ts.append(line.ts_ms)

    def last_at(self, timestamp_ms: int) -> Optional[EventLine]:
        idx = bisect.bisect_right(self._ts, timestamp_ms)
        return self._events[idx - 1] if idx > 0 else None

    def in_range(self, start_ms: int, end_ms: int) -> List[EventLine]:
        lo = bisect.bisect_left(self._ts, start_ms)
        hi = bisect.bisect_right(self._ts, end_ms)
        return self._events[lo:hi]

    def __len__(self) -> int:
        return len(self._events)
