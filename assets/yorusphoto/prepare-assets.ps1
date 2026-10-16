param([Parameter(Mandatory = $true)][string]$SourcePath)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Copy-Item -LiteralPath $SourcePath -Destination (Join-Path $PSScriptRoot 'logo.png') -Force
$sourceImage = [System.Drawing.Image]::FromFile((Join-Path $PSScriptRoot 'logo.png'))
$sizes = @(16, 24, 32, 48, 64, 128, 256)
$entries = [System.Collections.Generic.List[object]]::new()
try {
    foreach ($size in $sizes) {
        $bitmap = [System.Drawing.Bitmap]::new($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        $stream = [System.IO.MemoryStream]::new()
        $imageAttributes = [System.Drawing.Imaging.ImageAttributes]::new()
        try {
            $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
            $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
            $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
            # Mirror the edge samples so bicubic interpolation preserves an opaque source edge.
            $imageAttributes.SetWrapMode([System.Drawing.Drawing2D.WrapMode]::TileFlipXY)
            $destinationRect = [System.Drawing.Rectangle]::new(0, 0, $size, $size)
            $graphics.DrawImage($sourceImage, $destinationRect, 0, 0, $sourceImage.Width, $sourceImage.Height, [System.Drawing.GraphicsUnit]::Pixel, $imageAttributes)
            $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
            $bytes = $stream.ToArray()
            $entries.Add(@{ Size = $size; Bytes = $bytes })
            if ($size -eq 128 -or $size -eq 256) {
                [System.IO.File]::WriteAllBytes((Join-Path $PSScriptRoot "icon-$size.png"), $bytes)
            }
        } finally {
            $stream.Dispose()
            $imageAttributes.Dispose()
            $graphics.Dispose()
            $bitmap.Dispose()
        }
    }
} finally {
    $sourceImage.Dispose()
}
$iconStream = [System.IO.File]::Create((Join-Path $PSScriptRoot 'yorusphoto.ico'))
$writer = [System.IO.BinaryWriter]::new($iconStream)
try {
    $writer.Write([uint16]0)
    $writer.Write([uint16]1)
    $writer.Write([uint16]$entries.Count)
    $offset = [uint32](6 + 16 * $entries.Count)
    foreach ($entry in $entries) {
        $dimension = if ($entry.Size -eq 256) { 0 } else { $entry.Size }
        $writer.Write([byte]$dimension)
        $writer.Write([byte]$dimension)
        $writer.Write([byte]0)
        $writer.Write([byte]0)
        $writer.Write([uint16]1)
        $writer.Write([uint16]32)
        $writer.Write([uint32]$entry.Bytes.Length)
        $writer.Write($offset)
        $offset += $entry.Bytes.Length
    }
    foreach ($entry in $entries) { $writer.Write([byte[]]$entry.Bytes) }
} finally {
    $writer.Dispose()
    $iconStream.Dispose()
}
