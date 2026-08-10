#Requires -Version 5.1
# scripts/verify-zip-manifest.ps1 —— 发布 ZIP 清单断言（P4-02 / qa-perf.md §6）
#
# 断言内容（qa-perf.md §6 末段，与 bundle-zip.ps1 的 Test-ArchiveManifest 同源、
# 供流水线独立复核）：
#   1) 必需文件齐全：AnalysisBuddy.exe / WebView2Loader.dll / README-PORTABLE.txt /
#      plugins/builtin-csv/（plugin.json + 架构相关 exe）/ plugins/demo-tool/
#      （plugin.json + main.py）
#   2) 布局洁净：全部条目位于 AnalysisBuddy/ 前缀下（无散落顶层文件）
#   3) 无安装器残留：无 uninstaller / .msi / .nsi / bootstrapper / installer.exe
#   4) 资源目录（resources/ icons/）：存在则核对，缺省视为「资源内嵌于 exe」不判失败
#
# 输出约定（bundle-zip.ps1 风格 marker，CI/日志友好）：
#   VERIFY_CHECK=<pass|fail|na|note>: <条目> [详情]    逐项清单（checklist 行）
#   VERIFY_MANIFEST=<zip>:<ok|fail[:原因数]>           汇总行
# 退出码：任一 fail 断言 → 1（流水线阻塞）；全部通过 → 0。
#
# 用法：
#   .\scripts\verify-zip-manifest.ps1 -Zip dist/AnalysisBuddy-0.1.0-x86_64.zip

param(
    [Parameter(Mandatory = $true)]
    [string]$Zip
)

$ErrorActionPreference = 'Stop'

function Write-Check([string]$status, [string]$item, [string]$detail = '') {
    $line = "VERIFY_CHECK=$status`: $item"
    if ($detail) { $line += " — $detail" }
    Write-Output $line
}

if (-not (Test-Path -LiteralPath $Zip)) {
    Write-Output "VERIFY_CHECK=fail: ZIP 不存在 — $Zip"
    Write-Output "VERIFY_MANIFEST=$Zip`:fail(1)"
    exit 1
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
# 注意：局部变量不可命名为 $zip（与参数 $Zip 大小写不敏感重名，会被 [string]
# 类型约束强制转回字符串）——故用 $archive。
try {
    $archive = [System.IO.Compression.ZipFile]::OpenRead($Zip)
} catch {
    Write-Output "VERIFY_CHECK=fail: 无法打开 ZIP — $($_.Exception.Message)"
    Write-Output "VERIFY_MANIFEST=$Zip`:fail(open)"
    exit 1
}
try {
    $names = @($archive.Entries | ForEach-Object { $_.FullName -replace '\\', '/' })
} finally {
    $archive.Dispose()
}

$fails = @()
$checks = @(
    @{ item = '主程序 AnalysisBuddy.exe';       path = 'AnalysisBuddy/AnalysisBuddy.exe' }
    @{ item = '运行时库 WebView2Loader.dll';    path = 'AnalysisBuddy/WebView2Loader.dll' }
    @{ item = '便携说明 README-PORTABLE.txt';   path = 'AnalysisBuddy/README-PORTABLE.txt' }
    @{ item = '内置插件 builtin-csv/plugin.json'; path = 'AnalysisBuddy/plugins/builtin-csv/plugin.json' }
    @{ item = '内置插件 builtin-csv 架构 exe';   path = 'AnalysisBuddy/plugins/builtin-csv/target/release/builtin-csv.exe' }
    @{ item = '演示插件 demo-tool/plugin.json';  path = 'AnalysisBuddy/plugins/demo-tool/plugin.json' }
    @{ item = '演示插件 demo-tool/main.py';     path = 'AnalysisBuddy/plugins/demo-tool/main.py' }
)

Write-Output "VERIFY_CHECK=note: 清单断言 — $Zip（共 $($names.Count) 个条目）"

foreach ($check in $checks) {
    if ($names -contains $check.path) {
        Write-Check 'pass' $check.item
    } else {
        Write-Check 'fail' $check.item "缺失 $($check.path)"
        $fails += $check.path
    }
}

foreach ($dir in 'AnalysisBuddy/resources/', 'AnalysisBuddy/icons/') {
    $present = @($names | Where-Object { $_ -like "$dir*" })
    if ($present.Count -gt 0) {
        Write-Check 'pass' "资源目录 $dir" "$($present.Count) 个条目"
    } else {
        Write-Check 'na' "资源目录 $dir" '未随包（资源内嵌于 exe，bundle-zip.ps1 按需拷贝）'
    }
}

# 布局洁净：全部条目须位于 AnalysisBuddy/ 前缀下
$stray = @($names | Where-Object { $_ -notlike 'AnalysisBuddy/*' })
if ($stray.Count -gt 0) {
    Write-Check 'fail' '布局洁净（无散落顶层条目）' ($stray -join ', ')
    $fails += 'stray-top-level'
} else {
    Write-Check 'pass' '布局洁净（全部条目位于 AnalysisBuddy/ 下）'
}

# 无安装器残留（qa-perf.md §6：无 uninstaller / MSI / NSIS / bootstrapper）
$residue = @($names | Where-Object {
    $_ -match '(?i)uninstall|\.msi$|\.nsi$|bootstrapper|installer\.exe$'
})
if ($residue.Count -gt 0) {
    Write-Check 'fail' '无安装器残留（uninstaller/MSI/NSIS/bootstrapper）' ($residue -join ', ')
    $fails += 'installer-residue'
} else {
    Write-Check 'pass' '无安装器残留（uninstaller/MSI/NSIS/bootstrapper）'
}

if ($fails.Count -gt 0) {
    Write-Output "VERIFY_MANIFEST=$Zip`:fail($($fails.Count))"
    # 用 Write-Output 而非 Write-Error：EAP=Stop 下 Write-Error 是终止性错误，
    # 会跳过 exit 1（退出码仍为 1，但显式 exit 更确定）。
    Write-Output "VERIFY_CHECK=fail: ZIP 清单断言失败 $($fails.Count) 项：$($fails -join '; ')"
    exit 1
}

Write-Output "VERIFY_MANIFEST=$Zip`:ok"
Write-Output 'VERIFY_CHECK=note: 全部断言通过，可用于发布。'
exit 0
