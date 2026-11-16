param(
    [Parameter(Mandatory = $true)][string]$SourcePath,
    [Parameter(Mandatory = $true)][string]$OutputPath
)

Add-Type -AssemblyName System.Drawing

function Get-OpaqueBounds {
    param([System.Drawing.Bitmap]$Bitmap)
    $left = $Bitmap.Width; $top = $Bitmap.Height; $right = -1; $bottom = -1
    for ($y = 0; $y -lt $Bitmap.Height; $y++) {
        for ($x = 0; $x -lt $Bitmap.Width; $x++) {
            if ($Bitmap.GetPixel($x, $y).A -eq 0) { continue }
            $left = [Math]::Min($left, $x); $top = [Math]::Min($top, $y)
            $right = [Math]::Max($right, $x); $bottom = [Math]::Max($bottom, $y)
        }
    }
    if ($right -lt $left) { throw "第 $Frame 帧没有人物像素" }
    [System.Drawing.Rectangle]::FromLTRB($left, $top, $right + 1, $bottom + 1)
}

function Remove-EdgeNearWhite {
    param([System.Drawing.Bitmap]$Bitmap)
    $seen = New-Object 'bool[]' ($Bitmap.Width * $Bitmap.Height)
    $queue = New-Object 'System.Collections.Generic.Queue[System.Drawing.Point]'
    for ($x = 0; $x -lt $Bitmap.Width; $x++) {
        $queue.Enqueue([System.Drawing.Point]::new($x, 0))
        $queue.Enqueue([System.Drawing.Point]::new($x, $Bitmap.Height - 1))
    }
    for ($y = 0; $y -lt $Bitmap.Height; $y++) {
        $queue.Enqueue([System.Drawing.Point]::new(0, $y))
        $queue.Enqueue([System.Drawing.Point]::new($Bitmap.Width - 1, $y))
    }
    while ($queue.Count -gt 0) {
        $point = $queue.Dequeue()
        if ($point.X -lt 0 -or $point.X -ge $Bitmap.Width -or $point.Y -lt 0 -or $point.Y -ge $Bitmap.Height) { continue }
        $index = $point.Y * $Bitmap.Width + $point.X
        if ($seen[$index]) { continue }
        $seen[$index] = $true
        $pixel = $Bitmap.GetPixel($point.X, $point.Y)
        if ($pixel.A -gt 0 -and ($pixel.R -lt 242 -or $pixel.G -lt 242 -or $pixel.B -lt 242)) { continue }
        $Bitmap.SetPixel($point.X, $point.Y, [System.Drawing.Color]::Transparent)
        $queue.Enqueue([System.Drawing.Point]::new($point.X - 1, $point.Y))
        $queue.Enqueue([System.Drawing.Point]::new($point.X + 1, $point.Y))
        $queue.Enqueue([System.Drawing.Point]::new($point.X, $point.Y - 1))
        $queue.Enqueue([System.Drawing.Point]::new($point.X, $point.Y + 1))
    }
}

$source = [System.Drawing.Bitmap]::FromFile($SourcePath)
try {
    if ($source.Width -ne 720 -or $source.Height -ne 226) { throw "待机帧条必须是 720x226" }
    $sheet = New-Object System.Drawing.Bitmap 720, 226, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    try {
        $graphics = [System.Drawing.Graphics]::FromImage($sheet)
        try {
            $graphics.Clear([System.Drawing.Color]::Transparent)
            $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
            $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
            foreach ($frame in 0..3) {
                $cell = New-Object System.Drawing.Bitmap 180, 226, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
                try {
                    $cellGraphics = [System.Drawing.Graphics]::FromImage($cell)
                    try {
                        $cellGraphics.DrawImage($source, (New-Object System.Drawing.Rectangle 0, 0, 180, 226), (New-Object System.Drawing.Rectangle ($frame * 180), 0, 180, 226), [System.Drawing.GraphicsUnit]::Pixel)
                    } finally { $cellGraphics.Dispose() }
                    Remove-EdgeNearWhite $cell
                    $bounds = Get-OpaqueBounds $cell
                    # 按人物高度统一缩放，避免翼展相同但头身大小不同造成待机跳动。
                    $scale = 160.0 / $bounds.Height
                    $width = [int][Math]::Round($bounds.Width * $scale)
                    $height = [int][Math]::Round($bounds.Height * $scale)
                    $x = $frame * 180 + [int][Math]::Round((180 - $width) / 2.0)
                    $y = 216 - $height
                    $graphics.DrawImage($cell, (New-Object System.Drawing.Rectangle $x, $y, $width, $height), $bounds, [System.Drawing.GraphicsUnit]::Pixel)
                } finally { $cell.Dispose() }
            }
        } finally { $graphics.Dispose() }
        $sheet.Save($OutputPath, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally { $sheet.Dispose() }
} finally { $source.Dispose() }
