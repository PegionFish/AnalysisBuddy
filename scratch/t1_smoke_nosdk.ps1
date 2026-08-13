# T1 no-SDK negative check: smoke must FAIL when bundled SDK dir is missing (P1 regression)
$ErrorActionPreference = 'Stop'
function Write-Marker([string]$line) { Write-Output $line }
$utf8Bom = [System.Text.UTF8Encoding]::new($true)

$content = Get-Content -Raw -LiteralPath 'scripts\bundle-zip.ps1'
$s = $content.IndexOf('function Invoke-DemoToolSmoke')
$e = $content.IndexOf('# ---------- 主流程 ----------')
Invoke-Expression $content.Substring($s, $e - $s)

$tmp = Join-Path $env:TEMP 't1-nosdk-99999'   # dir with no analysisbuddy/ inside
$fixture = Join-Path (Get-Location) 'plugins\demo-tool\tests\fixtures\small_txt.log'
try {
    Invoke-DemoToolSmoke $tmp $fixture
    Write-Output 'NOSDK_NOT_CAUGHT'
    exit 1
} catch {
    Write-Output ('NOSDK_CAUGHT: ' + $_.Exception.Message)
    exit 0
}
