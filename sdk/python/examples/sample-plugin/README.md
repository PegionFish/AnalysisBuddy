# sample-plugin（合规样例插件）

用 `analysisbuddy-sdk` 写的最小合规插件：**SDK 用法即文档**（对照
`sdk-plugins.md` §1.6）。本目录即插件仓库根。

## 最短起步路径（复制即用）

```powershell
# 1. 复制本目录为新插件（如 my-tool/），改 plugin.json 的 id/display_name/version
Copy-Item -Recurse .\sdk\python\examples\sample-plugin .\plugins\my-tool

# 2. 改 my-tool/plugin.json：id 必须与目录名一致（MAN-02）
# 3. 改 main.py 里的 class SamplePlugin 的 id/name 与解析逻辑

# 4. 本机一次性安装 SDK（纯 stdlib 包，无第三方依赖）
pip install -e sdk/python

# 5. 自检（E 路 validator，需 plugin check 就绪）
plugin check .\plugins\my-tool\ --behavior --fixture .\sdk\python\examples\sample-plugin\sample.log --json
# 期望退出码 0，--json 输出 rules 数组无 error 级条目
```

宿主发现规则（protocol.md §7.1）：整个目录拖入 `plugins/<名>/` 即被识别，
仓库内其他文件（tests/、.git/ 等）宿主全无视。

## 样例日志格式（sample.log）

每行 `ISO8601 时间戳 指标 数值`：

```
2026-08-07T10:00:00.123 fps 59.8
2026-08-07T10:00:03.000 state scene=main_menu
```

- `fps` / `frame_ms` / `cpu_temp` → 时序指标（schema 三指标）；
- `state key=value` → 关键状态行，`key_values(T)` 取 ≤T 最新状态。

## 自测

```powershell
python -m pytest sdk/python/tests/test_sample_plugin.py -q
```

## 目录结构

```
sample-plugin/
├── plugin.json    # manifest：entry = python main.py（解释器按 PATH/py launcher 查找）
├── main.py        # SamplePlugin(AnalysisBuddyPlugin) + serve()
├── sample.log     # 行为回放夹具（--fixture 用）
└── README.md
```
