## 演练：模拟劣化 >15%

基线 parse_ms=100ms/ipc=50MB/s → 当前 130ms/40MB/s。

```text
## 性能回归检测（>15% 门槛）

对比基线 `0000000000000000000000000000000000000000`（fixture `bench_10mb.csv`）

| 指标 | 基线 | 当前 | 变化 |
|---|---|---|---|
| parse_ms | 100.00 | 130.00 | 劣化 30.0% |
| ipc_mbps | 50.00 | 40.00 | 劣化 20.0% |

### JSON diff（片段）

基线：
```json
{
  "git_sha": "0000000000000000000000000000000000000000",
  "arch": "x86_64",
  "machine": "perf-drill",
  "gpu": null,
  "fixture": "bench_10mb.csv",
  "metrics": {
    "parse_ms": 100.0,
    "rss_peak_mb": 200.0,
    "ipc_mbps": 50.0,
    "first_paint_ms": 500.0,
    "drag_fps_p95": 45.0
  },
  "thresholds_pass": [
    true,
    true,
    true,
    true
  ]
}
```

当前：
```json
{
  "git_sha": "0000000000000000000000000000000000000000",
  "arch": "x86_64",
  "machine": "perf-drill",
  "gpu": null,
  "fixture": "bench_10mb.csv",
  "metrics": {
    "parse_ms": 130.0,
    "rss_peak_mb": 200.0,
    "ipc_mbps": 40.0,
    "first_paint_ms": 500.0,
    "drag_fps_p95": 45.0
  },
  "thresholds_pass": [
    true,
    true,
    true,
    true
  ]
}
```
```
