param(
    [string]$AssetDirectory = "public/assets/characters/cyral"
)

Add-Type -AssemblyName System.Drawing

function Remove-EdgeWhite {
    param([System.Drawing.Bitmap]$Bitmap)
    $width = $Bitmap.Width
    $height = $Bitmap.Height
    $seen = New-Object 'bool[]' ($width * $height)
    $queue = New-Object 'System.Collections.Generic.Queue[System.Drawing.Point]'
    for ($x = 0; $x -lt $width; $x++) {
        $queue.Enqueue([System.Drawing.Point]::new($x, 0))
        $queue.Enqueue([System.Drawing.Point]::new($x, $height - 1))
    }
    for ($y = 0; $y -lt $height; $y++) {
        $queue.Enqueue([System.Drawing.Point]::new(0, $y))
        $queue.Enqueue([System.Drawing.Point]::new($width - 1, $y))
    }
    while ($queue.Count -gt 0) {
        $point = $queue.Dequeue()
        if ($point.X -lt 0 -or $point.X -ge $width -or $point.Y -lt 0 -or $point.Y -ge $height) { continue }
        $index = $point.Y * $width + $point.X
        if ($seen[$index]) { continue }
        $seen[$index] = $true
        $pixel = $Bitmap.GetPixel($point.X, $point.Y)
        if ($pixel.R -lt 242 -or $pixel.G -lt 242 -or $pixel.B -lt 242) { continue }
        $Bitmap.SetPixel($point.X, $point.Y, [System.Drawing.Color]::Transparent)
        $queue.Enqueue([System.Drawing.Point]::new($point.X - 1, $point.Y))
        $queue.Enqueue([System.Drawing.Point]::new($point.X + 1, $point.Y))
        $queue.Enqueue([System.Drawing.Point]::new($point.X, $point.Y - 1))
        $queue.Enqueue([System.Drawing.Point]::new($point.X, $point.Y + 1))
    }
}

function Convert-ToRuntimeStrip {
    param(
        [string]$SourcePath,
        [string]$OutputPath,
        [int]$Frames,
        [int]$SourceRows = 1,
        [int]$SourceRow = 0
    )
    if (Test-Path -LiteralPath $OutputPath) { return }
    $source = [System.Drawing.Bitmap]::FromFile($SourcePath)
    try {
        $sourceFrameWidth = [int]($source.Width / $Frames)
        $sourceFrameHeight = [int]($source.Height / $SourceRows)
        $strip = [System.Drawing.Bitmap]::new(180 * $Frames, 226, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        try {
            $stripGraphics = [System.Drawing.Graphics]::FromImage($strip)
            try { $stripGraphics.Clear([System.Drawing.Color]::Transparent) } finally { $stripGraphics.Dispose() }
            for ($index = 0; $index -lt $Frames; $index++) {
                $frame = [System.Drawing.Bitmap]::new(180, 226, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
                try {
                    $graphics = [System.Drawing.Graphics]::FromImage($frame)
                    try {
                        $graphics.Clear([System.Drawing.Color]::White)
                        $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
                        $scale = [Math]::Min(180.0 / $sourceFrameWidth, 226.0 / $sourceFrameHeight)
                        $drawWidth = [int][Math]::Round($sourceFrameWidth * $scale)
                        $drawHeight = [int][Math]::Round($sourceFrameHeight * $scale)
                        $drawX = [int][Math]::Round((180 - $drawWidth) / 2.0)
                        $drawY = 226 - $drawHeight
                        $sourceRect = [System.Drawing.Rectangle]::new($index * $sourceFrameWidth, $SourceRow * $sourceFrameHeight, $sourceFrameWidth, $sourceFrameHeight)
                        $destRect = [System.Drawing.Rectangle]::new($drawX, $drawY, $drawWidth, $drawHeight)
                        $graphics.DrawImage($source, $destRect, $sourceRect, [System.Drawing.GraphicsUnit]::Pixel)
                    } finally { $graphics.Dispose() }
                    Remove-EdgeWhite $frame
                    $stripGraphics = [System.Drawing.Graphics]::FromImage($strip)
                    try { $stripGraphics.DrawImageUnscaled($frame, $index * 180, 0) } finally { $stripGraphics.Dispose() }
                } finally { $frame.Dispose() }
            }
            $strip.Save($OutputPath, [System.Drawing.Imaging.ImageFormat]::Png)
        } finally { $strip.Dispose() }
    } finally { $source.Dispose() }
}

$storyboard = Join-Path $AssetDirectory "preview-compact-transition-storyboard.png"
Convert-ToRuntimeStrip $storyboard (Join-Path $AssetDirectory "exit-row.png") 6 2 0
Convert-ToRuntimeStrip $storyboard (Join-Path $AssetDirectory "enter-row.png") 6 2 1
foreach ($edge in @("left", "right", "bottom", "top")) {
    Convert-ToRuntimeStrip (Join-Path $AssetDirectory "preview-peek-$edge-3f.png") (Join-Path $AssetDirectory "peek-$edge.png") 3
}
