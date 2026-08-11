#Requires -Version 5.1
# scripts/bundle-zip.ps1 —— 纯 ZIP 双架构打包（P4-01 / ipc-ui.md §8 / qa-perf.md §6）
#
# 编排（每架构）：
#   1) cargo build --release --target <triple> -p ab-app（宿主）
#   2) cargo build --release --target <triple>（plugins/builtin-csv，架构相关 exe）
#   3) tauri build --no-bundle（tauri-cli：跑 beforeBuildCommand 出 ui/dist +
#      前端资源嵌入；tauri-cli 2.x Windows 无 zip bundle 目标（--bundles 仅
#      msi/nsis），故 ZIP 由本脚本按 ipc-ui.md §8 布局自行组装）
#   4) 组装：exe + WebView2Loader.dll（webview2-com-sys 自带，x64/arm64 各一）
#      + 便携 plugins/（builtin-csv + demo-tool）+ README-PORTABLE.txt（中英双语）
#   5) Compress-Archive → dist/AnalysisBuddy-{version}-{arch}.zip
#   6) 清单断言：主程序/WebView2Loader.dll/两个插件目录/README 就位，
#      且无 NSIS/MSI/uninstaller 残留（qa-perf.md §6）
#   7) x86_64 档：解压启动冒烟（5s 窗口检查后杀进程；-NoLaunch 跳过）
#
# ARM64 档位优雅降级（用户决策 2026-08-10：本机不做 ARM64 实机测试）：
#   本机无 ARM64 MSVC 链接工具（Hostx64/arm64/link.exe）→ 按 P0-03 降级链走
#   「cargo check 等价」档，标注「ARM64 产物由 CI 产出」并跳过该架构本地打包与
#   解压启动；该档失败不阻塞整体退出码（非致命）。若检测到 ARM64 链接工具则
#   走完整构建档，构建失败再降级。
#
# 输出 marker（ci-arm64.ps1 风格，CI/日志友好）：
#   BUNDLE_ATTEMPT=<arch>:<try|ok|fail[:detail]>    每次尝试一行
#   BUNDLE_MODE=<arch>:<full|ci-only|skipped>       最终档位
#   BUNDLE_ZIP=<arch>:<zip 绝对路径>                 产物（full 档）
#   BUNDLE_SMOKE=<arch>:<ok|skipped|fail[:detail]>  解压启动冒烟结果
#   BUNDLE_SUMMARY=<json>                            { attempts[], zips[] }
#
# 用法：
#   .\scripts\bundle-zip.ps1                          # 默认 x86_64 全档
#   .\scripts\bundle-zip.ps1 -Arch x86_64,aarch64     # 双架构（aarch64 本机降级）
#   .\scripts\bundle-zip.ps1 -Arch x86_64 -NoLaunch   # 自动环境：跳过启动冒烟

param(
    [string]$Arch = 'x86_64',
    [string]$Version = '0.1.0',
    [string]$Dist = 'dist',
    [switch]$NoLaunch
)

# MSVC 交叉工具链探测 / VsDevCmd 环境导入（与 ci-arm64.ps1 同源，见 vs-env.ps1）
. (Join-Path $PSScriptRoot 'vs-env.ps1')

$ErrorActionPreference = 'Stop'
$isActions = $env:GITHUB_ACTIONS -eq 'true'

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$appDir = Join-Path $repoRoot 'core\ab-app'
$builtinCsvDir = Join-Path $repoRoot 'plugins\builtin-csv'
$demoToolDir = Join-Path $repoRoot 'plugins\demo-tool'
$distDir = Join-Path $repoRoot $Dist
$utf8Bom = [System.Text.UTF8Encoding]::new($true)

function Write-Marker([string]$line) {
    Write-Output $line
}

function Invoke-Checked([string]$what, [string]$argLine, [string]$workdir) {
    Write-Marker "BUNDLE_CMD: $what -- $argLine"
    Push-Location $workdir
    try {
        & cargo @($argLine -split ' ')
        if ($LASTEXITCODE -ne 0) { throw "cargo $argLine failed (exit $LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
}

function Resolve-TauriCli {
    # PS 5.1 下 `& native 2>$null` 与 EAP=Stop 组合会把 stderr 转为终止性错误，
    # 故经 cmd.exe 捕获 stderr 文本与退出码。优先 cargo tauri（本地已装）；
    # 否则 npx @tauri-apps/cli（预编译二进制，npm 缓存后离线可用）。
    $check = & cmd.exe /d /c "cargo tauri --version 2>&1"
    if ($LASTEXITCODE -eq 0 -and ($check -match 'tauri-cli')) { return 'cargo tauri' }
    $check = & cmd.exe /d /c "npx --yes @tauri-apps/cli@2 --version 2>&1"
    if ($LASTEXITCODE -eq 0) { return 'npx --yes @tauri-apps/cli@2' }
    throw '未找到 tauri-cli：请安装 cargo tauri 或确保 npm/npx 可用'
}

function Get-WebView2LoaderDll([string]$arch) {
    # WebView2Loader.dll 随 webview2-com-sys crate 分发（tauri 运行时自带；
    # tauri 官方 bundler 同样取自该路径）。取最高版本 crate 的对应架构子目录。
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
    $registry = Join-Path $cargoHome 'registry\src'
    if (-not (Test-Path -LiteralPath $registry)) {
        throw "未找到 cargo registry src: $registry（缺 webview2-com-sys crate 源码）"
    }
    $crates = @(Get-ChildItem -LiteralPath $registry -Directory -Recurse -Depth 2 -Filter 'webview2-com-sys-*' -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending)
    if ($crates.Count -eq 0) { throw '未找到 webview2-com-sys crate' }
    $archDir = if ($arch -eq 'aarch64') { 'arm64' } else { 'x64' }
    $dll = Join-Path $crates[0].FullName "$archDir\WebView2Loader.dll"
    if (-not (Test-Path -LiteralPath $dll)) { throw "webview2-com-sys 缺 $archDir\WebView2Loader.dll" }
    return $dll
}

function Invoke-TauriBundle([string]$triple, [string]$tauriCli) {
    # --no-bundle：tauri-cli 2.x Windows 的 --bundles 仅支持 msi/nsis（无 zip），
    # ZIP 由脚本按 ipc-ui.md §8 布局组装（bundle.targets 配置亦为 []）。
    Push-Location $appDir
    try {
        if ($tauriCli -eq 'cargo tauri') {
            & cmd.exe /d /c "cargo tauri build --no-bundle --target $triple 2>&1"
        } else {
            & cmd.exe /d /c "npx --yes @tauri-apps/cli@2 build --no-bundle --target $triple 2>&1"
        }
        if ($LASTEXITCODE -ne 0) { throw "tauri build --no-bundle failed (exit $LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
}

function Write-ReadmePortable([string]$dir) {
    $readme = @'
AnalysisBuddy 便携版说明（中英双语 / Bilingual）
================================================

1. 便携模式（默认）
   解压即用：把本 ZIP 解压到任意目录（含空格/中文路径均可），双击 AnalysisBuddy.exe 启动。
   本应用无安装器：升级 = 下载新版本 ZIP 覆盖解压（plugins/ 与
   %APPDATA%\AnalysisBuddy\plugins 私有插件不受覆盖影响）。
   插件目录：exe 同级的 plugins/（内置 builtin-csv、demo-tool，优先级最高）
   与 %APPDATA%\AnalysisBuddy\plugins（私有插件）。

2. 运行时依赖：Microsoft Edge WebView2
   若启动时弹出「WebView2 运行时缺失」提示：打开
   https://developer.microsoft.com/microsoft-edge/webview2/
   下载 Evergreen Standalone Installer 并安装，然后重新启动本程序。

3. 内置插件
   - builtin-csv：CSV/TSV 通用解析器（Rust 静态分发，零运行时依赖）
   - demo-tool：演示工具解析器（Python 脚本，需要 Python 3.10+，
     按系统 PATH 的 python / py launcher 约定查找解释器）

----------------------------------------------------------------

1. Portable mode (default)
   Extract this ZIP to any directory (paths with spaces or non-ASCII
   characters are fine), then double-click AnalysisBuddy.exe.
   This app has NO installer: upgrade = download the new ZIP and extract
   over the old one (plugins/ and %APPDATA%\AnalysisBuddy\plugins private
   plugins are not affected).
   Plugin directories: plugins/ next to the exe (built-in builtin-csv and
   demo-tool, highest priority) and %APPDATA%\AnalysisBuddy\plugins (private).

2. Runtime dependency: Microsoft Edge WebView2
   If a "WebView2 Runtime missing" message box appears at startup, open
   https://developer.microsoft.com/microsoft-edge/webview2/
   and install the Evergreen Standalone Installer, then restart the app.

3. Built-in plugins
   - builtin-csv: CSV/TSV universal parser (Rust, static distribution, zero runtime deps)
   - demo-tool: demo parser (Python scripts; requires Python 3.10+,
     interpreter resolved from PATH via python / py launcher)
'@
    [System.IO.File]::WriteAllText(
        (Join-Path $dir 'README-PORTABLE.txt'), $readme + "`r`n", $utf8Bom)
}

function Test-ArchiveManifest([string]$zipPath) {
    # 清单断言（qa-perf.md §6）：布局逐条对齐 + 无 NSIS/MSI/uninstaller 残留。
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::OpenRead($zipPath)
    try {
        $names = @($zip.Entries | ForEach-Object { $_.FullName -replace '\\', '/' })
        $mustHave = @(
            'AnalysisBuddy/AnalysisBuddy.exe',
            'AnalysisBuddy/WebView2Loader.dll',
            'AnalysisBuddy/plugins/builtin-csv/plugin.json',
            'AnalysisBuddy/plugins/builtin-csv/target/release/builtin-csv.exe',
            'AnalysisBuddy/plugins/demo-tool/plugin.json',
            'AnalysisBuddy/plugins/demo-tool/main.py',
            'AnalysisBuddy/README-PORTABLE.txt'
        )
        foreach ($entry in $mustHave) {
            if ($names -notcontains $entry) {
                throw "清单断言失败：缺 $entry"
            }
        }
        # 无安装器残留（无 uninstaller、无 .msi/.nsi、无 bootstrapper）
        $residue = @($names | Where-Object {
            $_ -match '(?i)uninstall|\.msi$|\.nsi$|bootstrapper|installer\.exe$'
        })
        if ($residue.Count -gt 0) {
            throw "清单断言失败：发现安装器残留: $($residue -join ', ')"
        }
        # 架构断言（Fix I2）：主程序 PE machine 必须与目标架构一致，且体积 > 1 MB
        # （防止截断/空 exe 或 x64 产物误标 aarch64 混过仅查文件名的清单断言）。
        $exeEntry = $zip.Entries | Where-Object { ($_.FullName -replace '\\', '/') -eq 'AnalysisBuddy/AnalysisBuddy.exe' } | Select-Object -First 1
        if ($null -eq $exeEntry) {
            throw '清单断言失败：缺 AnalysisBuddy/AnalysisBuddy.exe（无法执行架构断言）'
        }
        if ($exeEntry.Length -le 1048576) {
            throw "清单断言失败：主程序体积异常（$($exeEntry.Length) 字节 ≤ 1 MB），疑似截断/空文件"
        }
        # 条目流为 Deflate 压缩流，不支持 Seek（set_Position 抛 NotSupportedException），
        # 故整段读入内存后按偏移解析 PE 头（e_lfanew @ 0x3C，machine @ e_lfanew+4）。
        $exeStream = $exeEntry.Open()
        try {
            $exeMs = [System.IO.MemoryStream]::new()
            $exeStream.CopyTo($exeMs)
        } finally {
            $exeStream.Dispose()
        }
        $exeBytes = $exeMs.ToArray()
        $eLfanew = [System.BitConverter]::ToUInt32($exeBytes, 0x3C)
        if ($eLfanew + 6 -gt $exeBytes.Length) {
            throw '清单断言失败：主程序 PE 头损坏（e_lfanew 越界）'
        }
        $machine = [System.BitConverter]::ToUInt16($exeBytes, $eLfanew + 4)
        $expectedMachine = if ($arch -eq 'aarch64') { 0xAA64 } else { 0x8664 }
        if ($machine -ne $expectedMachine) {
            throw "清单断言失败：PE machine 0x$($machine.ToString('X4')) 与 $arch 期望 0x$($expectedMachine.ToString('X4')) 不符"
        }
        Write-Marker "BUNDLE_MANIFEST=$zipPath`:ok ($($names.Count) entries)"
        return $names
    } finally {
        $zip.Dispose()
    }
}

function Invoke-UnpackSmoke([string]$arch, [string]$zipPath) {
    # 解压即启动（qa-perf.md §6 验收项 1 预检）：5s 内出主窗口后杀进程。
    $smokeDir = Join-Path $distDir "smoke-$arch"
    if (Test-Path -LiteralPath $smokeDir) { Remove-Item -LiteralPath $smokeDir -Recurse -Force }
    Expand-Archive -LiteralPath $zipPath -DestinationPath $smokeDir
    $exe = Join-Path $smokeDir 'AnalysisBuddy\AnalysisBuddy.exe'
    if (-not (Test-Path -LiteralPath $exe)) { throw "解压后主程序缺失: $exe" }
    $proc = Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe) -PassThru
    Start-Sleep -Seconds 5
    $proc.Refresh()
    if ($proc.HasExited) {
        throw "启动冒烟失败：进程 5s 内退出（exit $($proc.ExitCode)）"
    }
    $hwnd = $proc.MainWindowHandle
    if ($hwnd -eq 0) {
        throw '启动冒烟失败：进程存活但 5s 内无主窗口句柄'
    }
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    Write-Marker "BUNDLE_SMOKE=$arch`:ok (window=$hwnd)"
    return $smokeDir
}

# ---------- 主流程 ----------
$archList = @($Arch -split ',' | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$attempts = @()
$zips = @()
$failed = $false

foreach ($arch in $archList) {
    $triple = switch ($arch) {
        'x86_64' { 'x86_64-pc-windows-msvc' }
        'aarch64' { 'aarch64-pc-windows-msvc' }
        default { throw "未知架构: $arch（支持 x86_64 / aarch64）" }
    }
    $zipOut = Join-Path $distDir "AnalysisBuddy-$Version-$arch.zip"
    Write-Marker "BUNDLE_ATTEMPT=$arch`:try"
    try {
        # 版本漂移检查（Fix I3）：ZIP 命名取 -Version（CI 传 tag 版本），exe 版本资源取
        # core/ab-app/tauri.conf.json 的 version 字段，二者不一致 → 发布产物内外版本不符。
        $tauriConf = Get-Content -Raw -LiteralPath (Join-Path $appDir 'tauri.conf.json') | ConvertFrom-Json
        if ($tauriConf.version -ne $Version) {
            throw "版本漂移：-Version $Version 与 core/ab-app/tauri.conf.json 的 version $($tauriConf.version) 不一致（ZIP 命名 vs exe 版本资源）"
        }

        if (-not (Test-Path -LiteralPath $distDir)) {
            New-Item -ItemType Directory -Path $distDir -Force | Out-Null
        }

        if ($arch -eq 'aarch64' -and -not (Test-CrossToolchain)) {
            # P0-03 降级链：无 ARM64 链接工具 → cargo check 等价档（用户决策：本机不做 ARM64 实机测试）
            Invoke-Checked 'check-fallback' "check --target $triple -p ab-app" $repoRoot
            $attempts += @{ arch = $arch; tier = 'ci-only'; result = 'ok'; detail = '无 ARM64 链接工具，走 cargo check 等价档' }
            Write-Marker "BUNDLE_MODE=$arch`:ci-only"
            Write-Marker "BUNDLE_ATTEMPT=$arch`:ok"
            Write-Marker "BUNDLE_NOTE=$arch`:ARM64 产物由 CI 产出（本机无 Hostx64/arm64 link.exe）"
            continue
        }

        # ARM64 交叉链接：VsDevCmd 环境导入是进程级的（ci-arm64.ps1 是另一进程，不残留），
        # 故本进程内先导入 amd64_arm64 链接环境再构建（vs-env.ps1 共享函数；全档必过
        # Test-CrossToolchain 检查，此处再查一次仅作自说明）。
        if ($arch -eq 'aarch64' -and (Test-CrossToolchain)) {
            Import-VsDevEnv 'arm64'
        }

        # 1) tauri build --no-bundle：跑 beforeBuildCommand 出 ui/dist + 前端资源嵌入
        #    （内部即 cargo build --release --target <triple> -p ab-app；tauri-cli 2.x
        #    Windows 无 zip bundle 目标——--bundles 仅 msi/nsis——故 ZIP 由脚本组装）
        $tauriCli = Resolve-TauriCli
        Invoke-TauriBundle $triple $tauriCli
        $releaseDir = Join-Path $repoRoot "target\$triple\release"
        Write-Marker "BUNDLE_RELEASE_DIR=$arch`:$releaseDir"

        # 2) builtin-csv 架构相关 exe（sdk-plugins.md §5.4）
        Invoke-Checked 'build-plugin' "build --release --target $triple" $builtinCsvDir

        # 3) 组装便携布局（ipc-ui.md §8 / host-runtime.md §7.1 优先级 0）
        $stage = Join-Path $distDir "stage-$arch"
        if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
        $appStage = Join-Path $stage 'AnalysisBuddy'
        New-Item -ItemType Directory -Path $appStage -Force | Out-Null
        # cargo 产物名为包名 ab-app.exe；交付名为 productName AnalysisBuddy.exe
        Copy-Item -LiteralPath (Join-Path $releaseDir 'ab-app.exe') -Destination (Join-Path $appStage 'AnalysisBuddy.exe')
        Copy-Item -LiteralPath (Get-WebView2LoaderDll $arch) -Destination $appStage
        foreach ($sub in 'resources', 'icons') {
            if (Test-Path -LiteralPath (Join-Path $releaseDir $sub)) {
                Copy-Item -LiteralPath (Join-Path $releaseDir $sub) -Destination $appStage -Recurse
            }
        }
        if (-not (Test-Path -LiteralPath (Join-Path $appStage 'AnalysisBuddy.exe'))) {
            throw 'release 产物缺主程序 AnalysisBuddy.exe'
        }
        if (-not (Test-Path -LiteralPath (Join-Path $appStage 'WebView2Loader.dll'))) {
            throw '组装缺 WebView2Loader.dll'
        }

        $pluginsStage = Join-Path $appStage 'plugins'
        New-Item -ItemType Directory -Path (Join-Path $pluginsStage 'builtin-csv\target\release') -Force | Out-Null
        Copy-Item -LiteralPath (Join-Path $builtinCsvDir 'plugin.json') -Destination (Join-Path $pluginsStage 'builtin-csv\')
        Copy-Item -LiteralPath (Join-Path $builtinCsvDir 'config.json') -Destination (Join-Path $pluginsStage 'builtin-csv\')
        Copy-Item -LiteralPath (Join-Path $builtinCsvDir "target\$triple\release\builtin-csv.exe") -Destination (Join-Path $pluginsStage 'builtin-csv\target\release\')

        New-Item -ItemType Directory -Path (Join-Path $pluginsStage 'demo-tool') -Force | Out-Null
        Copy-Item -LiteralPath (Join-Path $demoToolDir 'main.py') -Destination (Join-Path $pluginsStage 'demo-tool\')
        Copy-Item -LiteralPath (Join-Path $demoToolDir 'parser.py') -Destination (Join-Path $pluginsStage 'demo-tool\')
        Copy-Item -LiteralPath (Join-Path $demoToolDir 'plugin.json') -Destination (Join-Path $pluginsStage 'demo-tool\')

        Write-ReadmePortable $appStage

        # 4) 压缩为交付 ZIP
        if (Test-Path -LiteralPath $zipOut) { Remove-Item -LiteralPath $zipOut -Force }
        Compress-Archive -Path $appStage -DestinationPath $zipOut
        $zips += $zipOut
        Write-Marker "BUNDLE_ZIP=$arch`:$zipOut"

        # 5) 清单断言（marker 输出；断言失败即抛）
        Test-ArchiveManifest $zipOut

        # 6) 解压启动冒烟：仅 x86_64 本机实机（用户决策：ARM64 不做实机测试）
        if ($NoLaunch -or $arch -ne 'x86_64') {
            Write-Marker "BUNDLE_SMOKE=$arch`:skipped"
        } else {
            Invoke-UnpackSmoke $arch $zipOut
        }

        $attempts += @{ arch = $arch; tier = 'full'; result = 'ok'; detail = "zip=$zipOut" }
        Write-Marker "BUNDLE_MODE=$arch`:full"
        Write-Marker "BUNDLE_ATTEMPT=$arch`:ok"
    } catch {
        $attempts += @{ arch = $arch; tier = 'full'; result = 'fail'; detail = $_.Exception.Message }
        Write-Marker "BUNDLE_ATTEMPT=$arch`:fail:$($_.Exception.Message)"
        if ($arch -eq 'x86_64') { $failed = $true }
        elseif ($arch -eq 'aarch64') {
            # aarch64 失败非致命：尝试 check 等价档后继续（CI 产出兜底）
            try {
                Invoke-Checked 'check-fallback' "check --target $triple -p ab-app" $repoRoot
                Write-Marker "BUNDLE_MODE=$arch`:ci-only"
                Write-Marker "BUNDLE_NOTE=$arch`:ARM64 产物由 CI 产出（本机构建失败: $($_.Exception.Message)）"
            } catch {
                Write-Marker "BUNDLE_MODE=$arch`:ci-only(fail)"
                Write-Marker "BUNDLE_NOTE=$arch`:ARM64 check 档也失败: $($_.Exception.Message)（CI 兜底产出）"
            }
        }
    }
}

Write-Marker "BUNDLE_SUMMARY=$((@{ attempts = $attempts; zips = $zips } | ConvertTo-Json -Compress -Depth 5))"

if ($failed) {
    Write-Error 'x86_64 打包失败（详见 BUNDLE_ATTEMPT marker）'
    exit 1
}
exit 0
