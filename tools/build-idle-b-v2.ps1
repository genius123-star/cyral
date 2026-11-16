param(
    [Parameter(Mandatory = $true)][string]$FirstSource,
    [Parameter(Mandatory = $true)][string]$SecondSource,
    [Parameter(Mandatory = $true)][string]$OutputPath
)

Add-Type -AssemblyName System.Drawing

function Remove-EdgeWhite {
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
        if ($pixel.R -lt 242 -or $pixel.G -lt 242 -or $pixel.B -lt 242) { continue }
        $Bitmap.SetPixel($point.X, $point.Y, [System.Drawing.Color]::Transparent)
        $queue.Enqueue([System.Drawing.Point]::new($point.X - 1, $point.Y))
        $queue.Enqueue([System.Drawing.Point]::new($point.X + 1, $point.Y))
        $queue.Enqueue([System.Drawing.Point]::new($point.X, $point.Y - 1))
        $queue.Enqueue([System.Drawing.Point]::new($point.X, $point.Y + 1))
    }
}

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
    if ($right -lt $left) { throw "未找到人物像素" }
    [System.Drawing.Rectangle]::FromLTRB($left, $top, $right + 1, $bottom + 1)
}

function Get-Character {
    param([string]$SourcePath, [int]$Half)
    $source = [System.Drawing.Bitmap]::FromFile($SourcePath)
    try {
        $halfWidth = [int]($source.Width / 2)
        $crop = New-Object System.Drawing.Bitmap $halfWidth, $source.Height, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        try {
            $graphics = [System.Drawing.Graphics]::FromImage($crop)
            try {
                $graphics.DrawImage($source, (New-Object System.Drawing.Rectangle 0, 0, $halfWidth, $source.Height), (New-Object System.Drawing.Rectangle ($Half * $halfWidth), 0, $halfWidth, $source.Height), [System.Drawing.GraphicsUnit]::Pixel)
            } finally { $graphics.Dispose() }
            Remove-EdgeWhite $crop
            $bounds = Get-OpaqueBounds $crop
            $character = New-Object System.Drawing.Bitmap $bounds.Width, $bounds.Height, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
            $graphics = [System.Drawing.Graphics]::FromImage($character)
            try { $graphics.DrawImage($crop, (New-Object System.Drawing.Rectangle 0, 0, $bounds.Width, $bounds.Height), $bounds, [System.Drawing.GraphicsUnit]::Pixel) } finally { $graphics.Dispose() }
            return $character
        } finally { $crop.Dispose() }
    } finally { $source.Dispose() }
}

$characters = @(
    (Get-Character $FirstSource 0), (Get-Character $FirstSource 1),
    (Get-Character $SecondSource 0), (Get-Character $SecondSource 1)
)
try {
    $sheet = New-Object System.Drawing.Bitmap 720, 226, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    try {
        $graphics = [System.Drawing.Graphics]::FromImage($sheet)
        try {
            $graphics.Clear([System.Drawing.Color]::Transparent)
            $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
            $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
            foreach ($index in 0..3) {
                $character = $characters[$index]
                $scale = [Math]::Min(172.0 / $character.Width, 206.0 / $character.Height)
                $width = [int][Math]::Round($character.Width * $scale)
                $height = [int][Math]::Round($character.Height * $scale)
                $x = $index * 180 + [int][Math]::Round((180 - $width) / 2.0)
                $y = 216 - $height
                $graphics.DrawImage($character, (New-Object System.Drawing.Rectangle $x, $y, $width, $height))
            }
        } finally { $graphics.Dispose() }
        $sheet.Save($OutputPath, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally { $sheet.Dispose() }
} finally {
    foreach ($character in $characters) { $character.Dispose() }
}
