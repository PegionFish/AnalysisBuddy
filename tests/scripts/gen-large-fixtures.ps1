# gen-large-fixtures.ps1 —— 大档夹具生成 + 哈希比对 + 体积/计时断言（qa-perf.md §2）
#
# 生成 4 档大夹具到 tests/.generated/（gitignore，不入仓），与冻结 SHA-256 比对
# 保证复现（F-01 DoD 确定性验收）；bench_100mb 生成计时 ≤60s（防自身成为
# nightly 瓶颈）。PS 5.1 与 pwsh 7 均兼容。
#
# 用法：  powershell -NoProfile -File tests/scripts/gen-large-fixtures.ps1
# 参数：  -LoggenPath <exe>    覆盖 loggen 路径（缺省 tools/loggen/target/release/loggen.exe）
#         -SkipBuild            不自动构建 loggen

param(
    [string]$LoggenPath = "",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$genDir = Join-Path $repoRoot "tests\.generated"

# ---- 冻结基准（F-01 确定性冻结，勿改：改动必须同步 README 与报告） ----
$frozen = @{
    "bench_10mb.csv"   = @{ rows = 243456;  seed = 10;  size = "auto";  extra = @();          sha256 = "4b8163cdcbf8b144b4b881c4bdaa98c454c0b990fc39ff53e40315164121e79b"; sizeLo = 9.5;  sizeHi = 10.5 }
    "bench_50mb.csv"   = @{ rows = 1217280; seed = 50;  size = "auto";  extra = @();          sha256 = "8d2b3705ba0c9579e6578beb5483ed9aa55c8295e838fcb13e2a971bf9ba6d65"; sizeLo = 47.5; sizeHi = 52.5 }
    "bench_100mb.csv"  = @{ rows = 1217280; seed = 100; size = "100MB"; extra = @("--disorder","0.02"); sha256 = "781537088934160bc0fe80aa7b87f36e7d1e5bb4c9ccffa34593d12dcd972b2d"; sizeLo = 98.0; sizeHi = 102.0 }
    "disorder_20pct.csv" = @{ rows = 5000;  seed = 21;  size = "auto";  extra = @("--disorder","0.2"); sha256 = "ee5e45e05337eb206409ac43db091aecaf3c6ad57b42c91b1ad39bfca6c9c49e"; sizeLo = 0.15; sizeHi = 0.3 }
}

# ---- 定位/构建 loggen ----
if (-not $LoggenPath) {
    $LoggenPath = Join-Path $repoRoot "tools\loggen\target\release\loggen.exe"
}
if (-not (Test-Path -LiteralPath $LoggenPath)) {
    if ($SkipBuild) { throw "loggen not found at $LoggenPath (pass -LoggenPath or drop -SkipBuild)" }
    Write-Host "[gen] building loggen (release)..."
    Push-Location (Join-Path $repoRoot "tools\loggen")
    try { & cargo build --release; if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed" } }
    finally { Pop-Location }
}

New-Item -ItemType Directory -Force -Path $genDir | Out-Null
Write-Host "[gen] output dir: $genDir"

$failures = 0
$totalElapsed = [System.Diagnostics.Stopwatch]::StartNew()
$timings = @()

foreach ($name in $frozen.Keys | Sort-Object) {
    $spec = $frozen[$name]
    $out = Join-Path $genDir $name
    $args = @("--rows", "$($spec.rows)", "--metrics", "3", "--size-target", $spec.size,
              "--format", "csv", "--seed", "$($spec.seed)") + $spec.extra +
            @("-o", $out)
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    # stderr 直通控制台（loggen 的 INFO 报告）。注意：PS 5.1 下对原生 stderr 做
    # 任何重定向都会在 EAP=Stop 时把它变成终止性 error record，故不重定向。
    & $LoggenPath @args
    $sw.Stop()
    if ($LASTEXITCODE -ne 0) { Write-Host "[FAIL] $name loggen exit $LASTEXITCODE"; $failures++ ; continue }

    $file = Get-Item -LiteralPath $out
    $sizeMb = $file.Length / 1MB
    $timings += [PSCustomObject]@{ Name = $name; Sec = [math]::Round($sw.Elapsed.TotalSeconds, 2); Mb = [math]::Round($sizeMb, 2) }

    # 体积断言
    if ($sizeMb -lt $spec.sizeLo -or $sizeMb -gt $spec.sizeHi) {
        Write-Host ("[FAIL] {0} size {1}MB outside [{2},{3}]MB" -f $name, [math]::Round($sizeMb,2), $spec.sizeLo, $spec.sizeHi)
        $failures++
        continue
    }
    # 哈希比对（确定性验收）
    $hash = (Get-FileHash -LiteralPath $out -Algorithm SHA256).Hash.ToLower()
    if ($hash -ne $spec.sha256) {
        Write-Host "[FAIL] $name hash mismatch:`n  got $hash`n  exp $($spec.sha256)"
        $failures++
        continue
    }
    Write-Host ("[ OK ] {0}  {1,7:N0} bytes  {2,5} MB  {3,5}s  {4}" -f $name, $file.Length, [math]::Round($sizeMb,2), [math]::Round($sw.Elapsed.TotalSeconds,2), $hash.Substring(0,16))
}

# 100MB 计时断言（≤60s，F-01 DoD）
$hundred = $timings | Where-Object { $_.Name -eq "bench_100mb.csv" }
if ($hundred -and $hundred.Sec -gt 60.0) {
    Write-Host ("[FAIL] bench_100mb.csv generation took {0}s > 60s" -f $hundred.Sec)
    $failures++
}

$totalElapsed.Stop()
Write-Host "[gen] total $([math]::Round($totalElapsed.Elapsed.TotalSeconds,1))s"
if ($failures -gt 0) {
    Write-Host "[gen] FAILURES: $failures"
    exit 1
}
Write-Host "[gen] all fixtures regenerated and verified"
exit 0
