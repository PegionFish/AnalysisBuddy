# rss_probe.ps1 —— PowerShell RSS 采样器（qa-perf.md §4.2 指标 2 双路互验）
#
# 与 Rust 采样器（K32GetProcessMemoryInfo）同口径：`System.Diagnostics.Process.
# WorkingSet64` 每 200ms 采样，取峰值（MB）。供 perf_harness 测试断言双路偏差 ≤5%。
#
# 用法：  powershell -NoProfile -File tests/perf/rss_probe.ps1 -ProcessId <pid> [-Seconds 3] [-IntervalMs 200]
# 输出：  峰值 MB（浮点，一行）

param(
    [Parameter(Mandatory = $true)][int]$ProcessId,
    [double]$Seconds = 3,
    [int]$IntervalMs = 200
)

$p = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
if (-not $p) {
    Write-Output "0"
    exit 0
}

$peak = [int64]0
$deadline = (Get-Date).AddSeconds($Seconds)
while ($p -and -not $p.HasExited -and (Get-Date) -lt $deadline) {
    $p.Refresh()
    if ($p.WorkingSet64 -gt $peak) { $peak = $p.WorkingSet64 }
    Start-Sleep -Milliseconds $IntervalMs
}

[math]::Round($peak / 1MB, 1)
