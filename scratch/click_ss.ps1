param([int]$SX, [int]$SY)
# screenshot(1568x1014) -> physical screen. Window physical rect (633,281,1928,1118); scale 1.211
$ox = 633; $oy = 281; $k = 0.8258
$x = [int]($ox + $SX * $k)
$y = [int]($oy + $SY * $k)
& "$PSScriptRoot\sendclick.ps1" -X $x -Y $y
Write-Output ("screenshot($SX,$SY) -> physical($x,$y)")
