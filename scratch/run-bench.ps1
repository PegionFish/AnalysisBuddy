<#
.SYNOPSIS
echo falsification prototype throughput matrix: batch in {1000,2000,4000,8000} x 100k records x 3 repeats, median table.

.DESCRIPTION
Run from anywhere under the repo (locates everything via $PSScriptRoot).
Builds the two standalone crates if needed, then drives echo-driver per (batch, repeat),
parses the RESULT line, verifies seq continuity and batch-sum integrity, prints the matrix.
Usage: .\run-bench.ps1 -Records 100000 -Repeats 3
#>
param(
    [int]$Records = 100000,
    [int]$Repeats = 3,
    [int[]]$Batch = @(1000, 2000, 4000, 8000)
)

# Note: do NOT use $ErrorActionPreference = 'Stop' here -- PS 5.1 treats native
# stderr output as a terminating error under Stop, aborting the matrix loop.
$ErrorActionPreference = 'Continue'

$root = $PSScriptRoot
$pluginDir = Join-Path $root 'echo-plugin'
$driverDir = Join-Path $root 'echo-driver'
$pluginExe = Join-Path $pluginDir 'target\release\echo-plugin.exe'
$driverExe = Join-Path $driverDir 'target\release\echo-driver.exe'

if (-not (Test-Path $pluginExe)) {
    Write-Host "building $pluginDir (release)..."
    cargo build --release --manifest-path (Join-Path $pluginDir 'Cargo.toml') | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'plugin build failed' }
}
if (-not (Test-Path $driverExe)) {
    Write-Host "building $driverDir (release)..."
    cargo build --release --manifest-path (Join-Path $driverDir 'Cargo.toml') | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'driver build failed' }
}

$runs = New-Object System.Collections.Generic.List[object]
$allOk = $true

foreach ($b in $Batch) {
    $medElapsed = New-Object System.Collections.Generic.List[double]
    $medMbps = New-Object System.Collections.Generic.List[double]
    for ($r = 1; $r -le $Repeats; $r++) {
        $raw = & $driverExe --plugin $pluginExe --batch $b --records $Records 2>$null | Out-String
        if ($LASTEXITCODE -ne 0) { $allOk = $false }
        $resLine = ($raw -split "`n" | Where-Object { $_ -match '^RESULT' } | Select-Object -First 1)
        if (-not $resLine) { throw "no RESULT line for batch=$b repeat=$r (driver exit=$LASTEXITCODE)" }
        $kv = @{}
        foreach ($tok in $resLine -split "`t") {
            if ($tok -match '^(\w+)=(.+)$') { $kv[$matches[1]] = $matches[2] }
        }
        $seqOk = $kv['seq_ok'] -eq 'true'
        $sumOk = $kv['sum_ok'] -eq 'true'
        $recv = [long]$kv['records_total']
        if (-not $seqOk -or -not $sumOk -or $recv -ne $Records) {
            $allOk = $false
            Write-Warning "batch=$b repeat=$r integrity FAIL seq_ok=$($kv['seq_ok']) sum_ok=$($kv['sum_ok']) records_total=$recv"
        }
        $obj = [PSCustomObject]@{
            batch           = [int]$b
            repeat          = $r
            elapsed_ms      = [double]$kv['elapsed_ms']
            mbps            = [double]$kv['mbps']
            max_line_bytes  = [long]$kv['max_line_bytes']
            progress_gap_ms = [double]$kv['progress_gap_ms']
            batch_frames    = [long]$kv['batch_frames']
            progress_frames = [long]$kv['progress_frames']
            seq_ok          = $seqOk
            sum_ok          = $sumOk
        }
        $runs.Add($obj)
        $medElapsed.Add($obj.elapsed_ms)
        $medMbps.Add($obj.mbps)
    }
    $medElapsedSorted = @($medElapsed | Sort-Object)
    $medMbpsSorted = @($medMbps | Sort-Object)
    $runs.Add([PSCustomObject]@{
        batch = [int]$b; repeat = 'MEDIAN'
        elapsed_ms = $medElapsedSorted[[math]::Floor($medElapsed.Count / 2)]
        mbps = $medMbpsSorted[[math]::Floor($medMbps.Count / 2)]
        max_line_bytes = 0; progress_gap_ms = 0; batch_frames = 0; progress_frames = 0
        seq_ok = $true; sum_ok = $true
    })
}

Write-Host ''
Write-Host "== echo throughput matrix (records=$Records, repeats=$Repeats) =="
$runs | Format-Table -AutoSize batch, repeat, elapsed_ms, mbps, max_line_bytes, progress_gap_ms, batch_frames, seq_ok, sum_ok

Write-Host '== median summary =='
$runs | Where-Object { $_.repeat -eq 'MEDIAN' } | Select-Object batch, elapsed_ms, mbps | Format-Table -AutoSize

if ($allOk) {
    Write-Host "INTEGRITY PASS: all runs seq continuous (from 0) and sum(batch lens) == records_total == $Records"
} else {
    Write-Host 'INTEGRITY FAIL: inconsistent runs detected, see WARNINGs above'
    exit 1
}
