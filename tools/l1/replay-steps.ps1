# Replay the follower: every step the model gave in the chosen run folders (feed.jsonl), through the
# follower library given, one JSON line per step of what it would do -- the click, the keys and text in
# order, what goes before the click, what it skips. Run it with the old and the new library, then
# compare-replay.py, to see which runs a follower fix would have changed; re-run only those tasks, on
# every arm (l1-live-battery skill). Pure ASCII.
#   powershell -File tools\l1\replay-steps.ps1 -Lib <l1-lib.ps1> -Out old.jsonl -DirPattern '^regular-20261014'
# Two blind spots of the first version, both fixed: a hint ring is the pointer (as run-l1.ps1 treats
# it), and what happens before the click is recorded -- a select-all moved from after its click to
# before it changes nothing else in the signature (2026-10-14).
param([string]$Lib, [string]$Out, [string]$DirPattern, [string]$RunRoot = (Join-Path $env:TEMP 'navisual-l1'))
. $Lib
$dirs = Get-ChildItem $RunRoot -Directory | Where-Object { $_.Name -match $DirPattern }
$lines = New-Object System.Collections.ArrayList
foreach ($d in $dirs) {
    $feed = Join-Path $d.FullName 'feed.jsonl'
    if (-not (Test-Path $feed)) { continue }
    $i = 0
    foreach ($l in [System.IO.File]::ReadAllLines($feed)) {
        $i++
        if (-not $l.Trim()) { continue }
        $s = $l | ConvertFrom-Json
        if (-not $s.instruction) { continue }
        if (-not $s.located -and $s.hint_shown -and $s.ai_bbox) { $s | Add-Member -NotePropertyName located -NotePropertyValue ([pscustomobject]@{ bbox = $s.ai_bbox; name = 'hint ring' }) -Force }
        $a = Get-StepActions $s
        $sig = [ordered]@{
            click = $(if ($a.click) { "$($a.click[0]),$($a.click[1])" } else { '' })
            button = $a.button; double = [bool]$a.double; triple = [bool]$a.triple; drag = [bool]$a.dragAcross
            area = [string]$a.areaClick; mod = [string]$a.clickMod; pointerOnly = [bool]$a.pointerOnly; scroll = $a.scroll; unsupported = [string]$a.unsupported
            seq = ((@($a.seq) | Sort-Object { [double]$_.pos } | ForEach-Object { "$($_.kind):$($_.value)" }) -join ' > ')
            skipped = (@($a.skipped) -join ' | ')
            beforeClick = $(if ($a.click) { (@($a.seq) | Where-Object { [double]$_.pos -lt [double]$a.clickPos } | Sort-Object { [double]$_.pos } | ForEach-Object { "$($_.kind):$($_.value)" }) -join ' > ' } else { '' })
        }
        [void]$lines.Add((@{ dir = $d.Name; line = $i; sig = $sig } | ConvertTo-Json -Compress -Depth 4))
    }
}
[System.IO.File]::WriteAllLines($Out, [string[]]$lines)
"$($lines.Count) steps"
