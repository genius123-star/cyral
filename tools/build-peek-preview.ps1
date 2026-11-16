param(
    [string]$LeftSource,
    [string]$BottomSource,
    [string]$OutputDirectory
)

Add-Type -AssemblyName System.Drawing

function New-PeekSheet {
    param(
        [string]$SourcePath,
        [string]$OutputPath,
        [ValidateSet('left', 'right', 'bottom', 'top')][string]$Direction,
        [bool]$MirrorX = $false,
        [bool]$MirrorY = $false
    )

    $source = [System.Drawing.Bitmap]::FromFile($SourcePath)
    try {
        if ($MirrorX) { $source.RotateFlip([System.Drawing.RotateFlipType]::RotateNoneFlipX) }
        if ($MirrorY) { $source.RotateFlip([System.Drawing.RotateFlipType]::RotateNoneFlipY) }

        $frameWidth = $source.Width
        $frameHeight = $source.Height
        $sheet = New-Object System.Drawing.Bitmap ($frameWidth * 3), $frameHeight, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        try {
            $graphics = [System.Drawing.Graphics]::FromImage($sheet)
            try {
                $graphics.Clear([System.Drawing.Color]::White)
                $graphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
                $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
                $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half

                $progress = @(0.35, 0.68, 1.0)
                for ($index = 0; $index -lt 3; $index++) {
                    $cellX = $index * $frameWidth
                    $graphics.SetClip((New-Object System.Drawing.Rectangle $cellX, 0, $frameWidth, $frameHeight))

                    if ($Direction -eq 'left') {
                        $drawX = $cellX - [int][Math]::Round($frameWidth * (1.0 - $progress[$index]))
                        $drawY = 0
                    } elseif ($Direction -eq 'right') {
                        $drawX = $cellX + [int][Math]::Round($frameWidth * (1.0 - $progress[$index]))
                        $drawY = 0
                    } elseif ($Direction -eq 'bottom') {
                        $drawX = $cellX
                        $drawY = [int][Math]::Round($frameHeight * (1.0 - $progress[$index]))
                    } else {
                        $drawX = $cellX
                        $drawY = -[int][Math]::Round($frameHeight * (1.0 - $progress[$index]))
                    }

                    $graphics.DrawImageUnscaled($source, $drawX, $drawY)
                    $graphics.ResetClip()
                }
            } finally {
                $graphics.Dispose()
            }

            $sheet.Save($OutputPath, [System.Drawing.Imaging.ImageFormat]::Png)
        } finally {
            $sheet.Dispose()
        }
    } finally {
        $source.Dispose()
    }
}

New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
New-PeekSheet -SourcePath $LeftSource -OutputPath (Join-Path $OutputDirectory 'preview-peek-left-3f.png') -Direction left
New-PeekSheet -SourcePath $LeftSource -OutputPath (Join-Path $OutputDirectory 'preview-peek-right-3f.png') -Direction right -MirrorX $true
New-PeekSheet -SourcePath $BottomSource -OutputPath (Join-Path $OutputDirectory 'preview-peek-bottom-3f.png') -Direction bottom
New-PeekSheet -SourcePath $BottomSource -OutputPath (Join-Path $OutputDirectory 'preview-peek-top-3f.png') -Direction top -MirrorY $true
