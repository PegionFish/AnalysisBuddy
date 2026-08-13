# T1 harness: validate Invoke-DemoToolSmoke extracted verbatim from scripts/bundle-zip.ps1
$ErrorActionPreference = 'Stop'
function Write-Marker([string]$line) { Write-Output $line }
$utf8Bom = [System.Text.UTF8Encoding]::new($true)

$content = Get-Content -Raw -LiteralPath 'scripts\bundle-zip.ps1'
$start = $content.IndexOf('function Invoke-DemoToolSmoke')
$end = $content.IndexOf('# ---------- 主流程 ----------')
if ($start -lt 0 -or $end -lt 0 -or $end -le $start) { throw "cannot extract function block ($start..$end)" }
$fn = $content.Substring($start, $end - $start)
Invoke-Expression $fn

$pluginDir = Join-Path (Get-Location) 'plugins\demo-tool'
$fixture = Join-Path $pluginDir 'tests\fixtures\small_txt.log'
Invoke-DemoToolSmoke $pluginDir $fixture
Write-Output "HARNESS_EXIT=$LASTEXITCODE"
