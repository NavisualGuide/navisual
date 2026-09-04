<#
.SYNOPSIS
  Rebuild the annotated screenshots of an exported Navisual session.

.DESCRIPTION
  Reads the untouched captures in `steps/` plus `session.json`, and writes marked-up
  copies to `steps-annotated/`. Nothing is re-captured and the originals are never
  modified, so this can be run as many times as you like with different choices.

  This exists because annotation is a decision you will want to change AFTER the
  session is over — a figure that needs a caption in an article often needs none in
  a bug report, and the pointer that helps on one step clutters another. The export
  keeps `steps/` pristine precisely so that decision stays open.

  Everything it needs is already on disk: session.json records each step's pointer
  rect in FRAME-RELATIVE pixels (so it does not care what monitor the session ran
  on) and the instruction text used for the caption.

.EXAMPLE
  .\tools\annotate-session.ps1 -Path "$env:USERPROFILE\Documents\Navisual\exports\navisual-..."

.EXAMPLE
  # Pointer only, and only on steps 4 and 7.
  .\tools\annotate-session.ps1 -Path <folder> -NoCaption -Steps 4,7

.PARAMETER Path
  The exported session folder (the one containing session.json).

.PARAMETER Steps
  1-based step numbers to render. Omit for all of them.

.PARAMETER NoPointer
  Do not draw the pointer.

.PARAMETER NoCaption
  Do not draw the instruction caption.

.PARAMETER OutDir
  Destination subfolder. Defaults to `steps-annotated`.
#>

[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)][string]$Path,
  [int[]]$Steps,
  [switch]$NoPointer,
  [switch]$NoCaption,
  [string]$OutDir = "steps-annotated"
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$json = Join-Path $Path "session.json"
$clean = Join-Path $Path "steps"
if (-not (Test-Path $json))  { throw "No session.json in $Path" }
if (-not (Test-Path $clean)) {
  throw "No steps/ folder in $Path. It was exported without the plain screenshots, so there is nothing to re-annotate from."
}

$session = Get-Content $json -Raw -Encoding UTF8 | ConvertFrom-Json
$dest = Join-Path $Path $OutDir
New-Item -ItemType Directory -Force $dest | Out-Null

# Accent orange, matching the app and the site.
$accent = [System.Drawing.Color]::FromArgb(255, 255, 107, 53)
$written = 0
$skipped = 0
$flat = 0

foreach ($turn in $session.turns) {
  foreach ($step in $turn.assistant.steps) {
    $flat++
    if ($Steps -and ($Steps -notcontains $flat)) { continue }
    if (-not $step.screenshot) { $skipped++; continue }

    # session.json records the path relative to the folder, and may point at
    # steps-annotated/ if that is what the export wrote. We always read the
    # pristine copy, whatever the manifest says.
    $name = Split-Path $step.screenshot -Leaf
    $src  = Join-Path $clean $name
    if (-not (Test-Path $src)) { $skipped++; continue }

    $img = [System.Drawing.Image]::FromFile($src)
    $bmp = New-Object System.Drawing.Bitmap($img)
    $img.Dispose()
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.TextRenderingHint = 'ClearTypeGridFit'

    if (-not $NoPointer -and $step.pointer.rect) {
      $r = $step.pointer.rect
      $pad = 6
      # Three nested rings, faintest outermost — the same shape the app draws.
      foreach ($ring in 3, 2, 1) {
        $alpha = @{ 3 = 90; 2 = 170; 1 = 255 }[$ring]
        $pen = New-Object System.Drawing.Pen(
          [System.Drawing.Color]::FromArgb($alpha, $accent.R, $accent.G, $accent.B), 2)
        $o = $pad + $ring * 2
        $g.DrawRectangle($pen, $r[0] - $o, $r[1] - $o, $r[2] + $o * 2, $r[3] + $o * 2)
        $pen.Dispose()
      }
    }

    if (-not $NoCaption -and $step.instruction) {
      # A real system font, so Chinese and other non-Latin instructions render.
      $size = [Math]::Max(14, [Math]::Min(40, [int]($bmp.Height * 0.022)))
      $font = New-Object System.Drawing.Font("Microsoft YaHei", $size, [System.Drawing.FontStyle]::Regular, [System.Drawing.GraphicsUnit]::Pixel)
      $pad = [int]($size * 0.7)
      $maxW = $bmp.Width - $pad * 4
      $fmt = New-Object System.Drawing.StringFormat
      $measured = $g.MeasureString($step.instruction, $font, $maxW, $fmt)
      $bandH = [int]$measured.Height + $pad * 2
      $band = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(190, 0, 0, 0))
      $g.FillRectangle($band, 0, $bmp.Height - $bandH, $bmp.Width, $bandH)
      $white = [System.Drawing.Brushes]::White
      $rect = New-Object System.Drawing.RectangleF(($pad * 2), ($bmp.Height - $bandH + $pad), $maxW, ($bandH - $pad))
      $g.DrawString($step.instruction, $font, $white, $rect, $fmt)
      $band.Dispose(); $font.Dispose(); $fmt.Dispose()
    }

    $g.Dispose()
    $bmp.Save((Join-Path $dest $name), [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    $written++
  }
}

Write-Host "Wrote $written image(s) to $dest" -ForegroundColor Green
if ($skipped -gt 0) {
  # Steps legitimately have no image: one that was never displayed, or one the
  # export preview redacted. Saying so beats a silent count mismatch.
  Write-Host "$skipped step(s) had no source image (never shown, or redacted)." -ForegroundColor DarkGray
}
