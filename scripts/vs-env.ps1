#Requires -Version 5.1
# scripts/vs-env.ps1 —— MSVC 交叉工具链探测 / VsDevCmd 环境导入（P0-03 / P4-01 共用）
#
# 供 ci-arm64.ps1 / bundle-zip.ps1 点源复用（避免两份拷贝漂移，Fix C2 提取）：
#   Test-CrossToolchain ：探测 x64 主机是否具备 ARM64 交叉链接工具（Hostx64\arm64\link.exe）
#   Import-VsDevEnv     ：经 VsDevCmd.bat 导入指定架构构建环境（进程级环境变量）
#
# 用法（调用方在 param 块后点源）：
#   . (Join-Path $PSScriptRoot 'vs-env.ps1')

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
    # VS 18+ 使用 -arch=arm64 -host_arch=amd64；旧版（VS 2019/2022）用 amd64_arm64 复合写法。
    # 逐种尝试，捕获输出用于失败诊断。
    $attempts = @(
        "-arch=$arch -host_arch=amd64 -no_logo",
        '-arch=amd64_' + $arch + ' -no_logo'
    )
    $lastErr = ''
    foreach ($argsLine in $attempts) {
        $envLines = & cmd.exe /d /s /c "`"$vsDevCmd`" $argsLine && set"
        if ($LASTEXITCODE -eq 0) {
            $imported = 0
            foreach ($line in $envLines) {
                if ($line -match '^([^=]+)=(.*)$') {
                    [Environment]::SetEnvironmentVariable($matches[1], $matches[2], 'Process')
                    $imported++
                }
            }
            if ($imported -eq 0) { throw 'VsDevCmd env import produced no variables' }
            return
        }
        $lastErr = ($envLines | Select-String -Pattern 'ERROR|error' | Select-Object -First 3 | ForEach-Object { $_.ToString().Trim() }) -join '; '
    }
    throw "VsDevCmd.bat failed: $lastErr"
}
