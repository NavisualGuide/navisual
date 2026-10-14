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

$a = Step 'Triple-click directly on "Quarterly update" to select its paragraph.' -tt 'Quarterly update' -Located
Check 'triple-click' ("triple=$($a.triple) double=$($a.double) click=$([bool]$a.click)") 'triple=True double=False click=True'
$a = Step 'Drag from just before "Quarterly" through the end of "update" to select both words.' -tt 'Quarterly update' -Located
Check 'drag from X through the end of Y' ("drag=$($a.dragAcross) unsupported=$($a.unsupported)") 'drag=True unsupported='
$a = Step 'Drag the file onto the Archive folder.' -tt 'report.csv' -Located
Check 'a drag between two places stays unsupported' ("drag=$($a.dragAcross) unsupported=$($a.unsupported)") 'drag=False unsupported=drag between two places'

# Step 3 review (2026-10-13).
$a = Step 'Enter "colour" in the Find field, then click the Replace field below and enter "color."' -tt 'Find' -clip 'colour' -Located
Check 'a field named before the text' ((Seq $a) + ' | skipped ' + ($a.skipped -join ',')) 'keys:Ctrl+A > paste:colour | skipped color. (for the Replace field)'
$a = Step 'Press Home to move the insertion point to the start of the "1. Introduction" heading.' -tt '1. Introduction' -Located
Check 'the insertion point is not point to' ("click=$([bool]$a.click) " + (Seq $a)) 'click=False keys:Home'
$a = Step 'Point to "Bottom of Page," then press Right to open its gallery.' -tt 'Bottom of Page' -Located
Check 'point to is still a click' ("click=$([bool]$a.click) " + (Seq $a)) 'click=True keys:Right'
$a = Step 'Click Center in the Paragraph group, then replace the value in the Font Size box with 20 and press Enter.' -tt 'Center' -clip '20' -Located -role 'Button'
Check 'text for another box, pointer on a button' ("click=$([bool]$a.click) seq=" + (Seq $a)) 'click=True seq='
$a = Step 'Paste Yes and No on separate lines in the large Entries box, then click "OK" at the bottom to apply the drop-down to all nine cells.' -tt 'OK' -clip "Yes`nNo" -Located -role 'Button'
Check 'text for another box, then click OK' ("click=$([bool]$a.click) seq=" + (Seq $a)) 'click=True seq='
$a = Step 'Replace the highlighted archive name with q3-sales, leaving the.zip ending in place, then press Enter.' -tt 'sales-august.zip' -clip 'q3-sales' -Located -role 'Textbox'
Check 'typing over a highlight' ("click=$([bool]$a.click) seq=" + (Seq $a)) 'click=False seq=paste:q3-sales > keys:Enter'
$a = Step 'Replace the selected filename with q3-sales, keeping the.zip extension, then press Enter.' -tt 'sales-august.zip' -clip 'q3-sales' -Located -role 'Textbox'
Check 'keeping the extension' ("click=$([bool]$a.click) seq=" + (Seq $a)) 'click=False seq=paste:q3-sales > keys:Enter'
# Must not change.
$a = Step 'Replace the text in the File name field with small.png.' -tt 'File name:' -clip 'small.png' -Located -role 'Textbox'
Check 'replace in a named field' ("click=$([bool]$a.click) seq=" + (Seq $a)) 'click=True seq=keys:Ctrl+A > paste:small.png'
$a = Step 'The title is selected. In the Font Size box near the top, replace 14 with 20 and press Enter.' -tt 'Font Size' -clip '20' -Located -role 'Combobox'
Check 'the title is selected is not a highlight to type over' ("click=$([bool]$a.click) seq=" + (Seq $a)) 'click=True seq=keys:Ctrl+A > paste:20 > keys:Enter'
$a = Step 'Select "Start at", enter 1 in the number box, then click OK.' -tt 'Start at' -clip '1' -Located -role 'Radio'
Check 'a radio still takes the text after it' (Seq $a) 'paste:1'
$a = Step 'Enter "Budget" in the Subject field, then press Tab.' -tt 'Subject' -clip 'Budget' -Located -role 'Textbox'
Check 'in the target field moves the text after the click' ("click=$([bool]$a.click) seq=" + (Seq $a)) 'click=True seq=keys:Ctrl+A > paste:Budget > keys:Tab'
$a = Step 'The Total Row checkbox is still off, so the last click didn''t enable it. Press Esc to close the Quick Analysis popup first.' -tt 'Quick Analysis' -Located -role 'Button'
Check 'the last click is not a click' ("click=$([bool]$a.click) " + (Seq $a)) 'click=False keys:Esc'
$a = Step 'The toolbar click did not start renaming. With the selected archive, press F2, then press Enter.' -tt 'q3-sales.zip.zip' -Located -role 'Other'
Check 'the toolbar click is not a click' ("click=$([bool]$a.click) " + (Seq $a)) 'click=False keys:F2 > keys:Enter'
$a = Step 'That click missed. Click the Total Row box, then press Space.' -tt 'Total Row' -Located -role 'Checkbox'
Check 'a click verb after a click noun' ("click=$([bool]$a.click) " + (Seq $a)) 'click=True keys:Space'
# Clean run v6 (2026-10-13).
$a = Step 'The extension warning was cancelled, so the archive is still named sales-august.zip. Press F2, replace only the name before ".zip" with q3-sales, keep ".zip," then press Enter.' -tt 'sales-august.zip' -clip 'q3-sales' -Located -role 'Other'
Check 'replace ... with across .zip' ("click=$([bool]$a.click) " + (Seq $a)) 'click=False keys:F2 > paste:q3-sales > keys:Enter'
$a = Step 'Click the visible page number in the cover''s footer and press Delete. This removes the number from section 1, including the contents page.' -tt '1' -Located -role 'Other'
Check 'the click goes with its verb' ("click=$([bool]$a.click) clickPos=$($a.clickPos) " + (Seq $a)) 'click=True clickPos=0 keys:Delete'
$a = Step 'Select the title with Home, then Shift+End. Click the Font Size box, replace its value with 20.' -tt 'Font Size' -clip '20' -Located -role 'Combobox'
$before = @($a.seq | Where-Object { $_.pos -lt $a.clickPos } | ForEach-Object { $_.value }) -join ','
Check 'keys before the last verb stay first' "before=$before" 'before=Home,Shift+End'
$a = Step 'Double-click the small 1 in the footer at the bottom of the upper page, then press Delete to remove numbering from the cover and contents section.'
Check 'nothing after a click with no pointer' ("cwp=$($a.clickWithoutPointer) seq=" + (Seq $a)) 'cwp=True seq='
$a = Step 'Click the large blank Entries box, paste "Yes" and "No" on separate lines, then click "OK" at the bottom.' -tt 'OK' -clip "Yes`nNo" -Located -role 'Button'
Check 'text named before the click, pointer on a button' ("click=$([bool]$a.click) seq=" + (Seq $a)) 'click=True seq='
$a = Step 'Hover over the 3D Viewport, press F3, search "Add Cylinder," and press Enter.' -clip 'Add Cylinder'
Check 'a hover with no pointer keeps its keys' ("seq=" + (Seq $a)) 'seq=keys:F3 > paste:Add Cylinder > keys:Enter'
$a = Step 'Click sales-august.csv, then hold Ctrl while clicking sales-july.csv and sales-september.csv to select only the three spreadsheets.' -tt 'sales-august.csv' -Located -role 'Other'
Check 'a modifier for the later clicks only' "mod=$($a.clickMod)" 'mod='
$a = Step 'Hold Ctrl and click sales-july.csv, then sales-september.csv.' -tt 'sales-july.csv' -Located -role 'Other'
Check 'hold Ctrl and click the target' "mod=$($a.clickMod)" 'mod=ctrl'
$a = Step 'Ctrl+click sales-july.csv to add it.' -tt 'sales-july.csv' -Located -role 'Other'
Check 'Ctrl+click the target' "mod=$($a.clickMod)" 'mod=ctrl'
# Flash baseline (2026-10-13).
$a = Step 'Type B2:B10 and press Enter to select the cells for all nine requests.' -tt 'A1' -clip 'B2:B10' -Located -role 'Combobox'
Check 'to select is a purpose, not where the click goes' ("click=$([bool]$a.click) clickPos=$($a.clickPos) " + (Seq $a)) 'click=True clickPos=0 keys:Ctrl+A > paste:B2:B10 > keys:Enter'
$a = Step 'Select the Horizontal box, clear the text completely, and type 50.' -tt 'Horizontal' -clip '50' -Located -role 'Textbox'
Check 'clear the text, then type' ("click=$([bool]$a.click) " + (Seq $a)) 'click=True keys:Ctrl+A > paste:50'
$a = Step 'Paste the ticket reference number into the empty "Notes for the service desk" box, then select "Save profile" below it.' -tt 'Notes for the service desk' -clip 'K7Q-4821' -Located -role 'Textbox'
Check 'the empty box: a field, so set' (Seq $a) 'keys:Ctrl+A > paste:K7Q-4821'
$a = Step 'In the File name box, type small.png.' -tt 'File name:' -clip 'small.png' -Located -role 'Textbox'
Check 'typing into an input box sets it' ("click=$([bool]$a.click) " + (Seq $a)) 'click=True keys:Ctrl+A > paste:small.png'
$a = Step 'Click at the end of the Notes box and add "urgent".' -tt 'Notes' -clip 'urgent' -Located -role 'Textbox'
Check 'adding to a box keeps what is there' (Seq $a) 'paste:urgent'
$a = Step 'Type q3-sales and press Enter to rename the new zip archive.' -tt 'sales-august.zip' -clip 'q3-sales' -Located -role 'Textbox'
Check 'the rename box is typed over' ((Seq $a)) 'paste:q3-sales > keys:Enter'
$a = Step 'Click cell B2 and type 4800.' -tt 'B2' -Located -role 'Dataitem'
Check 'a cell is not an input box' (Seq $a) 'text:4800'

if ($fail) { "$fail FAILED"; exit 1 } else { "all ok" }
