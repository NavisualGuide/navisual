# How the follower reads a step (Get-StepActions), on instructions taken from live runs: each case
# was once read wrong, or must not change. Run after any change to the parser. Pure ASCII.
#   powershell -NoProfile -File tools\l1\test-follower.ps1
. "$PSScriptRoot\l1-lib.ps1"
$fail = 0
function Step($ins, $tt = '', $clip = '', [switch]$Located, $role = '') {
    $s = [pscustomobject]@{ instruction = $ins; target_text = $tt; clipboard = $clip; target_role = $role; located = $null }
    if ($Located) { $s.located = [pscustomobject]@{ bbox = [pscustomobject]@{ x = 600; y = 195; width = 118; height = 25 }; name = $tt } }
    Get-StepActions $s
}
function Seq($a) { (@($a.seq) | ForEach-Object { "$($_.kind):$($_.value)" }) -join ' > ' }
function Check($label, $got, $want) {
    if ($got -ne $want) { $script:fail++; "FAIL $label`n   got:  $got`n   want: $want" } else { "ok   $label" }
}

$a = Step 'In the 3D Viewport, press F3, enter "Add Cylinder," then press Enter to add it.' -clip 'Add Cylinder'
Check 'enter + quote is typing' (Seq $a) 'keys:F3 > paste:Add Cylinder > keys:Enter'

$a = Step 'The cursor is in the body text, not the title. Press Ctrl+Home, then Home and Shift+End to select the title line.'
Check 'then Home' (Seq $a) 'keys:Ctrl+Home > keys:Home > keys:Shift+End'

$a = Step 'It is still linked. Focus that button and press Space to switch it off, then confirm it shows OFF.' -tt 'Link to Previous' -Located
Check 'focus is a click' ("click=$([bool]$a.click) " + (Seq $a)) 'click=True keys:Space'

$a = Step 'Click an empty area of the 3D Viewport, then press F3, paste "Add Cylinder," and press Enter.' -clip 'Add Cylinder'
Check 'empty area click' ("area=$($a.areaClick) cwp=$($a.clickWithoutPointer) " + (Seq $a)) 'area=empty cwp=False keys:F3 > paste:Add Cylinder > keys:Enter'

$a = Step 'Click the 3D View, press F3, paste "Add Cylinder" and press Enter. Then press G, Z, type 2, and press Enter to move the new cylinder 2 metres up.' -clip 'Add Cylinder'
Check '3D View click' ("area=$($a.areaClick) " + (Seq $a)) 'area=middle keys:F3 > paste:Add Cylinder > keys:Enter > keys:G > keys:Z > text:2 > keys:Enter'

$a = Step 'Click the large grid area in the middle to focus the 3D Viewport. Press F3, paste "Add Cylinder," then press Enter.' -clip 'Add Cylinder'
Check 'grid area click' ("area=$($a.areaClick)") 'area=middle'

$a = Step 'Undo the last alignment change with Ctrl+Z, then drag across "Quarterly update" to select the title text.' -tt 'Quarterly update' -Located
$before = @($a.seq | Where-Object { $_.pos -lt $a.clickPos -and $_.kind -eq 'keys' } | ForEach-Object { $_.value }) -join ','
Check 'keys before a drag' ("drag=$($a.dragAcross) before=$before") 'drag=True before=Ctrl+Z'

# Must not change.
$a = Step 'Click the Layout tab, then Home tab to go back.' -tt 'Layout' -Located
Check 'then Home tab is a tab' (Seq $a) ''
$a = Step 'Select "Start at", enter 1 in the number box, then click OK.' -tt 'Start at' -clip '1' -Located
Check 'enter 1 is typing' (Seq $a) 'paste:1'
$a = Step 'Press Enter to confirm.'
Check 'press Enter' (Seq $a) 'keys:Enter'
$a = Step 'Click the Center button in the paragraph controls near the top.' -tt 'Center'
Check 'named control, no pointer: no area click' ("area=$($a.areaClick) cwp=$($a.clickWithoutPointer)") 'area= cwp=True'
$a = Step 'Press Alt, then J and H.'
Check 'key tips' (Seq $a) 'keys:Alt > keys:J > keys:H'

if ($fail) { "$fail FAILED"; exit 1 } else { "all ok" }
