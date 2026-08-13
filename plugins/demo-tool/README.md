# demo-tool — 演示日志插件（Python + 自家 SDK，dogfood）

模拟某游戏测试工具产出的 Log（FRAME/STATE/EVENT 三类行），用 `analysisbuddy-sdk`（D1 路 Python SDK）编写，dogfood SDK 公共 API。仓库根即插件目录，`git clone` 到 `plugins/demo-tool/` 即用（§4.5 自适应写法：`entry: {command: "python", args: ["main.py"]}`）。

## 输入行格式（§4.1）

```text
2026-08-07T10:00:00.123+08:00 FRAME fps=60.1 frame_ms=16.6 cpu_temp=63.2
2026-08-07T10:00:05.000+08:00 STATE scene=boss_fight hero_hp=100 stamina=80
2026-08-07T10:00:12.481+08:00 EVENT crash_dump reason="GPU hang" level=error
```

- `FRAME`：指标来源（fps / frame_time / cpu_temp，数值）
- `STATE`：状态变更，key_values 来源
- `EVENT`：annotate 来源
- 无法识别的行 stderr 告警并跳过计数

## 指标（§4.2，3 个指标，三种聚合）

| metric id | name | unit | aggregation |
|-----------|------|------|-------------|
| `fps` | 帧率 | `fps` | `last` |
| `frame_time` | 帧耗时 | `ms` | `avg` |
| `cpu_temp` | CPU 温度 | `°C` | `max` |

每条 FRAME 行产出 3 条 `Record`（同 timestamp，不同 metric）；`tags: {"scene": <当时场景>}` 取自 ≤T 的最新 STATE（顺序扫描即得）。

## key_values（§4.3，真实语义）

STATE 行按 timestamp 存为有序列表，查询时 `bisect` 二分定位 ≤T 的最近一条，合并其前全部状态行（后者覆盖前者）得状态快照；附 `last_event`（≤T 最近 EVENT，无则省略该 entry）。定位 O(log n)，100MB 级文件下响应远低于 10s 超时（有单测断言）。

## annotate（§4.4）

`capabilities.annotate: true`（SDK 自动探测 on_annotate 覆写）；EVENT → `{timestamp_ms, label: reason, level}`，`level` 缺省 `"info"`；范围内无事件返回空数组。

## 运行依赖

- Python 3.10+（纯 stdlib 包，零第三方依赖）
- **SDK 已 vendor**：`analysisbuddy/` 是从 `sdk/python/analysisbuddy` 复制的随包副本
  （P1 修复：便携包不 `pip install`、不设 `PYTHONPATH`，靠「脚本同目录即 sys.path[0]」
  自适应加载）。开发机无需 `pip install`，`python main.py` 即用。
  同步纪律：`sdk/python/analysisbuddy` 有改动时，必须同步刷新本目录副本
  （`scripts/bundle-zip.ps1` 的 demo-tool 冒烟会验证打包内 SDK 可导入）。
- 仓库内无构建产物、无打包步骤（§4.5 自适应要点）；`.git/`、`tests/` 等无关文件宿主全无视（MAN-09）

## 开发期测试

```powershell
pip install -e sdk/python      # D1 路 SDK；未安装时 tests 自动用 analysisbuddy_stub 替身
python -m pytest plugins/demo-tool/tests/ -q
```

`tests/test_e2e.py` 以子进程拉起 `main.py` 走真实 NDJSON 会话（initialize → schema → can_handle → load_file → parse → key_values → annotate → unload_file → shutdown），断言记录数 == FRAME 行数 × 3 与退出码 0。

## validator 校验

```powershell
plugin check .\plugins\demo-tool\ --behavior --fixture .\tests\fixtures\small_txt.log --json
echo $LASTEXITCODE   # 期望 0
```
