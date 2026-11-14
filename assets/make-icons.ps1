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
    # Wrap the SVG in a page that stretches it to exactly the window size.
    # (Loading the SVG directly draws it at its own 256 px size in the
    # top-left corner of a bigger window.)
    $src = "file:///" + ($svg -replace '\\', '/')
    $page = Join-Path $work ("render-" + [IO.Path]::GetFileNameWithoutExtension($svg) + ".html")
    $html = "<!doctype html><html><body style='margin:0;padding:0;overflow:hidden;background:transparent'>" +
            "<img src='$src' style='display:block;width:${w}px;height:${h}px'></body></html>"
    [IO.File]::WriteAllText($page, $html, (New-Object Text.UTF8Encoding($false)))
    if (Test-Path $png) { Remove-Item $png }
    $url = "file:///" + ($page -replace '\\', '/')
    $args = @("--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-device-scale-factor=1",
              "--user-data-dir=$work\edge-profile", "--window-size=$w,$h",
              "--default-background-color=00000000", "--screenshot=$png", $url)
    Start-Process -FilePath $edge -ArgumentList $args -Wait -WindowStyle Hidden | Out-Null
    if (-not (Test-Path $png)) { throw "Edge did not render $svg" }
}

# Fails loudly if the opaque area is not centered (more than 1 px off) or
# does not fill most of the image.
function Assert-Centered($png) {
    $b = [System.Drawing.Bitmap]::FromFile($png)
    try {
        $minx = $b.Width; $miny = $b.Height; $maxx = -1; $maxy = -1
        for ($y = 0; $y -lt $b.Height; $y++) {
            for ($x = 0; $x -lt $b.Width; $x++) {
                if ($b.GetPixel($x, $y).A -gt 40) {
                    if ($x -lt $minx) { $minx = $x }; if ($x -gt $maxx) { $maxx = $x }
                    if ($y -lt $miny) { $miny = $y }; if ($y -gt $maxy) { $maxy = $y }
                }
            }
        }
        $left = $minx; $right = $b.Width - 1 - $maxx; $top = $miny; $bottom = $b.Height - 1 - $maxy
        $fill = ($maxx - $minx + 1) / $b.Width
        if ([math]::Abs($left - $right) -gt 1 -or [math]::Abs($top - $bottom) -gt 1 -or $fill -lt 0.85) {
            throw "$png is off-center: gaps L$left R$right T$top B$bottom, fills $([math]::Round($fill * 100))%"
        }
        Write-Output ("{0}: centered (gaps L{1} R{2} T{3} B{4}, fills {5}%)" -f (Split-Path $png -Leaf), $left, $right, $top, $bottom, [math]::Round($fill * 100))
    } finally { $b.Dispose() }
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
Assert-Centered $big
Assert-Centered $small

# Small sizes use the simplified drawing; 48 px and up use the full one.
$sizes = 16, 20, 24, 32, 40, 48, 64, 96, 128, 256
$entries = @()
foreach ($s in $sizes) {
    $src = if ($s -le 32) { $small } else { $big }
    $png = Join-Path $work "icon-$s.png"
    Resize-Png $src $png $s
    Assert-Centered $png
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
