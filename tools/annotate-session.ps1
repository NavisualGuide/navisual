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

.PARAMETER Thickness
  The Pointer thickness slider position (1-10) the session ran at, mapped to the
  same stroke multiplier the live overlay uses. Defaults to 4, the no-op position.

.PARAMETER CaptionInset
  Pixels at the bottom of each frame that the caption must stay clear of -- the
  taskbar. Normally read per step from `caption_bottom_inset` in session.json;
  pass this to override it, which is what a session exported before that field
  existed needs (48 for a Windows 11 taskbar at 100% scaling).

#>

[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)][string]$Path,
  [int[]]$Steps,
  [switch]$NoPointer,
  [switch]$NoCaption,
  [ValidateRange(1, 10)][int]$Thickness = 4,
  [ValidateRange(0, 400)][int]$CaptionInset = -1,
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

# Slider position -> stroke weight multiplier. Must stay in step with strokeScale in
# src/lib/overlay-weight.ts and stroke_scale() in src-tauri/src/session_export.rs:
# the exported pointer and the on-screen one are supposed to be the same picture.
# Only STROKE WIDTHS scale. Geometry -- padding, arm length, ripple growth --
# is layout and stays put.
$k = if ($Thickness -le 4) { 0.6 + (($Thickness - 1) / 3.0) * 0.4 }
     else { 1.0 + ($Thickness - 4) / 6.0 }

# The live overlay pulses on a timer; a still takes the value each pulse holds at
# t=0, which for (sin(0)+1)/2 is the midpoint.
$pulse = 0.5
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

      # The rect the MARK is built on: the located rect, padded, then floored so a
      # tiny target still gets something you can spot. Must match markRect in
      # src/Overlay.svelte and the pad/MIN_MARK block in draw_pointer() in
      # session_export.rs -- the three are supposed to produce the same picture.
      # Geometry, so none of it scales with $k.
      # Frame px per logical px. The overlay draws in logical pixels; this frame is
      # physical and may have been downscaled. The located rect was already converted
      # with the frame -- only the fixed constants below need it. Absent (a session
      # exported before the field existed) means 1.0, which is what they already were.
      $markScale = 1.0
      if ($null -ne $step.PSObject.Properties['mark_scale']) {
        $ms = [double]$step.mark_scale
        if ($ms -gt 0 -and -not [double]::IsNaN($ms)) { $markScale = $ms }
      }

      # A hint is the model's own bbox, drawn because both locator passes missed. The
      # app marks that difference -- dashed brackets, no corner dots, no crosshair,
      # softer alphas -- and an export that drew the confident mark instead would claim
      # a precision the session never had.
      $isHint = ($step.pointer.state -eq 'hint')

      $MIN_MARK = 36.0 * $markScale
      $pad = [Math]::Min(20.0 * $markScale, [Math]::Max(12.0 * $markScale, [Math]::Min($bw, $bh) * 0.45))
      $pw = [Math]::Max($bw + $pad * 2.0, $MIN_MARK)
      $ph = [Math]::Max($bh + $pad * 2.0, $MIN_MARK)
      $px = $cx - $pw / 2.0; $py = $cy - $ph / 2.0

      # Ripple rings. Ellipse, not circle: a circle sized by the long axis balloons
      # past a thin element's short axis. Growth is capped on the short axis of a
      # wide row for the same reason.
      $growth   = [Math]::Min($pw, $ph) * 0.7
      $ryGrowth = if ($pw -gt $ph * 2.0) { [Math]::Min($growth, $ph * 0.4) } else { $growth }
      foreach ($i in 0, 1, 2) {
        $phase = $i / 3.0
        $rx = $pw / 2.0 + 8.0 * $markScale + $phase * $growth
        $ry = $ph / 2.0 + 8.0 * $markScale + $phase * $ryGrowth
        $a  = [int]((1.0 - $phase) * $(if ($isHint) { 0.40 } else { 0.55 }) * 255)
        $taper = if ($isHint) { 2.0 - $phase * 1.4 } else { 2.5 - $phase * 1.8 }
        $wd = [Math]::Max(1.0, $taper * $k * $markScale)
        $pen = New-Object System.Drawing.Pen(
          [System.Drawing.Color]::FromArgb($a, $accent.R, $accent.G, $accent.B), $wd)
        $g.DrawEllipse($pen, $cx - $rx, $cy - $ry, $rx * 2.0, $ry * 2.0)
        $pen.Dispose()
      }

      # Corner brackets: a dark stroke under the accent one, so the mark survives on
      # any background.
      $arm = [Math]::Min(26.0 * $markScale, [Math]::Max(14.0 * $markScale, [Math]::Min($pw * 0.38, $ph * 0.5)))
      if ($isHint) {
        $shadow = New-Object System.Drawing.Pen(
          [System.Drawing.Color]::FromArgb(166, 0, 0, 0), [single](4.5 * $k * $markScale))
        $bright = New-Object System.Drawing.Pen(
          [System.Drawing.Color]::FromArgb([int]((0.72 + $pulse * 0.15) * 255),
            $accent.R, $accent.G, $accent.B), [single](2.5 * $k * $markScale))
        foreach ($pen in $shadow, $bright) {
          $pen.DashStyle = 'Custom'
          # GDI+ dash lengths are multiples of the pen width; canvas setLineDash is in
          # pixels, so divide through to get the same 5-on/4-off pattern.
          $pen.DashPattern = @([single](5.0 * $markScale / $pen.Width), [single](4.0 * $markScale / $pen.Width))
        }
      } else {
        $shadow = New-Object System.Drawing.Pen(
          [System.Drawing.Color]::FromArgb(191, 0, 0, 0), [single](5.5 * $k * $markScale))
        $bright = New-Object System.Drawing.Pen(
          [System.Drawing.Color]::FromArgb(255, $accent.R, $accent.G, $accent.B), [single](3.0 * $k * $markScale))
      }
      $dot = New-Object System.Drawing.SolidBrush(
        [System.Drawing.Color]::FromArgb(255, $accent.R, $accent.G, $accent.B))
      # Indices, not an array of arrays: PowerShell flattens nested arrays built with
      # @(), so $corner[0] came back as an array and the arithmetic below failed with
      # "does not contain a method named 'op_Addition'".
      foreach ($corner in 0, 1, 2, 3) {
        $left = ($corner -eq 0 -or $corner -eq 2)
        $top  = ($corner -lt 2)
        $ox = if ($left) { $px } else { $px + $pw }
        $oy = if ($top)  { $py } else { $py + $ph }
        $dx = if ($left) { 1.0 } else { -1.0 }
        $dy = if ($top)  { 1.0 } else { -1.0 }
        foreach ($pen in $shadow, $bright) {
          $g.DrawLine($pen, $ox + $dx * $arm, $oy, $ox, $oy)
          $g.DrawLine($pen, $ox, $oy, $ox, $oy + $dy * $arm)
        }
        if (-not $isHint) {
          $dotR = 3.5 * $k * $markScale
          $g.FillEllipse($dot, $ox - $dotR, $oy - $dotR, $dotR * 2.0, $dotR * 2.0)
        }
      }
      $shadow.Dispose(); $bright.Dispose(); $dot.Dispose()

      # Centre crosshair. The app pulses this between 0.35 and 0.60 alpha; a still
      # takes the midpoint.
      if (-not $isHint) {
        $cross = New-Object System.Drawing.Pen(
          [System.Drawing.Color]::FromArgb([int](0.475 * 255), $accent.R, $accent.G, $accent.B),
          [single](1.5 * $k * $markScale))
        $cr = 5.0 * $markScale
        $g.DrawLine($cross, $cx - $cr, $cy, $cx + $cr, $cy)
        $g.DrawLine($cross, $cx, $cy - $cr, $cx, $cy + $cr)
        $cross.Dispose()
      }

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
      # The live overlay anchors the caption to the monitor's WORK AREA, not the
      # monitor, so the strip sits above the taskbar instead of across its icons
      # (fixed in the app 2026-09-07 after a live report). A frame is a whole-
      # monitor capture WITH the taskbar in it, so anchoring to the frame's own
      # bottom edge reintroduces exactly that defect in the exported figure.
      $inset = 0
      if ($CaptionInset -ge 0) {
        $inset = $CaptionInset
      } elseif ($null -ne $step.PSObject.Properties['caption_bottom_inset']) {
        $inset = [int]$step.caption_bottom_inset
      }
      $gap = [int]($size * 0.6)
      $floor = [Math]::Max($bmp.Height - $inset, $stripH + $gap)
      $stripY = [Math]::Max($floor - $stripH - $gap, 0)
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
