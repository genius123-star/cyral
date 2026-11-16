param(
    [Parameter(Mandatory = $true)][string]$FirstSource,
    [Parameter(Mandatory = $true)][string]$SecondSource,
    [Parameter(Mandatory = $true)][string]$OutputPath
)

Add-Type -AssemblyName System.Drawing

function Add-Frame {
    param(
        [System.Drawing.Graphics]$Graphics,
        [System.Drawing.Bitmap]$Source,
        [System.Drawing.Rectangle]$SourceRect,
        [int]$Frame
    )
    $attributes = New-Object System.Drawing.Imaging.ImageAttributes
    try {
        # 原图背景为纯白；仅以纯白做透明键，角色脸部、服装与像素高光不参与任何重绘。
        $attributes.SetColorKey([System.Drawing.Color]::White, [System.Drawing.Color]::White)
        $destination = New-Object System.Drawing.Rectangle ($Frame * 180), 46, 180, 170
        $Graphics.DrawImage($Source, $destination, $SourceRect.X, $SourceRect.Y, $SourceRect.Width, $SourceRect.Height, [System.Drawing.GraphicsUnit]::Pixel, $attributes)
    } finally { $attributes.Dispose() }
}

$first = [System.Drawing.Bitmap]::FromFile($FirstSource)
$second = [System.Drawing.Bitmap]::FromFile($SecondSource)
try {
    $sheet = New-Object System.Drawing.Bitmap 720, 226, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    try {
        $graphics = [System.Drawing.Graphics]::FromImage($sheet)
        try {
            $graphics.Clear([System.Drawing.Color]::Transparent)
            $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
            $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
            Add-Frame $graphics $first  (New-Object System.Drawing.Rectangle 0,   170, 728, 690) 0
            Add-Frame $graphics $first  (New-Object System.Drawing.Rectangle 728, 170, 728, 690) 1
            Add-Frame $graphics $second (New-Object System.Drawing.Rectangle 0,   180, 728, 720) 2
            Add-Frame $graphics $second (New-Object System.Drawing.Rectangle 728, 180, 728, 720) 3
        } finally { $graphics.Dispose() }
        $sheet.Save($OutputPath, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally { $sheet.Dispose() }
} finally {
    $first.Dispose()
    $second.Dispose()
}
