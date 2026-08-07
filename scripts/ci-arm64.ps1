#Requires -Version 5.1
# scripts/ci-arm64.ps1 —— ARM64 构建三档降级链编排（P0-03 / PLAN.md §8 风险 1）
#
# 档位（逐级失败自动降档，降档事实写入 job summary）：
#   1) native          ：ARM64 原生 runner，cargo build --release + 占位冒烟（cargo test 原生执行）
#   2) cross           ：x64 runner + MSVC amd64_arm64 交叉链接（VsDevCmd.bat 导入环境后构建）
#   3) check-fallback  ：cargo check --target aarch64-pc-windows-msvc 通过即视为构建达标，
#                        summary 标注「ARM64 转人工冒烟」（P3-06 承接）
#
# 能力探测：PROCESSOR_ARCHITECTURE / PROCESSOR_ARCHITEW6432 / RUNNER_ARCH 判定原生档；
#           vswhere 定位含 VC.Tools.ARM64 的 VS 安装并确认 Hostx64\arm64\link.exe 判定交叉档。
# 输出约定（本地与 CI 通用）：
#   ARM64_ATTEMPT=<tier>:<try|ok|fail[:detail]>   每次尝试一行 marker
#   ARM64_MODE=<final tier>                       最终命中档位 marker 行
#   ARM64_SUMMARY=<json>                          { tier, has_exe, attempts[] }
#   CI 环境（GITHUB_ACTIONS=true）额外写 GITHUB_OUTPUT 的 arm64_mode / arm64_has_exe，
#   并追加 job summary Markdown（档位表 + 降档说明）。
#
# 用法：
#   ./scripts/ci-arm64.ps1                          # 自动探测档位
#   ./scripts/ci-arm64.ps1 -Tier cross              # 强制从 cross 档开始（本地预演用）

param(
    [string]$Target = 'aarch64-pc-windows-msvc',
    [ValidateSet('auto', 'native', 'cross', 'check')]
    [string]$Tier = 'auto',
    [string]$Profile = 'release'
)

$ErrorActionPreference = 'Stop'
$isActions = $env:GITHUB_ACTIONS -eq 'true'

function Write-Marker([string]$line) {
    Write-Output $line
}

function Test-NativeRunner {
    $arch = $env:PROCESSOR_ARCHITECTURE
    $w6432 = $env:PROCESSOR_ARCHITEW6432
    $runnerArch = $env:RUNNER_ARCH
    return ($arch -eq 'ARM64' -or $w6432 -eq 'ARM64' -or $runnerArch -eq 'ARM64')
}

function Test-CrossToolchain {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path -LiteralPath $vswhere)) { return $false }
    $vsPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.ARM64 -property installationPath 2>$null
    if (-not $vsPath) { return $false }
    $msvcRoot = Join-Path $vsPath 'VC\Tools\MSVC'
    if (-not (Test-Path -LiteralPath $msvcRoot)) { return $false }
    $vcVer = Get-ChildItem -LiteralPath $msvcRoot -Directory -ErrorAction SilentlyContinue |
        Sort-Object -Property @{ Expression = { try { [version]$_.Name } catch { [version]'0.0' } } } -Descending |
        Select-Object -First 1
    if (-not $vcVer) { return $false }
    $link = Join-Path $vcVer.FullName 'bin\Hostx64\arm64\link.exe'
    return (Test-Path -LiteralPath $link)
}

function Import-VsDevEnv([string]$arch) {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $vsPath = & $vswhere -latest -products * -property installationPath 2>$null
    if (-not $vsPath) { throw 'VS install not found (vswhere)' }
    $vsDevCmd = Join-Path $vsPath 'Common7\Tools\VsDevCmd.bat'
    if (-not (Test-Path -LiteralPath $vsDevCmd)) { throw "VsDevCmd.bat not found: $vsDevCmd" }
    $envLines = & cmd.exe /d /s /c "`"$vsDevCmd`" -arch=$arch -host_arch=amd64 -no_logo && set"
    if ($LASTEXITCODE -ne 0) { throw 'VsDevCmd.bat failed' }
    $imported = 0
    foreach ($line in $envLines) {
        if ($line -match '^([^=]+)=(.*)$') {
            [Environment]::SetEnvironmentVariable($matches[1], $matches[2], 'Process')
            $imported++
        }
    }
    if ($imported -eq 0) { throw 'VsDevCmd env import produced no variables' }
}

function Ensure-TargetInstalled([string]$target) {
    $rustup = Get-Command rustup -ErrorAction SilentlyContinue
    if (-not $rustup) { return }
    $installed = (& rustup target list --installed) -join ';'
    if ($installed -notmatch [regex]::Escape($target)) {
        & rustup target add $target
        if ($LASTEXITCODE -ne 0) { throw "rustup target add $target failed" }
    }
}

function Invoke-Cargo([string]$argLine) {
    & cargo @($argLine -split ' ')
    if ($LASTEXITCODE -ne 0) { throw "cargo $argLine failed (exit $LASTEXITCODE)" }
}

function Invoke-TierNative {
    Ensure-TargetInstalled $Target
    Invoke-Cargo "build --release --target $Target"
    # 占位冒烟：release 测试二进制在 ARM64 原生执行（空壳阶段无真机交互，P3-06 人工冒烟兜底）
    Invoke-Cargo "test --release --target $Target"
}

function Invoke-TierCross {
    Ensure-TargetInstalled $Target
    Import-VsDevEnv 'amd64_arm64'
    Invoke-Cargo "build --release --target $Target"
}

function Invoke-TierCheck {
    Ensure-TargetInstalled $Target
    Invoke-Cargo "check --target $Target"
}

function Write-Result([string]$tier, [array]$attempts) {
    $exe = Join-Path 'target' (Join-Path $Target (Join-Path $Profile 'ab-app.exe'))
    $hasExe = Test-Path -LiteralPath $exe
    Write-Marker "ARM64_MODE=$tier"
    $summary = @{ tier = $tier; has_exe = $hasExe; attempts = $attempts } | ConvertTo-Json -Compress -Depth 5
    Write-Marker "ARM64_SUMMARY=$summary"
    if ($isActions) {
        $utf8 = [System.Text.UTF8Encoding]::new($false)
        $hasExeStr = $hasExe.ToString().ToLowerInvariant()
        [System.IO.File]::AppendAllText($env:GITHUB_OUTPUT, "arm64_mode=$tier`n", $utf8)
        [System.IO.File]::AppendAllText($env:GITHUB_OUTPUT, "arm64_has_exe=$hasExeStr`n", $utf8)
        if ($env:GITHUB_STEP_SUMMARY) {
            $note = switch ($tier) {
                'native' { '原生 ARM64 runner 构建 + 占位冒烟通过。' }
                'cross' { 'x64 runner + MSVC amd64_arm64 交叉链接构建通过。' }
                'check-fallback' { '**ARM64 转人工冒烟**（P3-06 承接）：cargo check 通过，完整链接与实机冒烟后补。' }
            }
            $attemptRows = ($attempts | ForEach-Object { "| $($_.tier) | $($_.result) | $($_.detail) |" }) -join "`n"
            $tierLabel = '`' + $tier + '`'
            $md = @"
## ARM64 构建档位：$tierLabel
$note

| 尝试档位 | 结果 | 说明 |
|----------|------|------|
$attemptRows
"@
            [System.IO.File]::AppendAllText($env:GITHUB_STEP_SUMMARY, $md + "`n", $utf8)
        }
    }
}

$attempts = @()
$tiers = @()
if (Test-NativeRunner) { $tiers += 'native' }
if (Test-CrossToolchain) { $tiers += 'cross' }
$tiers += 'check'
if ($Tier -ne 'auto') {
    $tiers = @($Tier) + ($tiers | Where-Object { $_ -ne $Tier })
}

$final = $null
foreach ($t in $tiers) {
    Write-Marker "ARM64_ATTEMPT=$t`:try"
    try {
        switch ($t) {
            'native' { Invoke-TierNative }
            'cross' { Invoke-TierCross }
            'check' { Invoke-TierCheck }
        }
        $attempts += @{ tier = $t; result = 'ok'; detail = '' }
        Write-Marker "ARM64_ATTEMPT=$t`:ok"
        $final = $t
        break
    } catch {
        $attempts += @{ tier = $t; result = 'fail'; detail = $_.Exception.Message }
        Write-Marker "ARM64_ATTEMPT=$t`:fail:$($_.Exception.Message)"
    }
}

if (-not $final) {
    Write-Error 'ARM64 构建三档全部失败'
    exit 1
}

Write-Result $final $attempts
exit 0
