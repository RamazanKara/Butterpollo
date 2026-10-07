# Renders butterpollo.ico, the mark the console uses as its favicon (a white
# "B" on a rounded burnt-orange square), at the sizes Windows asks for in
# Explorer, Start, the taskbar and the notification area.
#   pwsh -File rust/assets/make-icon.ps1
Add-Type -AssemblyName System.Drawing
$sizes = 16, 20, 24, 32, 40, 48, 64, 96, 128, 256
$frames = foreach ($size in $sizes) {
    $bitmap = [Drawing.Bitmap]::new($size, $size, [Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [Drawing.Graphics]::FromImage($bitmap)
    $g.SmoothingMode = 'AntiAlias'
    $g.TextRenderingHint = 'AntiAliasGridFit'
    $g.Clear([Drawing.Color]::Transparent)
    # Same proportions as the 32-unit favicon: corner radius 8, glyph 24.
    $scale = $size / 32
    $radius = 8 * $scale
    $shape = [Drawing.Drawing2D.GraphicsPath]::new()
    $shape.AddArc(0, 0, 2 * $radius, 2 * $radius, 180, 90)
    $shape.AddArc($size - 2 * $radius, 0, 2 * $radius, 2 * $radius, 270, 90)
    $shape.AddArc($size - 2 * $radius, $size - 2 * $radius, 2 * $radius, 2 * $radius, 0, 90)
    $shape.AddArc(0, $size - 2 * $radius, 2 * $radius, 2 * $radius, 90, 90)
    $shape.CloseFigure()
    $g.FillPath([Drawing.SolidBrush]::new([Drawing.Color]::FromArgb(0xad, 0x4b, 0x10)), $shape)
    $font = [Drawing.Font]::new('Segoe UI', [float](22 * $scale), [Drawing.FontStyle]::Bold, [Drawing.GraphicsUnit]::Pixel)
    $format = [Drawing.StringFormat]::new()
    $format.Alignment = 'Center'
    $format.LineAlignment = 'Center'
    $box = [Drawing.RectangleF]::new(0, [float](0.5 * $scale), $size, $size)
    $g.DrawString('B', $font, [Drawing.Brushes]::White, $box, $format)
    $g.Dispose()
    $stream = [IO.MemoryStream]::new()
    $bitmap.Save($stream, [Drawing.Imaging.ImageFormat]::Png)
    $bitmap.Dispose()
    , @($size, $stream.ToArray())
}
# ICO container with PNG frames (supported since Windows Vista).
$out = [IO.MemoryStream]::new()
$w = [IO.BinaryWriter]::new($out)
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$frames.Count)
$offset = 6 + 16 * $frames.Count
foreach ($frame in $frames) {
    $size, $png = $frame
    $w.Write([byte]($size % 256)); $w.Write([byte]($size % 256))
    $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]$png.Length); $w.Write([uint32]$offset)
    $offset += $png.Length
}
foreach ($frame in $frames) { $w.Write($frame[1]) }
$w.Flush()
[IO.File]::WriteAllBytes((Join-Path $PSScriptRoot 'butterpollo.ico'), $out.ToArray())
