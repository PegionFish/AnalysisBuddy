# analysisbuddy-sdk

AnalysisBuddy 插件 Python SDK（协议 v1）——**零第三方依赖**（纯 stdlib，
Python 3.10~3.14）。设计正本：`AnalysisBuddy-devdocs/deep-dive/sdk-plugins.md` §1。

## 安装

```powershell
pip install -e sdk/python
```

## 最小插件

```python
from analysisbuddy import AnalysisBuddyPlugin

class HelloPlugin(AnalysisBuddyPlugin):
    id, name, version = "hello-plugin", "Hello", "0.1.0"

    def on_can_handle(self, p):
        return {"can_handle": p["ext"] == "log", "confidence": 0.9}

    def on_parse(self, file_id, options, ctx):
        total = 0
        for line in open(self._files[file_id], encoding="utf-8"):
            ctx.check_cancelled()
            ts, value = parse_line(line)
            ctx.emit_records([{"timestamp": ts, "metric": "demo", "value": value}])
            total += 1
        return total

if __name__ == "__main__":
    HelloPlugin().serve()
```

## 测试

```powershell
python -m pytest tests/ -q
```
