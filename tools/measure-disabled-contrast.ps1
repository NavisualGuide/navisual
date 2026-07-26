# Does "greyed out" survive the AI image pipeline (1536x768 cap, JPEG q=75)?
# Measures text-stroke contrast for a DISABLED vs two ENABLED buttons.
Add-Type -AssemblyName System.Drawing
$sp = Split-Path -Parent $MyInvocation.MyCommand.Path
$src = [System.Drawing.Bitmap]::FromFile("$sp\r05_step4.png")

# Button text bands in r05_step4.png (1920x1080 captured from screen -1920,0)
$regions = @(
  @{ n='Custom Paper Size... (DISABLED)'; x=596; y=762; w=120; h=18 },
  @{ n='Page Options...    (enabled)';    x=812; y=762; w=90;  h=18 },
  @{ n='Restore Defaults   (enabled)';    x=1008;y=762; w=100; h=18 }
)

function Measure-Band($bmp, $r, $sx, $sy) {
  $lums = New-Object System.Collections.ArrayList
  $x0=[int]($r.x*$sx); $y0=[int]($r.y*$sy)
  $x1=[int](($r.x+$r.w)*$sx); $y1=[int](($r.y+$r.h)*$sy)
  for($y=$y0; $y -lt $y1; $y++){
    for($x=$x0; $x -lt $x1; $x++){
      if($x -ge 0 -and $y -ge 0 -and $x -lt $bmp.Width -and $y -lt $bmp.Height){
        $c=$bmp.GetPixel($x,$y)
        [void]$lums.Add(0.299*$c.R + 0.587*$c.G + 0.114*$c.B)
      }
    }
  }
  $a=@($lums | Sort-Object)
  if($a.Count -lt 10){ return $null }
  # background = median (most pixels are background); stroke = darkest 5%
  $bg=$a[[int]($a.Count*0.5)]
  $ink=($a[0..([int]($a.Count*0.05))] | Measure-Object -Average).Average
  [pscustomobject]@{ bg=[math]::Round($bg,1); ink=[math]::Round($ink,1); contrast=[math]::Round($bg-$ink,1) }
}

Write-Host "=== NATIVE (what the locator/OCR sees) ==="
foreach($r in $regions){ $m=Measure-Band $src $r 1 1; "{0}  bg={1,5}  ink={2,5}  contrast={3,5}" -f $r.n,$m.bg,$m.ink,$m.contrast }

# Reproduce the AI pipeline: Lanczos3 downscale to fit 1536x768, then JPEG q=75
$MAXW=1536; $MAXH=768
$scale=[math]::Min($MAXW/$src.Width, $MAXH/$src.Height)
$nw=[int]($src.Width*$scale); $nh=[int]($src.Height*$scale)
$small=New-Object System.Drawing.Bitmap $nw,$nh
$g=[System.Drawing.Graphics]::FromImage($small)
$g.InterpolationMode=[System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
$g.DrawImage($src,0,0,$nw,$nh); $g.Dispose()

$enc=[System.Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object { $_.MimeType -eq 'image/jpeg' }
$ps=New-Object System.Drawing.Imaging.EncoderParameters 1
$ps.Param[0]=New-Object System.Drawing.Imaging.EncoderParameter ([System.Drawing.Imaging.Encoder]::Quality),75L
$small.Save("$sp\ai_pipeline.jpg",$enc,$ps)
$small.Dispose()
$jpg=[System.Drawing.Bitmap]::FromFile("$sp\ai_pipeline.jpg")

Write-Host "`n=== AFTER AI PIPELINE ($nw x $nh, JPEG q=75) ==="
foreach($r in $regions){ $m=Measure-Band $jpg $r $scale $scale; "{0}  bg={1,5}  ink={2,5}  contrast={3,5}" -f $r.n,$m.bg,$m.ink,$m.contrast }
$jpg.Dispose(); $src.Dispose()
