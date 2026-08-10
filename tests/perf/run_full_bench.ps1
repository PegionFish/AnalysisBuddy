# run_full_bench.ps1 —— P3-05 全量性能基准编排（qa-perf.md §4/§5）
#
# 流程：构建 loggen/ab-perf/builtin-csv → 生成三档夹具（loggen 固定 seed，冻结
# SHA-256 比对）→ 离线 harness 单测 → mock 交叉基准（perf_bench，F 路口径）→
# 真实插件基准（perf_real_bench：builtin-csv × bench_10/50/100mb.csv，5 次中位数、
# 预热 1 次丢弃）→ 报告 schema 校验 + 门禁判定（仅已测量门槛）。
#
# 采样纪律（qa-perf.md §4.2/§4.3）：电源「最佳性能」、Defender 排除测试目录、
# release+LTO、每档预热 1 次丢弃、5 次采样取中位数。
#
# 注意：
# 1) 脚本内字符串一律 ASCII（PS 5.1 无 BOM 时按系统代码页误读 UTF-8 中文会
#    吃掉字符串结束引号导致解析失败；中文只出现在注释中，与 tests/scripts/ 既有脚本一致）；
# 2) EAP 用 Continue + 显式 $LASTEXITCODE 检查（PS 5.1 下原生 stderr 一旦被重定向
#    且 EAP=Stop 会变终止性 error record，gen-large-fixtures.ps1 同款约束）。
#
# 用法：  powershell -NoProfile -ExecutionPolicy Bypass -File tests/perf/run_full_bench.ps1
# 参数：  -Fixtures <10mb,50mb,100mb>   需要覆盖的档位（缺省三档全跑；实测恒为三档）
#         -Repeats <N>                  每项采样次数（缺省 5；经 AB_PERF_REPEATS 传 harness）

param(
    [string]$Fixtures = "10mb,50mb,100mb",
    [int]$Repeats = 5
)

$ErrorActionPreference = "Continue"
$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$genDir = Join-Path $repoRoot "tests\.generated"
$reportsDir = Join-Path $repoRoot "tests\perf\reports"
$sw = [System.Diagnostics.Stopwatch]::StartNew()

Write-Host "== P3-05 full perf bench =="
Write-Host "fixtures: $Fixtures  repeats: $Repeats"

# ---- 0. 参数校验 ----
# 注意：PS 会把 `10mb` 解析为数字字面量（10 MB = 10485760），
# `-Fixtures 10mb,50mb,100mb` 实际收到 "10485760 52428800 104857600"；
# 这里把字节数/MB 数归一到档位名，兼容两种写法。
function Normalize-Tier($tok) {
    if ($tok -match '^(\d+)\s*mb$') { return "$($matches[1])mb" }
    if ($tok -match '^\d+$') {
        $v = [long]$tok
        $mb = if ($v -ge 1048576) { $v / 1048576.0 } else { $v }
        if ($mb -eq 10) { return "10mb" }
        if ($mb -eq 50) { return "50mb" }
        if ($mb -eq 100) { return "100mb" }
    }
    return $tok
}
$wanted = @($Fixtures -split '[, ]+' | ForEach-Object { (Normalize-Tier $_.Trim().ToLower()) } | Where-Object { $_ -ne "" })
$valid = @("10mb", "50mb", "100mb")
foreach ($f in $wanted) {
    if ($f -notin $valid) { throw "unknown fixture tier `$f (valid: 10mb,50mb,100mb)" }
}
if ($Repeats -lt 3) { throw "-Repeats must be >= 3 (qa-perf.md sampling discipline)" }

# ---- 1. 夹具生成（loggen 固定 seed + 冻结 SHA-256 比对，qa-perf.md §1.3） ----
$loggen = Join-Path $repoRoot "tools\loggen\target\release\loggen.exe"
if (-not (Test-Path -LiteralPath $loggen)) {
    Write-Host "[bench] building loggen (release)..."
    Push-Location (Join-Path $repoRoot "tools\loggen")
    try { & cargo build --release; if ($LASTEXITCODE -ne 0) { throw "loggen build failed" } }
    finally { Pop-Location }
}
Write-Host "[bench] generating fixtures + hash verification..."
& powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $repoRoot "tests\scripts\gen-large-fixtures.ps1")
if ($LASTEXITCODE -ne 0) { throw "fixture generation / hash verification failed" }

# ---- 2. 确保 builtin-csv release exe ----
$csvBin = Join-Path $repoRoot "plugins\builtin-csv\target\release\builtin-csv.exe"
if (-not (Test-Path -LiteralPath $csvBin)) {
    Write-Host "[bench] building builtin-csv (release)..."
    & cargo build --release --manifest-path (Join-Path $repoRoot "plugins\builtin-csv\Cargo.toml")
    if ($LASTEXITCODE -ne 0) { throw "builtin-csv build failed" }
}

# ---- 3. 构建 ab-perf（release + LTO；workspace profile 冻结） ----
Write-Host "[bench] building ab-perf (release)..."
& cargo build --release -p ab-perf
if ($LASTEXITCODE -ne 0) { throw "ab-perf build failed" }

# ---- 4. 离线 harness 单测（RSS 双路互验等） ----
Write-Host "[bench] offline harness tests..."
& cargo test -p ab-perf --release --test perf_harness
if ($LASTEXITCODE -ne 0) { throw "perf_harness failed" }

# ---- 5. mock 交叉基准（F 路口径；报告为中间产物，第 6 步覆盖） ----
Write-Host "[bench] mock cross-benchmark (perf_bench, echo stream)..."
& cargo test -p ab-perf --release --test perf_bench -- --ignored --nocapture
if ($LASTEXITCODE -ne 0) { throw "perf_bench failed" }

# ---- 6. 真实插件基准（builtin-csv × 三档；入仓报告，须最后运行） ----
Write-Host "[bench] real-plugin benchmark (builtin-csv x 10/50/100MB)..."
$env:AB_PERF_REPEATS = "$Repeats"
try {
    & cargo test -p ab-perf --release --test perf_real_bench -- --ignored --nocapture
    if ($LASTEXITCODE -ne 0) { throw "perf_real_bench failed" }
}
finally {
    Remove-Item Env:AB_PERF_REPEATS -ErrorAction SilentlyContinue
}

# ---- 7. 报告校验（schema 冻结 + 门禁判定，语义同 perf-smoke.yml Gate step） ----
$report = Get-ChildItem (Join-Path $reportsDir "perf-report-*.json") |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $report) { throw "no perf report artifact found" }
$rep = Get-Content $report.FullName -Raw | ConvertFrom-Json
$frozen = @("git_sha", "arch", "metrics", "fixture", "thresholds_pass", "machine")
$missing = $frozen | Where-Object { -not ($rep.PSObject.Properties.Name -contains $_) }
if ($missing) { throw "report missing frozen fields: $($missing -join ',')" }
$metricFrozen = @("parse_ms", "rss_peak_mb", "ipc_mbps", "first_paint_ms", "drag_fps_p95")
$missingM = $metricFrozen | Where-Object { -not ($rep.metrics.PSObject.Properties.Name -contains $_) }
if ($missingM) { throw "report missing frozen metric fields: $($missingM -join ',')" }
if ($rep.thresholds_pass.Count -ne 4) { throw "thresholds_pass length must be 4" }

$perfId = @(1, 2, 4, 3)
$measured = @(
    ($null -ne $rep.metrics.parse_ms),
    ($null -ne $rep.metrics.rss_peak_mb),
    ($null -ne $rep.metrics.ipc_mbps),
    ($null -ne $rep.metrics.drag_fps_p95)
)
$failed = @()
for ($i = 0; $i -lt $rep.thresholds_pass.Count; $i++) {
    if (-not $measured[$i]) { continue }
    if (-not $rep.thresholds_pass[$i]) { $failed += $perfId[$i] }
}
if ($failed.Count -gt 0) { throw "PERF-$($failed -join ',') gate failed: $($report.Name)" }

$sw.Stop()
Write-Host "[bench] DONE in $([math]::Round($sw.Elapsed.TotalMinutes,1)) min"
Write-Host "[bench] report: $($report.Name)"
Write-Host "[bench] git_sha: $($rep.git_sha)  arch: $($rep.arch)"
Write-Host "[bench] machine: $($rep.machine)"
Write-Host ("[bench] metrics: parse={0}ms rss={1}MB ipc={2}MB/s first_paint={3} drag_fps={4}" -f `
    $rep.metrics.parse_ms, $rep.metrics.rss_peak_mb, $rep.metrics.ipc_mbps, `
    $rep.metrics.first_paint_ms, $rep.metrics.drag_fps_p95)
Write-Host "[bench] thresholds_pass (PERF-01/02/04/03): $($rep.thresholds_pass -join ',')"
Write-Host "[bench] gate: PASS (PERF-03 unmeasured -> skipped per qa-perf.md)"
exit 0
