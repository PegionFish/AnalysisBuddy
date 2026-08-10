#Requires -Version 5.1
# scripts/arm64-smoke.ps1 —— ARM64 冒烟编排（P3-06 / PLAN.md §5 Phase 3 / PLAN.md §8 风险 1）
#
# 冒烟用例最小集（对齐 P3-06 任务卡，不追求 e2e 覆盖）：
#   1) 宿主启动且主窗口出现 —— CLI 代理：ab-app --smoke-host 全流程 ALL GREEN
#      （headless/CI 无法断言 GUI 窗口时以 CLI 冒烟为准；-GuiCheck 开启时追加 GUI 主窗口
#      探测，探测失败不判用例失败，仅记录——本机/CI 无窗口会话属预期）
#   2) 便携 plugins/ 发现 builtin-csv —— host-runtime.md §1.1 Portable 源静态断言：
#      <exe 目录>/plugins/builtin-csv/plugin.json 存在且 manifest id == 目录名、
#      match.extensions 含 csv、entry command 可解析到文件；目录缺失时按 ZIP 布局从仓库根暂存
#   3) 导入 fixture → parse 完成 → run_query 非空 —— ab-app --smoke-pipeline：
#      import OK + query OK（点数 > 0）；-Fixture 存在性 + 头列对齐断言
#   4) key_values 非空 —— 解析 --smoke-host 输出 key_values OK (N entries)，N > 0
#   5) 退出后无孤儿插件进程 —— protocol.md §9 第 5 条：Get-Process 断言
#      builtin-csv / demo-tool / mock-plugin 进程数 == 0（mock-plugin 为 CLI 冒烟实际拉起者）
#
# 档位判定（与 scripts/ci-arm64.ps1 输出约定对齐）：
#   native          ：宿主 exe 为 ARM64 且本机为 ARM64 —— 自动执行 5 用例
#   cross           ：宿主 exe 为 ARM64 但本机非 ARM64 —— 产物不可执行，转人工冒烟（P3-06）
#   check-fallback  ：无 ab-app.exe 产物（仅 cargo check 达标）—— 转人工冒烟（P3-06）
#   x64-sanity      ：宿主 exe 为 x64 —— 本地双架构冒烟验证（自动执行 5 用例）
#
# 输出约定（本地与 CI 通用，marker 供机器消费）：
#   ARM64_SMOKE_MODE=<tier>                  命中档位
#   ARM64_SMOKE_RESULT=<pass|fail|manual>    总体结论
#   ARM64_SMOKE_CASE_<1..5>=<pass|fail|na>   逐用例结论（na = 档位不可执行）
#   ARM64_SMOKE_SUMMARY=<json>               { mode, result, host_exe, cases[], pre_orphans }
#   退出码：0 = pass/manual；1 = fail
#   CI 环境（GITHUB_ACTIONS=true）追加 GITHUB_OUTPUT 的 smoke_mode / smoke_result。
#
# 用法：
#   ./scripts/arm64-smoke.ps1                                             # 自动解析产物与档位
#   ./scripts/arm64-smoke.ps1 -Fixture tests/fixtures/small_with_header.csv
#   ./scripts/arm64-smoke.ps1 -HostBinary target\debug\ab-app.exe -GuiCheck

param(
    [string]$HostBinary = '',
    [string]$Fixture = 'tests/fixtures/small_with_header.csv',
    [switch]$GuiCheck
)

$ErrorActionPreference = 'Stop'
$isActions = $env:GITHUB_ACTIONS -eq 'true'
$script:HostExe = ''
$script:GuiEnabled = $GuiCheck.IsPresent
$script:SmokeHostOut = ''
$script:SmokePipelineOut = ''
$script:PreOrphans = @{ Total = 0; Per = @{} }

function Write-Marker([string]$line) {
    Write-Output $line
}

function Resolve-RepoRoot {
    $dir = $PSScriptRoot
    for ($i = 0; $i -lt 6; $i++) {
        if (Test-Path -LiteralPath (Join-Path $dir '.git')) { return $dir }
        $dir = Split-Path $dir -Parent
        if (-not $dir) { break }
    }
    throw 'repo root not found from scripts/'
}

# 读 PE 头 Machine 字段（0x014c=x86, 0x8664=AMD64, 0xaa64=ARM64）。
function Get-PEMachine([string]$path) {
    $fs = [System.IO.File]::OpenRead($path)
    try {
        $br = New-Object System.IO.BinaryReader($fs)
        [void]$fs.Seek(0x3C, [System.IO.SeekOrigin]::Begin)
        $peOffset = $br.ReadInt32()
        if ($peOffset -le 0 -or $peOffset -gt 4096) { return 0 }
        [void]$fs.Seek($peOffset + 4, [System.IO.SeekOrigin]::Begin)
        return $br.ReadUInt16()
    } finally {
        $fs.Dispose()
    }
}

function Test-NativeArch {
    $arch = $env:PROCESSOR_ARCHITECTURE
    $w6432 = $env:PROCESSOR_ARCHITEW6432
    $runnerArch = $env:RUNNER_ARCH
    return ($arch -eq 'ARM64' -or $w6432 -eq 'ARM64' -or $runnerArch -eq 'ARM64')
}

function Resolve-HostBinary {
    if ($HostBinary) {
        if (-not (Test-Path -LiteralPath $HostBinary)) { throw "HostBinary not found: $HostBinary" }
        return (Resolve-Path -LiteralPath $HostBinary).Path
    }
    $root = Resolve-RepoRoot
    $candidates = @(
        (Join-Path $root 'target\aarch64-pc-windows-msvc\release\ab-app.exe'),
        (Join-Path $root 'target\aarch64-pc-windows-msvc\debug\ab-app.exe'),
        (Join-Path $root 'target\x86_64-pc-windows-msvc\release\ab-app.exe'),
        (Join-Path $root 'target\release\ab-app.exe'),
        (Join-Path $root 'target\debug\ab-app.exe')
    )
    # 多候选命中时取最新构建：显式 --target 目录可能残留 P3-01 之前的旧产物
    # （无 --smoke-host 开关，会被误当 GUI 启动导致冒烟挂起），按 LastWriteTime 取新避旧。
    $found = @($candidates | Where-Object { Test-Path -LiteralPath $_ })
    if ($found.Count -eq 0) { return $null }
    return ($found | Sort-Object -Property @{ Expression = { (Get-Item -LiteralPath $_).LastWriteTime } } -Descending | Select-Object -First 1)
}

function Invoke-App([string]$exe, [string[]]$argList, [string]$tag, [int]$timeoutSec = 300) {
    # 用 System.Diagnostics.Process 直管：Start-Process -PassThru（无 -Wait）读不到
    # ExitCode（PS 5.1 实测），本实现支持超时强杀 + 同步读 stdout/stderr。
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.Arguments = $argList -join ' '
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $proc = New-Object System.Diagnostics.Process
    $proc.StartInfo = $psi
    if (-not $proc.Start()) { throw "process start failed: $exe" }
    $outTask = $proc.StandardOutput.ReadToEndAsync()
    $errTask = $proc.StandardError.ReadToEndAsync()
    $timedOut = -not $proc.WaitForExit($timeoutSec * 1000)
    if ($timedOut) {
        # 防御：产物为旧构建（无 --smoke-host 开关）时会被当作 GUI 拉起而永不退出；
        # 强杀后其子进程（mock-plugin 等）按协议 §9-5 随 stdin EOF 自退。
        try { $proc.Kill() } catch {}
        $proc.WaitForExit(5000) | Out-Null
    }
    $outText = ''
    $errText = ''
    try { $outText = $outTask.Result } catch {}
    try { $errText = $errTask.Result } catch {}
    $exitCode = $null
    if (-not $timedOut) { $exitCode = $proc.ExitCode }
    return @{ ExitCode = $exitCode; TimedOut = $timedOut; Stdout = $outText; Stderr = $errText }
}

function Tail-Lines([string]$text, [int]$count) {
    $lines = @($text -split "`r?`n" | Where-Object { $_ -ne '' })
    $start = [Math]::Max(0, $lines.Count - $count)
    return ($lines[$start..($lines.Count - 1)] -join ' | ')
}

function Get-OrphanCounts {
    $names = @('builtin-csv', 'demo-tool', 'mock-plugin')
    $per = @{}
    $total = 0
    foreach ($n in $names) {
        $procs = @(Get-Process -Name $n -ErrorAction SilentlyContinue)
        $per[$n] = $procs.Count
        $total += $procs.Count
    }
    return @{ Total = $total; Per = $per }
}

function Test-GuiWindow([string]$exe) {
    if (-not $script:GuiEnabled) { return $null }
    $p = Start-Process -FilePath $exe -PassThru
    $found = $false
    for ($i = 0; $i -lt 60; $i++) {
        Start-Sleep -Milliseconds 500
        if ($p.HasExited) { break }
        try { if ($p.MainWindowHandle -ne 0) { $found = $true; break } } catch {}
    }
    if ($found) {
        $null = $p.CloseMainWindow()
        try { $null = $p.WaitForExit(10000) } catch {}
    }
    if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
    return $found
}

# CASE 1：宿主启动（CLI 代理 --smoke-host 全流程），可选 GUI 主窗口探测。
function Invoke-Case1 {
    $r = Invoke-App $script:HostExe @('--smoke-host') 'host'
    $script:SmokeHostOut = $r.Stdout
    $pass = ($r.ExitCode -eq 0) -and ($r.Stdout -match 'ALL GREEN')
    $guiNote = ''
    $gui = Test-GuiWindow $script:HostExe
    if ($gui -ne $null) { $guiNote = "gui_window=$gui" }
    $timeoutNote = ''
    if ($r.TimedOut) { $timeoutNote = 'TIMEOUT(进程未退出，疑似旧构建无 --smoke-host 开关被当 GUI 拉起，已强杀)' }
    $errNote = ''
    if ($r.Stderr) { $errNote = " stderr=[$(Tail-Lines $r.Stderr 3)]" }
    $detail = "exit=$($r.ExitCode) $timeoutNote $guiNote$errNote tail=[$(Tail-Lines $r.Stdout 4)]"
    return @{ Pass = $pass; Detail = $detail }
}

# CASE 2：便携 plugins/ 发现 builtin-csv（host-runtime.md §1.1 Portable 源静态断言）。
function Invoke-Case2 {
    $exeDir = Split-Path $script:HostExe -Parent
    $portable = Join-Path $exeDir 'plugins'
    $staged = $false
    if (-not (Test-Path -LiteralPath $portable)) {
        $root = Resolve-RepoRoot
        $repoPlugins = Join-Path $root 'plugins'
        if (Test-Path -LiteralPath $repoPlugins) {
            Copy-Item -LiteralPath $repoPlugins -Destination $portable -Recurse -Force
            $staged = $true
        }
    }
    if (-not (Test-Path -LiteralPath $portable)) {
        return @{ Pass = $false; Detail = 'portable plugins/ 目录不存在且无法按 ZIP 布局暂存' }
    }
    $pluginDir = Join-Path $portable 'builtin-csv'
    $manifestPath = Join-Path $pluginDir 'plugin.json'
    if (-not (Test-Path -LiteralPath $manifestPath)) {
        return @{ Pass = $false; Detail = "builtin-csv/plugin.json 缺失: $manifestPath" }
    }
    $m = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
    $ok = $true
    $problems = @()
    if ($m.id -ne 'builtin-csv') {
        $ok = $false
        $problems += "manifest id='$($m.id)' 与目录名不符"
    }
    $extOk = $false
    foreach ($e in @($m.match.extensions)) { if ($e -eq 'csv') { $extOk = $true } }
    if (-not $extOk) { $ok = $false; $problems += 'match.extensions 不含 csv' }
    if (-not $m.entry -or -not $m.entry.command) {
        $ok = $false
        $problems += 'entry.command 缺失'
    } else {
        $entry = Join-Path $pluginDir $m.entry.command
        if (-not (Test-Path -LiteralPath $entry)) {
            $ok = $false
            $problems += "entry command 不可解析: $($m.entry.command)"
        }
    }
    $demoOk = Test-Path -LiteralPath (Join-Path $portable 'demo-tool\plugin.json')
    $src = if ($staged) { 'staged(zip-layout)' } else { 'existing' }
    $detail = "portable=$src manifest id=$($m.id) v$($m.version) demo-tool=$demoOk"
    if ($problems.Count -gt 0) { $detail += '; ' + ($problems -join '; ') }
    return @{ Pass = $ok; Detail = $detail }
}

# CASE 3：导入 fixture → parse → run_query 非空（--smoke-pipeline 代理 + fixture 断言）。
# 已知缺陷（x64/ARM64 通用，非本卡引入）：HostSessionAdapter::parse_stream 在 parse 响应
# 到达时直接 forward.abort()，与通知扇出（mpsc 1024）排队中的 RecordBatch 存在调度竞态——
# 偶发 records_total mismatch（declared 3, received 0）。冒烟脚本对该签名失败重试一次，
# 重试事实经 ARM64_SMOKE_SUMMARY 上报；根因闭环由后续修复卡承接（见 checklist 备注）。
function Invoke-Case3 {
    $fixture = $Fixture
    if (-not [System.IO.Path]::IsPathRooted($fixture)) {
        $fixture = Join-Path (Resolve-RepoRoot) $fixture
    }
    if (-not (Test-Path -LiteralPath $fixture)) {
        return @{ Pass = $false; Detail = "fixture 不存在: $fixture"; Retried = $false }
    }
    $head = Get-Content -LiteralPath $fixture -TotalCount 1 -Encoding UTF8
    $cols = @($head -split ',')
    $hasTs = $cols -contains 'timestamp'
    $hasFps = $cols -contains 'fps'
    $hasFrameMs = $cols -contains 'frame_ms'
    $r = Invoke-App $script:HostExe @('--smoke-pipeline') 'pipeline'
    $script:SmokePipelineOut = $r.Stdout
    $retried = $false
    if ($r.Stderr -match 'records_total mismatch') {
        # 已知竞态签名：重试一次（冒烟级容错，证据保留在 summary 与 stderr 摘要）。
        $retried = $true
        $r = Invoke-App $script:HostExe @('--smoke-pipeline') 'pipeline'
        $script:SmokePipelineOut = $r.Stdout
    }
    $mm = [regex]::Match($r.Stdout, 'query OK \((\d+) points')
    $points = 0
    if ($mm.Success) { $points = [int]$mm.Groups[1].Value }
    $pass = ($r.ExitCode -eq 0) -and ($r.Stdout -match 'import OK') -and `
        ($r.Stdout -match 'ALL GREEN') -and ($points -gt 0)
    $timeoutNote = ''
    if ($r.TimedOut) { $timeoutNote = 'TIMEOUT ' }
    $retryNote = ''
    if ($retried) { $retryNote = 'RETRIED(known-race) ' }
    $errNote = ''
    if ($r.Stderr) { $errNote = " stderr=[$(Tail-Lines $r.Stderr 3)]" }
    $detail = "$timeoutNote$retryNote" + "fixture=[$hasTs timestamp,$hasFps fps,$hasFrameMs frame_ms] pipeline_exit=$($r.ExitCode) points=$points$errNote"
    if (-not $pass) { $detail += " tail=[$(Tail-Lines $r.Stdout 4)]" }
    return @{ Pass = $pass; Detail = $detail; Retried = $retried }
}

# CASE 4：key_values 非空（解析 --smoke-host 输出）。
function Invoke-Case4 {
    $mm = [regex]::Match($script:SmokeHostOut, 'key_values OK \((\d+) entries\)')
    if (-not $mm.Success) {
        return @{ Pass = $false; Detail = '--smoke-host 输出无 key_values OK 断言行（依赖 case 1）' }
    }
    $n = [int]$mm.Groups[1].Value
    return @{ Pass = ($n -gt 0); Detail = "key_values entries=$n" }
}

# CASE 5：退出后无孤儿插件进程（protocol.md §9 第 5 条）。
function Invoke-Case5 {
    $post = Get-OrphanCounts
    $pass = ($post.Total -eq 0)
    $detail = "orphans builtin-csv=$($post.Per['builtin-csv']) demo-tool=$($post.Per['demo-tool']) " +
        "mock-plugin=$($post.Per['mock-plugin']) (pre_total=$($script:PreOrphans.Total))"
    return @{ Pass = $pass; Detail = $detail }
}

$script:HostExe = Resolve-HostBinary
$mode = 'check-fallback'
$executable = $false
if ($script:HostExe) {
    $machine = Get-PEMachine $script:HostExe
    if ($machine -eq 0xAA64) {
        $executable = Test-NativeArch
        $mode = if ($executable) { 'native' } else { 'cross' }
    } elseif ($machine -eq 0x8664 -or $machine -eq 0x014C) {
        $mode = 'x64-sanity'
        $executable = $true
    }
}

$script:PreOrphans = Get-OrphanCounts

$labels = @(
    '宿主启动（--smoke-host 代理）',
    '便携 plugins/ 发现 builtin-csv',
    '导入→parse→run_query 非空',
    'key_values 非空',
    '退出后无孤儿进程'
)
$cases = @()
if ($executable) {
    $cases += Invoke-Case1
    $cases += Invoke-Case2
    $cases += Invoke-Case3
    $cases += Invoke-Case4
    $cases += Invoke-Case5
    $fails = @($cases | Where-Object { -not $_.Pass })
    $result = if ($fails.Count -gt 0) { 'fail' } else { 'pass' }
} else {
    for ($i = 0; $i -lt 5; $i++) {
        $cases += @{ Pass = $null; Detail = '档位不可执行（产物非本机架构或无产物）' }
    }
    $result = 'manual'
}

for ($i = 0; $i -lt 5; $i++) {
    $v = 'pass'
    if ($cases[$i].Pass -eq $null) { $v = 'na' }
    elseif (-not $cases[$i].Pass) { $v = 'fail' }
    Write-Output ("CASE $($i + 1) [$($labels[$i])]: $v — " + $cases[$i].Detail)
}

Write-Marker "ARM64_SMOKE_MODE=$mode"
Write-Marker "ARM64_SMOKE_RESULT=$result"
for ($i = 0; $i -lt 5; $i++) {
    $v = 'pass'
    if ($cases[$i].Pass -eq $null) { $v = 'na' }
    elseif (-not $cases[$i].Pass) { $v = 'fail' }
    Write-Marker "ARM64_SMOKE_CASE_$($i + 1)=$v"
}

$retries = 0
$caseObjs = for ($i = 0; $i -lt 5; $i++) {
    if ($cases[$i].Retried) { $retries++ }
    @{ case = "case$($i + 1)"; pass = $cases[$i].Pass; detail = $cases[$i].Detail; retried = [bool]$cases[$i].Retried }
}
$summary = @{
    mode = $mode; result = $result; host_exe = $script:HostExe;
    cases = $caseObjs; retries = $retries; pre_orphans = $script:PreOrphans.Total
} | ConvertTo-Json -Compress -Depth 5
Write-Marker "ARM64_SMOKE_SUMMARY=$summary"

if ($isActions) {
    $utf8 = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::AppendAllText($env:GITHUB_OUTPUT, "smoke_mode=$mode`n", $utf8)
    [System.IO.File]::AppendAllText($env:GITHUB_OUTPUT, "smoke_result=$result`n", $utf8)
}

if ($result -eq 'fail') { exit 1 }
exit 0
