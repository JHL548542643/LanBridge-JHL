$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Drawing

function New-Logo([int]$size) {
  $bmp = New-Object System.Drawing.Bitmap $size, $size
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
  $g.Clear([System.Drawing.Color]::Transparent)

  $pad = [int]($size * 0.02)
  $r = [int]($size * 0.24)
  $rect = New-Object System.Drawing.Rectangle $pad, $pad, ($size - 2*$pad), ($size - 2*$pad)
  $path = New-Object System.Drawing.Drawing2D.GraphicsPath
  $d = $r * 2
  $path.AddArc($rect.X, $rect.Y, $d, $d, 180, 90)
  $path.AddArc($rect.Right - $d, $rect.Y, $d, $d, 270, 90)
  $path.AddArc($rect.Right - $d, $rect.Bottom - $d, $d, $d, 0, 90)
  $path.AddArc($rect.X, $rect.Bottom - $d, $d, $d, 90, 90)
  $path.CloseFigure()

  $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush($rect, [System.Drawing.Color]::FromArgb(255,124,58,237), [System.Drawing.Color]::FromArgb(255,76,29,149), 45.0)
  $g.FillPath($brush, $path)

  # two arrows: right (top) and left (bottom)
  $white = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::White)
  $w = [single]($size * 0.085)
  $len = [single]($size * 0.40)
  $head = [single]($size * 0.13)
  $cx = [single]($size / 2.0)
  $gap = [single]($size * 0.115)

  # top arrow ->
  $y1 = [single]($cx - $gap)
  $x0 = [single]($cx - $len / 2)
  $x1 = [single]($cx + $len / 2)
  $g.FillRectangle($white, $x0, ($y1 - $w/2), ($x1 - $x0 - $head*0.6), $w)
  $pts1 = @(
    (New-Object System.Drawing.PointF(($x1 - $head), ($y1 - $head*0.75))),
    (New-Object System.Drawing.PointF($x1, $y1)),
    (New-Object System.Drawing.PointF(($x1 - $head), ($y1 + $head*0.75)))
  )
  $g.FillPolygon($white, $pts1)

  # bottom arrow <-
  $y2 = [single]($cx + $gap)
  $g.FillRectangle($white, ($x0 + $head*0.6), ($y2 - $w/2), ($x1 - $x0 - $head*0.6), $w)
  $pts2 = @(
    (New-Object System.Drawing.PointF(($x0 + $head), ($y2 - $head*0.75))),
    (New-Object System.Drawing.PointF($x0, $y2)),
    (New-Object System.Drawing.PointF(($x0 + $head), ($y2 + $head*0.75)))
  )
  $g.FillPolygon($white, $pts2)

  $g.Dispose()
  return $bmp
}

$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'assets\branding'
New-Item -ItemType Directory -Force -Path $out | Out-Null
$big = New-Logo 1024
$big.Save((Join-Path $out 'logo.png'), [System.Drawing.Imaging.ImageFormat]::Png)

$sizes = @(16,24,32,48,64,128,256)
$pngs = @{}
foreach ($s in $sizes) {
  $b = New-Object System.Drawing.Bitmap $s, $s
  $g = [System.Drawing.Graphics]::FromImage($b)
  $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  $g.DrawImage($big, 0, 0, $s, $s)
  $g.Dispose()
  $ms = New-Object System.IO.MemoryStream
  $b.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
  $pngs[$s] = $ms.ToArray()
  if ($s -eq 32)  { $b.Save((Join-Path $out '32x32.png'), [System.Drawing.Imaging.ImageFormat]::Png) }
  if ($s -eq 128) { $b.Save((Join-Path $out '128x128.png'), [System.Drawing.Imaging.ImageFormat]::Png) }
  if ($s -eq 256) { $b.Save((Join-Path $out 'icon.png'), [System.Drawing.Imaging.ImageFormat]::Png) }
  $b.Dispose()
}

# ICO container with PNG payloads
$ico = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter $ico
$bw.Write([UInt16]0); $bw.Write([UInt16]1); $bw.Write([UInt16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
foreach ($s in $sizes) {
  $data = $pngs[$s]
  $bw.Write([Byte]($(if ($s -ge 256) { 0 } else { $s })))
  $bw.Write([Byte]($(if ($s -ge 256) { 0 } else { $s })))
  $bw.Write([Byte]0); $bw.Write([Byte]0)
  $bw.Write([UInt16]1); $bw.Write([UInt16]32)
  $bw.Write([UInt32]$data.Length)
  $bw.Write([UInt32]$offset)
  $offset += $data.Length
}
foreach ($s in $sizes) { $bw.Write($pngs[$s]) }
$bw.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $out 'icon.ico'), $ico.ToArray())
$ico.Dispose()

# copy into both shells
$winIcons = Join-Path $root 'apps\win-shell\src-tauri\icons'
Copy-Item (Join-Path $out 'icon.ico') (Join-Path $winIcons 'icon.ico') -Force
Copy-Item (Join-Path $out 'icon.png') (Join-Path $winIcons 'icon.png') -Force
Copy-Item (Join-Path $out '32x32.png') (Join-Path $winIcons '32x32.png') -Force
Copy-Item (Join-Path $out '128x128.png') (Join-Path $winIcons '128x128.png') -Force
$ohos = New-Object System.Drawing.Bitmap 256, 256
$g2 = [System.Drawing.Graphics]::FromImage($ohos)
$g2.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
$g2.DrawImage($big, 0, 0, 256, 256)
$g2.Dispose()
$ohos.Save((Join-Path $root 'apps\ohos-shell\entry\src\main\resources\base\media\app_icon.png'), [System.Drawing.Imaging.ImageFormat]::Png)
$ohos.Dispose()
$big.Dispose()
Get-ChildItem $out | Select-Object Name,Length | Format-Table -AutoSize | Out-String
