# Renders assets\icon.svg / icon-small.svg to PNGs with headless Edge and packs
# assets\lottery-miner.ico (PNG-compressed entries, Windows Vista+).
# Run from the repo root:  powershell -ExecutionPolicy Bypass -File assets\make-icons.ps1
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$edge = "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
$assets = (Resolve-Path "$PSScriptRoot").Path
$work = Join-Path $env:TEMP "lm-icon-render"
New-Item -ItemType Directory -Force $work | Out-Null

function Render-Svg($svg, $png, $w, $h) {
    $url = "file:///" + ($svg -replace '\\', '/')
    $args = @("--headless=new", "--disable-gpu", "--hide-scrollbars",
              "--user-data-dir=$work\edge-profile", "--window-size=$w,$h",
              "--default-background-color=00000000", "--screenshot=$png", $url)
    Start-Process -FilePath $edge -ArgumentList $args -Wait -WindowStyle Hidden | Out-Null
    if (-not (Test-Path $png)) { throw "Edge did not render $svg" }
}

function Resize-Png($src, $dst, $size) {
    $img = [System.Drawing.Image]::FromFile($src)
    $bmp = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)
    $g.DrawImage($img, 0, 0, $size, $size)
    $g.Dispose(); $img.Dispose()
    $bmp.Save($dst, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
}

$big = Join-Path $work "icon-512.png"
$small = Join-Path $work "icon-small-512.png"
Render-Svg (Join-Path $assets "icon.svg") $big 512 512
Render-Svg (Join-Path $assets "icon-small.svg") $small 512 512

# Small sizes use the simplified drawing; 48 px and up use the full one.
$sizes = 16, 20, 24, 32, 40, 48, 64, 96, 128, 256
$entries = @()
foreach ($s in $sizes) {
    $src = if ($s -le 32) { $small } else { $big }
    $png = Join-Path $work "icon-$s.png"
    Resize-Png $src $png $s
    $entries += , @($s, [System.IO.File]::ReadAllBytes($png))
}
Resize-Png $big (Join-Path $assets "icon-256.png") 256
Copy-Item (Join-Path $work "icon-512.png") (Join-Path $assets "icon-512.png") -Force

# ICO: ICONDIR, one ICONDIRENTRY per size, then the PNG bytes.
$ms = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter $ms
$bw.Write([UInt16]0); $bw.Write([UInt16]1); $bw.Write([UInt16]$entries.Count)
$offset = 6 + 16 * $entries.Count
foreach ($e in $entries) {
    $s = $e[0]; $data = $e[1]
    $dim = if ($s -ge 256) { 0 } else { $s }
    $bw.Write([byte]$dim); $bw.Write([byte]$dim); $bw.Write([byte]0); $bw.Write([byte]0)
    $bw.Write([UInt16]1); $bw.Write([UInt16]32)
    $bw.Write([UInt32]$data.Length); $bw.Write([UInt32]$offset)
    $offset += $data.Length
}
foreach ($e in $entries) { $bw.Write([byte[]]$e[1]) }
$bw.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $assets "lottery-miner.ico"), $ms.ToArray())
Write-Output ("wrote lottery-miner.ico with sizes " + ($sizes -join ", "))
