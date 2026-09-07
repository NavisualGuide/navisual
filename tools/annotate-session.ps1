<#
.SYNOPSIS
  Rebuild the annotated screenshots of an exported Navisual session.

.DESCRIPTION
  Reads the untouched captures in `steps/` plus `session.json`, and writes marked-up
  copies to `steps-annotated/`. Nothing is re-captured and the originals are never
  modified, so this can be run as many times as you like with different choices.

  This exists because annotation is a decision you will want to change AFTER the
  session is over -- a figure that needs a caption in an article often needs none in
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

# Resolve before touching .NET: System.Drawing takes raw strings and does not
# understand `~`, PSDrives or PowerShell-relative paths, so a path that passes
# Test-Path can still fail inside the image loader with a misleading message.
if (-not (Test-Path -LiteralPath $Path)) { throw "Not found: $Path" }
$Path = (Resolve-Path -LiteralPath $Path).ProviderPath

$json = Join-Path $Path "session.json"
$clean = Join-Path $Path "steps"
if (-not (Test-Path -LiteralPath $json)) {
  throw "No session.json in $Path -- this takes an exported session FOLDER, not a file or a log."
}
if (-not (Test-Path -LiteralPath $clean)) {
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
      # Same pointer the app draws (Overlay.svelte drawBox) and the same one the Rust
      # exporter burns in: ripple rings, corner brackets, centre crosshair. This used
      # to be three nested rectangles under a comment claiming it was "the same shape
      # the app draws" -- it never was. Keep this in step with draw_pointer() in
      # src-tauri/src/session_export.rs; they must produce the same picture, because
      # this script exists to redo exactly what the export already did.
      #
      # Two deliberate departures from the live overlay, because a still cannot show
      # motion: the ripples are frozen at the three phases they hold at t=0, and the
      # sweeping scan line is dropped (frozen it is just a bar across the element).
      $r  = $step.pointer.rect
      $bx = [double]$r[0]; $by = [double]$r[1]
      $bw = [double]$r[2]; $bh = [double]$r[3]
      $cx = $bx + $bw / 2.0
      $cy = $by + $bh / 2.0

      # Ripple rings. Ellipse, not circle: a circle sized by the long axis balloons
      # past a thin element's short axis. Growth is capped on the short axis of a
      # wide row for the same reason.
      $growth   = [Math]::Min($bw, $bh) * 0.7
      $ryGrowth = if ($bw -gt $bh * 2.0) { [Math]::Min($growth, $bh * 0.4) } else { $growth }
      foreach ($i in 0, 1, 2) {
        $phase = $i / 3.0
        $rx = $bw / 2.0 + 8.0 + $phase * $growth
        $ry = $bh / 2.0 + 8.0 + $phase * $ryGrowth
        $a  = [int]((1.0 - $phase) * 0.55 * 255)
        $wd = [Math]::Max(1.0, 2.5 - $phase * 1.8)
        $pen = New-Object System.Drawing.Pen(
          [System.Drawing.Color]::FromArgb($a, $accent.R, $accent.G, $accent.B), $wd)
        $g.DrawEllipse($pen, $cx - $rx, $cy - $ry, $rx * 2.0, $ry * 2.0)
        $pen.Dispose()
      }

      # Corner brackets: a dark stroke under the accent one, so the mark survives on
      # any background.
      $arm = [Math]::Min(26.0, [Math]::Max(14.0, [Math]::Min($bw * 0.38, $bh * 0.5)))
      $shadow = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(191, 0, 0, 0), 5.5)
      $bright = New-Object System.Drawing.Pen(
        [System.Drawing.Color]::FromArgb(255, $accent.R, $accent.G, $accent.B), 3.0)
      $dot = New-Object System.Drawing.SolidBrush(
        [System.Drawing.Color]::FromArgb(255, $accent.R, $accent.G, $accent.B))
      # Indices, not an array of arrays: PowerShell flattens nested arrays built with
      # @(), so $corner[0] came back as an array and the arithmetic below failed with
      # "does not contain a method named 'op_Addition'".
      foreach ($corner in 0, 1, 2, 3) {
        $left = ($corner -eq 0 -or $corner -eq 2)
        $top  = ($corner -lt 2)
        $ox = if ($left) { $bx } else { $bx + $bw }
        $oy = if ($top)  { $by } else { $by + $bh }
        $dx = if ($left) { 1.0 } else { -1.0 }
        $dy = if ($top)  { 1.0 } else { -1.0 }
        foreach ($pen in $shadow, $bright) {
          $g.DrawLine($pen, $ox + $dx * $arm, $oy, $ox, $oy)
          $g.DrawLine($pen, $ox, $oy, $ox, $oy + $dy * $arm)
        }
        $g.FillEllipse($dot, $ox - 3.5, $oy - 3.5, 7.0, 7.0)
      }
      $shadow.Dispose(); $bright.Dispose(); $dot.Dispose()

      # Centre crosshair. The app pulses this between 0.35 and 0.60 alpha; a still
      # takes the midpoint.
      $cross = New-Object System.Drawing.Pen(
        [System.Drawing.Color]::FromArgb([int](0.475 * 255), $accent.R, $accent.G, $accent.B), 1.5)
      $g.DrawLine($cross, $cx - 5.0, $cy, $cx + 5.0, $cy)
      $g.DrawLine($cross, $cx, $cy - 5.0, $cx, $cy + 5.0)
      $cross.Dispose()
    }

    if (-not $NoCaption -and $step.instruction) {
      # Same shape as the app's on-screen caption (Overlay.svelte drawSubtitle)
      # and as the Rust exporter: a rounded strip fitted to the text, centred,
      # rgba(0,0,0,0.52), white centred text, floated just above the bottom edge.
      # Someone comparing an exported figure with their own screen should see the
      # same thing.
      #
      # Microsoft YaHei so Chinese instructions render -- the app replies in the
      # user's language, so a Latin-only font would be useless to many of them.
      $size = [Math]::Max(14, [Math]::Min(40, [int]($bmp.Height * 0.022)))
      $font = New-Object System.Drawing.Font("Microsoft YaHei", $size, [System.Drawing.FontStyle]::Regular, [System.Drawing.GraphicsUnit]::Pixel)
      $fmt = New-Object System.Drawing.StringFormat
      $fmt.Alignment = 'Center'
      $fmt.LineAlignment = 'Center'

      $maxW    = [int]($bmp.Width * 0.78)
      $hPad    = [int]($size * 1.2)
      $vPad    = [int]($size * 0.65)
      $measured = $g.MeasureString($step.instruction, $font, $maxW, $fmt)
      $stripW  = [Math]::Min([int]$measured.Width + $hPad * 2, $bmp.Width)
      $stripH  = [int]$measured.Height + $vPad * 2
      $stripX  = [int](($bmp.Width - $stripW) / 2)
      $stripY  = $bmp.Height - $stripH - [int]($size * 0.6)
      $radius  = [int]($size * 0.55)

      # Rounded rect via a path, so the corners match the app's 10px radius look.
      # NOT `$path` -- PowerShell variable names are case-insensitive, so that is
      # the SAME variable as the `$Path` parameter. Assigning a GraphicsPath to it
      # replaced the session folder with a graphics object, and the next call blew
      # up with "[System.String] does not contain a method named 'AddArc'".
      $gp = New-Object System.Drawing.Drawing2D.GraphicsPath
      $d = $radius * 2
      $gp.AddArc($stripX, $stripY, $d, $d, 180, 90)
      $gp.AddArc($stripX + $stripW - $d, $stripY, $d, $d, 270, 90)
      $gp.AddArc($stripX + $stripW - $d, $stripY + $stripH - $d, $d, $d, 0, 90)
      $gp.AddArc($stripX, $stripY + $stripH - $d, $d, $d, 90, 90)
      $gp.CloseFigure()
      # 0.52 alpha = 133/255, the overlay's own value.
      $band = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(133, 0, 0, 0))
      $g.FillPath($band, $gp)

      $rect = New-Object System.Drawing.RectangleF($stripX, $stripY, $stripW, $stripH)
      $g.DrawString($step.instruction, $font, [System.Drawing.Brushes]::White, $rect, $fmt)
      $band.Dispose(); $gp.Dispose(); $font.Dispose(); $fmt.Dispose()
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
