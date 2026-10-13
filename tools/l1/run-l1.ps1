<#
.SYNOPSIS
L1 -- live multi-step tasks, end to end, driven by a follower (navisual-internal docs/harness-plan.md).

.DESCRIPTION
Starts a release build of Navisual in its own data folder (NAVISUAL_DATA_DIR, CLAUDE.md rule 22)
with a copy of the installed app's .env set to one tier's model, then for each task: puts the
app in its start state, types the instruction into the panel, follows every step with real mouse
and keyboard input, presses the Next hotkey, and stops at the end state, a cap, or a step it
cannot follow. One result row per run.

The follower does what the step says and nothing else:
  - a located target is clicked at its centre (right/double when the instruction says so), and
    so is the model's box when the dashed hint ring stands in for a missed locate;
  - clipboard text is pasted (rule 11 puts any text to type there, and the app copies it);
  - key combinations named in the instruction are pressed;
  - a question (needs_input) is answered from the task's persona replies;
  - a step with nothing to act on gets "I can't find that on my screen." (twice at most).

The tier -> model mapping below is L1's roster as decided on 2026-10-11 (harness-plan.md); the
live tier mapping is server-plan.md's.

.EXAMPLE
.\tools\l1\run-l1.ps1 -Tier speed -Tasks np-replace
.EXAMPLE
.\tools\l1\run-l1.ps1 -Tier smart -Tasks all -Repeat 3
.EXAMPLE
.\tools\l1\run-l1.ps1 -Solve -Tasks np-replace     # the solvability gate: reference solution only
#>
param(
    [ValidateSet('speed', 'regular', 'smart')] [string]$Tier = 'speed',
    [string]$Tasks = 'all',
    [int]$Repeat = 1,
    [string]$TaskRoot = '',
    [string]$OutDir = '',
    [string]$Exe = '',
    [string]$RunRoot = '',
    [switch]$Solve,
    [switch]$IncludeHeldOut,
    # Stop before the next run once this tier's estimated spend passes it (list prices, no cache).
    [double]$MaxUsd = 10.0,
    # Debugging: save the task window as it is just before each click (the test app only).
    [switch]$ShotBeforeClick,
    # The harness switches, for L1 step 3 (harness-plan.md). 'default' is the product as it
    # ships: screen-first rules, the list of earlier screens and states in the list on; H1 and
    # the look tools off. 'on' turns every switch on (H1, zoom and read_text, recall, ui_tree
    # too); 'off' turns every one off, with rules 2-3 back to v0.7.36's.
    [ValidateSet('default', 'on', 'off')] [string]$Harness = 'default',
    # Learned notes carry what one run learned into the later runs of the same tier, so repeated
    # runs stop being independent; off unless asked for, even with -Harness on.
    [switch]$LearnedNotes
)

. "$PSScriptRoot\l1-lib.ps1"
Keep-Awake

$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$internal = Join-Path (Split-Path -Parent $repo) 'navisual-internal'
if (-not $TaskRoot) { $TaskRoot = Join-Path $internal 'eval\l1\tasks' }
if (-not $OutDir) { $OutDir = Join-Path $internal 'eval\results\l1' }
if (-not $Exe) { $Exe = Join-Path $repo 'src-tauri\target\release\navisual-backend.exe' }
if (-not $RunRoot) { $RunRoot = Join-Path $env:TEMP 'navisual-l1' }
New-Item -ItemType Directory -Force -Path $OutDir, $RunRoot | Out-Null

$Roster = @{
    speed   = @{ provider = 'openai'; model = 'gpt-6-luna';       effort = ''; usdIn = 0.10; usdOut = 0.50 }
    regular = @{ provider = 'gemini'; model = 'gemini-3.8-flash'; effort = 'low'; usdIn = 0.75; usdOut = 3.75 }
    smart   = @{ provider = 'openai'; model = 'gpt-6.1-sol';      effort = ''; usdIn = 2.0; usdOut = 10.0 }
}
# What produced the run: the app's commit and the build it ran.
$appCommit = (git -C $repo rev-parse --short HEAD 2>$null)
$exeTime = ''; if (Test-Path $Exe) { $exeTime = (Get-Item $Exe).LastWriteTime.ToString('s') }

# ---------------------------------------------------------------------------------------------
# Tasks

$all = Get-ChildItem $TaskRoot -Directory | Where-Object { Test-Path (Join-Path $_.FullName 'task.json') }
$picked = @()
foreach ($d in $all) {
    $t = Get-Content (Join-Path $d.FullName 'task.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    $t | Add-Member -NotePropertyName dir -NotePropertyValue $d.FullName
    if ($Tasks -ne 'all') {
        if (($Tasks -split ',' | ForEach-Object { $_.Trim() }) -notcontains $t.id) { continue }
    } elseif ($t.held_out -and -not $IncludeHeldOut) { continue }
    $picked += $t
}
if ($picked.Count -eq 0) { throw "No tasks matched '$Tasks' under $TaskRoot" }

$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$runDir = Join-Path $RunRoot "$Tier-$stamp"
New-Item -ItemType Directory -Force -Path $runDir | Out-Null
$global:L1Log = Join-Path $runDir 'run.log'
$resultsFile = Join-Path $OutDir "l1-$Tier-$stamp.jsonl"
if ($Solve) { $resultsFile = Join-Path $OutDir "l1-solve-$stamp.jsonl" }

function Invoke-TaskScript($task, [string]$name, [string]$work) {
    $p = Join-Path $task.dir "$name.ps1"
    if (-not (Test-Path $p)) { return $null }
    $out = & $p -Work $work 2>&1 | Out-String
    return $out.Trim()
}

function Get-Verdict($task, [string]$work) {
    $raw = Invoke-TaskScript $task 'verify' $work
    try { return ($raw | ConvertFrom-Json) } catch { return [pscustomobject]@{ success = $false; progress = 0; detail = "verify output unreadable: $raw" } }
}

function Get-TargetInfo([string]$work) {
    $f = Join-Path $work 'target.json'
    if (-not (Test-Path $f)) { return $null }
    return (Get-Content $f -Raw | ConvertFrom-Json)
}

# The task's window: by title where the setup gave one (Explorer shares its process with the
# shell and every other folder window), else the process's first titled window.
function Get-TargetWindow($info) {
    if (-not $info) { return $null }
    if ($info.hwnd) {
        $hw = [IntPtr][long]$info.hwnd
        $w = Get-TopWindows { param($x) $x.Hwnd -eq $hw } | Select-Object -First 1
        if ($w) { return $w }
    }
    if ($info.title) {
        $t = [string]$info.title
        $w = Get-TopWindows { param($x) $x.Title -like "*$t*" } | Select-Object -First 1
        if ($w) { return $w }
    }
    $p = [int]$info.pid
    Get-TopWindows { param($x) $x.Pid -eq $p -and $x.Title -ne '' } | Select-Object -First 1
}

# ---------------------------------------------------------------------------------------------
# The solvability gate: setup, the reference solution, verify. No Navisual, no model.

if ($Solve) {
    foreach ($task in $picked) {
        $work = Join-Path $runDir "$($task.id)-solve"
        New-Item -ItemType Directory -Force -Path $work | Out-Null
        Write-L1 "=== solve $($task.id)"
        Protect-OpenWindows
        $global:L1TextMode = 'unicode'; if ($task.text_input -eq 'keys') { $global:L1TextMode = 'keys' }
        Write-L1 ("setup: " + (Invoke-TaskScript $task 'setup' $work))
        Start-Sleep -Seconds 2
        Write-L1 ("solve: " + (Invoke-TaskScript $task 'solve' $work))
        Start-Sleep -Seconds 1
        $v = Get-Verdict $task $work
        Write-L1 ("verify: success={0} progress={1} {2}" -f $v.success, $v.progress, $v.detail)
        Write-L1 ("teardown: " + (Invoke-TaskScript $task 'teardown' $work))
        $row = [ordered]@{ mode = 'solve'; task = $task.id; success = [bool]$v.success; progress = $v.progress; detail = $v.detail; ts = (Get-Date -Format o) }
        Add-Content -Path $resultsFile -Value ($row | ConvertTo-Json -Compress) -Encoding UTF8
    }
    Write-L1 "results: $resultsFile"
    return
}

# ---------------------------------------------------------------------------------------------
# Navisual, in its own data folder, on this tier's model.

$cfg = $Roster[$Tier]
$data = Join-Path $runDir 'data'
New-Item -ItemType Directory -Force -Path $data | Out-Null
$srcEnv = Join-Path $env:LOCALAPPDATA 'com.navisual.app\.env'
$envLines = @()
if (Test-Path $srcEnv) { $envLines = [System.IO.File]::ReadAllLines($srcEnv) }
$set = [ordered]@{
    API_PROVIDER = $cfg.provider
    REASONING_EFFORT = $cfg.effort
    AUTO_ADVANCE = 'false'
    TTS_ENABLED = 'false'
    UI_LANGUAGE = 'en'
    TRAINING_CAPTURE_ENABLED = 'false'
    SESSION_EXPORT_ENABLED = 'false'
    DEBUG_LOG_FILES_ENABLED = 'true'
    DEBUG_SCREENSHOT_ENABLED = 'false'
    LEARNED_NOTES = 'false'
    INSPECT = ''
    EXPECTED_RESULT = 'false'
    HOTKEY_NEXT = 'Ctrl+Backquote'
}
if ($cfg.provider -eq 'openai') { $set['OPENAI_MODEL'] = $cfg.model } else { $set['GEMINI_MODEL'] = $cfg.model }
$harnessSets = @{
    default = [ordered]@{ SCREEN_FIRST_RULES = 'true'; EARLIER_SCREENS = 'true'; LIST_STATES = 'true'; EXPECTED_RESULT = 'false'; INSPECT = ''; INSPECT_TOOLS = 'zoom'; RECALL = 'false'; UI_TREE = 'false' }
    on      = [ordered]@{ SCREEN_FIRST_RULES = 'true'; EARLIER_SCREENS = 'true'; LIST_STATES = 'true'; EXPECTED_RESULT = 'true'; INSPECT = 'choice'; INSPECT_TOOLS = 'both'; RECALL = 'true'; UI_TREE = 'true' }
    off     = [ordered]@{ SCREEN_FIRST_RULES = 'false'; EARLIER_SCREENS = 'false'; LIST_STATES = 'false'; EXPECTED_RESULT = 'false'; INSPECT = ''; INSPECT_TOOLS = 'zoom'; RECALL = 'false'; UI_TREE = 'false' }
}
$hs = $harnessSets[$Harness]
foreach ($k in $hs.Keys) { $set[$k] = $hs[$k] }
$set['LEARNED_NOTES'] = $(if ($LearnedNotes) { 'true' } else { 'false' })
$out = New-Object System.Collections.Generic.List[string]
$seen = @{}
foreach ($l in $envLines) {
    $k = ($l -split '=', 2)[0].Trim()
    if ($set.Contains($k)) { $out.Add("$k=$($set[$k])"); $seen[$k] = $true } else { $out.Add($l) }
}
foreach ($k in $set.Keys) { if (-not $seen[$k]) { $out.Add("$k=$($set[$k])") } }
[System.IO.File]::WriteAllLines((Join-Path $data '.env'), $out)

$feed = Join-Path $runDir 'feed.jsonl'
$env:NAVISUAL_DATA_DIR = $data
$env:NAVISUAL_STEP_FEED = $feed
Write-L1 "tier $Tier -> $($cfg.provider) $($cfg.model) effort='$($cfg.effort)'; harness $Harness, learned notes $([bool]$LearnedNotes); data $data"
$proc = Start-Process -FilePath $Exe -PassThru -WorkingDirectory (Split-Path -Parent $Exe)
$env:NAVISUAL_DATA_DIR = ''
$env:NAVISUAL_STEP_FEED = ''
$navPid = $proc.Id
$deadline = (Get-Date).AddSeconds(60)
while (-not (Get-PanelWindow $navPid) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500 }
if (-not (Get-PanelWindow $navPid)) { Stop-Process -Id $navPid -Force; throw "Navisual panel did not appear" }
Start-Sleep -Seconds 4
Write-L1 "Navisual up (pid $navPid)"
# A fresh data folder means a first run, and the first-run privacy notice covers the panel. A user
# reads it and presses "I understand -- continue" before any task; so does the run. (Until
# 2026-10-11 the follower typed behind it, a state no user is in. The button only sets a flag in
# the panel's own storage, inside the isolated folder.)
# The notice renders a few seconds after the window: wait for it, up to 20 s.
$ack = $null
for ($i = 0; $i -lt 20 -and -not $ack; $i++) {
    $root = Get-PanelRoot $navPid
    if ($root) { $ack = Find-ByType $root 'Button' | Where-Object { $_.Current.Name -like 'I understand*' } | Select-Object -First 1 }
    if (-not $ack) { Start-Sleep -Seconds 1 }
}
if ($ack) { Invoke-Element $ack; Start-Sleep -Milliseconds 800; Write-L1 "privacy notice acknowledged" } else { Write-L1 "no privacy notice showing" }

# ---------------------------------------------------------------------------------------------
# One run.

function Invoke-L1Run($task, [int]$rep) {
    $work = Join-Path $runDir "$($task.id)-r$rep"
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    Write-L1 "=== $($task.id) r$rep ($Tier)"
    Protect-OpenWindows
    $global:L1TextMode = 'unicode'; if ($task.text_input -eq 'keys') { $global:L1TextMode = 'keys' }
    Write-L1 ("setup: " + (Invoke-TaskScript $task 'setup' $work))
    Start-Sleep -Seconds 2
    $tinfo = Get-TargetInfo $work
    $tw = Get-TargetWindow $tinfo
    $targetPid = 0; if ($tw) { $targetPid = $tw.Pid }
    if (-not $tw) { Write-L1 "no target window"; return [ordered]@{ task = $task.id; rep = $rep; outcome = 'setup_failed' } }
    Write-L1 ("start-state: " + ((Get-Verdict $task $work) | ConvertTo-Json -Compress))

    Wait-InputUnblocked
    Write-L1 ("panel: " + (Start-PanelNewTask $navPid))
    Focus-ByTitleBar $tw.Hwnd | Out-Null
    $feedCount = @(Get-FeedLines $feed).Count
    $firstFeed = $feedCount
    Write-L1 ("task: " + (Send-PanelText $navPid $task.instruction))

    $maxReq = 15; if ($task.max_requests) { $maxReq = [int]$task.max_requests }
    $maxMin = 8; if ($task.max_minutes) { $maxMin = [double]$task.max_minutes }
    $t0 = Get-Date
    $requests = 0; $steps = 0; $cantFind = 0; $replies = 0; $errors = 0; $clicks = 0; $misclicked = $false; $hintSteps = 0
    $outcome = 'running'; $verdict = $null; $claimedDone = $false

    while ($outcome -eq 'running') {
        $r = Wait-FeedLine $feed $feedCount 150
        if (-not $r) { $outcome = 'timeout'; break }
        $feedCount = $r.Count
        $s = $r.Item
        if ($s.kind -ne 'next' -and $s.request_id) { $requests++ }
        $tag = "[$($s.kind) $($s.step_index + 1)/$($s.step_count)]"
        if (-not $s.ok -or $s.error) {
            $errors++
            Write-L1 "$tag error: $($s.error)"
            if ($errors -ge 3) { $outcome = 'error'; break }
            # The panel puts the request's text back in its box after a failure; a person presses its
            # button again. Next is disabled when the very first request failed, so pressing it
            # waited out the timeout instead (two scored runs, 2026-10-12).
            Start-Sleep -Seconds 3
            $re = Resend-PanelText $navPid
            if ($re) { Write-L1 "retry: $re" } else { Send-NextHotkey; Write-L1 "retry: Next" }
            continue
        }
        # The locator missed and the dashed hint ring stood in, around the model's box. A person
        # clicks inside the ring, so the follower takes its box as the pointer. The first scored
        # pass ignored every hint, which made the follower stricter than a person (2026-10-12).
        if (-not $s.located -and $s.hint_shown -and $s.ai_bbox) {
            $s | Add-Member -NotePropertyName located -NotePropertyValue ([pscustomobject]@{ bbox = $s.ai_bbox; name = 'hint ring' }) -Force
            $hintSteps++
        }
        $loc = 'no pointer'
        if ($s.located) { $b = $s.located.bbox; $loc = "pointer ($($b.x),$($b.y) $($b.width)x$($b.height)) '$($s.located.name)'" }
        Write-L1 "$tag `"$($s.instruction)`" target='$($s.target_text)' $loc clip='$($s.clipboard)' ask=$($s.needs_input)"

        if ($s.needs_input) {
            $reply = $task.default_reply
            # Questions about what the user did or sees get the follower's honest answer: it did what
            # the step said, at the pointer, and it only knows where the pointer is.
            if ($s.instruction -match '(?i)\b(did you|have you|were you able)\b') { $reply = 'Yes, I did what the last step said, where the pointer was.' }
            elseif ($s.instruction -match '(?i)\b(do you see|can you see|is there|are there|which .* do you see)\b') { $reply = "I'm not sure. Please point to it." }
            foreach ($p in @($task.persona)) { if ($p -and $s.instruction -match $p.match) { $reply = $p.reply; break } }
            if (-not $reply) { $reply = 'Whatever is usual is fine.' }
            $replies++
            Write-L1 ("persona: '$reply' " + (Send-PanelText $navPid $reply))
            continue
        }
        if ($s.step_count -eq 0) {
            $claimedDone = $true
            $verdict = Get-Verdict $task $work
            if ($verdict.success) { $outcome = 'success' } else { $outcome = 'claimed_done' }
            break
        }

        Wait-InputUnblocked
        $a = Get-StepActions $s
        $acted = $false
        $tw = Get-TargetWindow $tinfo
        $needsTarget = @($a.seq | Where-Object { $_.kind -in 'paste', 'text' }).Count -gt 0
        foreach ($sk in @($a.skipped)) { Write-L1 "skipped text $sk -- no pointer on that field" }
        if ($a.pointerOnly) { Write-L1 "pointer shown, but the step asks for no click" }
        if ($a.unsupported) { Write-L1 "cannot follow: $($a.unsupported)" }
        elseif ($a.dragAcross) {
            # Keys the step gives before the drag go first: "Undo the last alignment change with
            # Ctrl+Z, then drag across ..." dragged and never undid (verification run, 2026-10-12).
            foreach ($t in @($a.seq | Where-Object { $_.pos -lt $a.clickPos -and $_.kind -eq 'keys' })) {
                if (Send-Combo $t.value) { Write-L1 "keys $($t.value)" } else { Write-L1 "keys not understood: $($t.value)" }
                Start-Sleep -Milliseconds 250
            }
            $b = $s.located.bbox
            $cy = [int]($b.y + $b.height / 2)
            Drag-Between ([int]$b.x + 2) $cy ([int]($b.x + $b.width) - 2) $cy
            Write-L1 "drag across '$($s.located.name)' ($($b.x)..$($b.x + $b.width), $cy)"; $acted = $true
        }
        elseif ($a.areaClick -and $tw) {
            # Inside the app window: its middle, or lower left for "an empty area" -- the spot the
            # setup scripts already use to close Blender's splash without touching a control.
            $r = Get-WindowRect $tw.Hwnd
            $fx = 0.5; $fy = 0.55
            if ($a.areaClick -eq 'empty') { $fx = 0.25; $fy = 0.75 }
            $ax = [int]($r.Left + ($r.Right - $r.Left) * $fx); $ay = [int]($r.Top + ($r.Bottom - $r.Top) * $fy)
            Click-Point $ax $ay
            $clicks++; $acted = $true
            Write-L1 "click ($ax,$ay) in the app's $($a.areaClick) area -- the step names no control"
            Start-Sleep -Milliseconds 400
            foreach ($t in @($a.seq)) {
                if ($t.kind -eq 'paste') { Set-Clipboard -Value $t.value; Send-Combo 'Ctrl+V' | Out-Null; Write-L1 "paste '$($t.value)'" }
                elseif ($t.kind -eq 'text') { Send-Text $t.value; Write-L1 "type '$($t.value)'" }
                elseif (Send-Combo $t.value) { Write-L1 "keys $($t.value)" } else { Write-L1 "keys not understood: $($t.value)" }
                Start-Sleep -Milliseconds 250
            }
        }
        elseif ($a.scrollOnly -and $tw) {
            if ($a.click) { $sx = $a.click[0]; $sy = $a.click[1] } else { $r = Get-WindowRect $tw.Hwnd; $sx = [int](($r.Left + $r.Right) / 2); $sy = [int](($r.Top + $r.Bottom) / 2) }
            Scroll-At $sx $sy $a.scroll
            Write-L1 "scroll $($a.scroll) at ($sx,$sy)"; $acted = $true
        }
        elseif (($a.named -or $a.clickWithoutPointer) -and -not $a.click -and -not $a.pointerOnly -and ($needsTarget -or $a.seq.Count -eq 0)) {
            # The step names a control the locator did not find. A follower acts on the pointer, so it
            # cannot click it or type into it -- pasting at the cursor instead put "20" into the
            # document text on the first live run (2026-10-11).
            Write-L1 "target '$($s.target_text)' not located; nothing to act on"
        }
        else {
            $before = @(); $after = @($a.seq)
            if ($a.click) { $before = @($a.seq | Where-Object { $_.pos -lt $a.clickPos }); $after = @($a.seq | Where-Object { $_.pos -ge $a.clickPos }) }
            if ($tw -and ($before.Count -gt 0 -or (-not $a.click -and $after.Count -gt 0))) {
                # Keys go to the foreground window. A dialog of the task app is fine; the panel
                # (after a typed reply) or anything else is not.
                $fgH = [L1W]::GetForegroundWindow()
                $fgPid = [uint32]0
                [L1W]::GetWindowThreadProcessId($fgH, [ref]$fgPid) | Out-Null
                if ($fgH -ne $tw.Hwnd -and ($fgPid -ne $targetPid -or $fgPid -eq $navPid)) { Focus-ByTitleBar $tw.Hwnd | Out-Null; Write-L1 "refocused the app" }
                elseif (-not $a.click) {
                    # Keys with no click: the hand rests over the middle of the app. Blender sends a
                    # key to the area under the mouse, and the last click may have left it elsewhere.
                    $r = Get-WindowRect $tw.Hwnd
                    [L1W]::SetCursorPos([int](($r.Left + $r.Right) / 2), [int](($r.Top + $r.Bottom) / 2)) | Out-Null
                }
                Start-Sleep -Milliseconds 300
            }
            foreach ($t in $before) {
                if ($t.kind -eq 'paste') { Set-Clipboard -Value $t.value; Send-Combo 'Ctrl+V' | Out-Null; Write-L1 "paste '$($t.value)'" }
                elseif ($t.kind -eq 'text') { Send-Text $t.value; Write-L1 "type '$($t.value)'" }
                elseif (Send-Combo $t.value) { Write-L1 "keys $($t.value)" } else { Write-L1 "keys not understood: $($t.value)" }
                $acted = $true; Start-Sleep -Milliseconds 250
            }
            if ($a.click) {
                $x = $a.click[0]; $y = $a.click[1]
                if ($ShotBeforeClick -and $tw) {
                    Add-Type -AssemblyName System.Drawing
                    $r = Get-WindowRect $tw.Hwnd
                    $bmp = New-Object System.Drawing.Bitmap ($r.Right - $r.Left), ($r.Bottom - $r.Top)
                    $g = [System.Drawing.Graphics]::FromImage($bmp)
                    $g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
                    $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::Magenta), 2
                    $g.DrawEllipse($pen, $x - $r.Left - 5, $y - $r.Top - 5, 10, 10)
                    $shot = Join-Path $work ("click-{0:D2}.png" -f ($clicks + 1))
                    $bmp.Save($shot); $g.Dispose(); $bmp.Dispose()
                }
                if ($task.misclick_at -and -not $misclicked -and ($clicks + 1) -eq [int]$task.misclick_at) {
                    $b = $s.located.bbox
                    $x = [int]($b.x + $b.width + 30); $misclicked = $true
                    Write-L1 "planted misclick at ($x,$y) instead of the target"
                    # The select-all the follower adds is for the field it meant to click. After a
                    # miss it selected the whole document and the paste replaced it (verification run,
                    # 2026-10-12). A person who missed types where focus is; the select-all goes.
                    $after = @($after | Where-Object { -not $_.before })
                }
                Click-Point $x $y -Button $a.button -Double:$a.double -Triple:([bool]$a.triple) -Modifier ([string]$a.clickMod)
                $clicks++; $acted = $true
                Write-L1 "click ($x,$y) $($a.button)$(if ($a.double) {' double'})$(if ($a.triple) {' triple'})$(if ($a.clickMod) {" with $($a.clickMod)"})"
                Start-Sleep -Milliseconds 400
            }
            foreach ($t in $after) {
                if ($t.kind -eq 'paste') { Set-Clipboard -Value $t.value; Send-Combo 'Ctrl+V' | Out-Null; Write-L1 "paste '$($t.value)'" }
                elseif ($t.kind -eq 'text') { Send-Text $t.value; Write-L1 "type '$($t.value)'" }
                elseif (Send-Combo $t.value) { Write-L1 "keys $($t.value)" } else { Write-L1 "keys not understood: $($t.value)" }
                $acted = $true; Start-Sleep -Milliseconds 250
            }
        }
        # A step that only reports the task finished ("Done -- the smaller copy is saved as
        # small.png") is the model saying it is done. Answering "I can't find that" ended a finished
        # run as stuck (verification run, 2026-10-12).
        if (-not $acted -and -not $s.target_text -and @($a.seq).Count -eq 0 -and -not $a.scroll -and -not $a.areaClick -and
            $s.instruction -match '(?i)\b(done|completed?|finished|already (?:done|saved|set|applied|in place)|nothing (?:else|more)|all set|is (?:now )?(?:saved|set|applied|in place|numbered))\b') {
            $claimedDone = $true
            $verdict = Get-Verdict $task $work
            if ($verdict.success) { $outcome = 'success' } else { $outcome = 'claimed_done' }
            Write-L1 "the step reports the task done"
            break
        }
        if (-not $acted) {
            $cantFind++
            if ($cantFind -gt 2) { $outcome = 'stuck'; break }
            $say = "I can't find that on my screen."
            if ($a.unsupported) { $say = "I can't do that with the mouse. Is there another way?" }
            elseif ($s.target_text) { $say = "I can't find `"$($s.target_text)`" on my screen." }
            Write-L1 ("persona: '$say' " + (Send-PanelText $navPid $say))
            continue
        }
        $steps++
        Start-Sleep -Milliseconds 1500
        $verdict = Get-Verdict $task $work
        if ($verdict.success) { $outcome = 'success'; break }
        if ($requests -ge $maxReq) { $outcome = 'cap_requests'; break }
        if (((Get-Date) - $t0).TotalMinutes -ge $maxMin) { $outcome = 'cap_time'; break }
        Send-NextHotkey
    }

    if (-not $verdict) { $verdict = Get-Verdict $task $work }
    $wall = [math]::Round(((Get-Date) - $t0).TotalSeconds, 1)
    $lines = @(Get-FeedLines $feed)
    $mine = @(); for ($i = $firstFeed; $i -lt $lines.Count; $i++) { $mine += ($lines[$i] | ConvertFrom-Json) }
    $inTok = ($mine | Where-Object { $_.input_tokens } | Measure-Object -Property input_tokens -Sum).Sum
    $outTok = ($mine | Where-Object { $_.output_tokens } | Measure-Object -Property output_tokens -Sum).Sum
    Write-L1 ("end: {0} success={1} progress={2} damage={3} requests={4} steps={5} wall={6}s {7}" -f $outcome, $verdict.success, $verdict.progress, $verdict.damage, $requests, $steps, $wall, $verdict.detail)
    Write-L1 ("teardown: " + (Invoke-TaskScript $task 'teardown' $work))
    return [ordered]@{
        task = $task.id; rep = $rep; tier = $Tier; provider = $cfg.provider; model = $cfg.model; effort = $cfg.effort
        outcome = $outcome; success = [bool]$verdict.success; progress = $verdict.progress; damage = [bool]$verdict.damage; detail = $verdict.detail
        claimed_done = $claimedDone; requests = $requests; steps = $steps; clicks = $clicks; cant_find = $cantFind
        replies = $replies; errors = $errors; planted_misclick = $misclicked; hint_steps = $hintSteps; wall_s = $wall
        input_tokens = $inTok; output_tokens = $outTok; feed_from = $firstFeed; feed_to = $lines.Count
        est_usd = [math]::Round(($inTok * $cfg.usdIn + $outTok * $cfg.usdOut) / 1e6, 5)
        app_commit = $appCommit; exe_time = $exeTime; held_out = [bool]$task.held_out
        harness = $Harness; learned_notes = [bool]$LearnedNotes
        run_dir = $runDir; ts = (Get-Date -Format o)
    }
}

try {
    $spent = 0.0
    :outer for ($rep = 1; $rep -le $Repeat; $rep++) {
        foreach ($task in $picked) {
            if ($spent -ge $MaxUsd) { Write-L1 ("spend cap reached: {0:N2} of {1:N2} USD; stopping" -f $spent, $MaxUsd); break outer }
            # Pause between runs while a PAUSE file sits in the run root: the founder may need the
            # machine. Each run snapshots open windows at its start, so anything opened meanwhile
            # is protected from teardown.
            $pauseFile = Join-Path $RunRoot 'PAUSE'
            if (Test-Path $pauseFile) {
                Write-L1 "paused ($pauseFile present)"
                while (Test-Path $pauseFile) { Start-Sleep -Seconds 5 }
                Write-L1 "resumed"
            }
            # One run's harness failure is recorded and the pass goes on; it never ends the tier.
            try { $row = Invoke-L1Run $task $rep }
            catch {
                Write-L1 "harness error in $($task.id) r${rep}: $($_.Exception.Message)"
                $w = Join-Path $runDir "$($task.id)-r$rep"
                try { Write-L1 ("teardown: " + (Invoke-TaskScript $task 'teardown' $w)) } catch { }
                $row = [ordered]@{ task = $task.id; rep = $rep; tier = $Tier; provider = $cfg.provider; model = $cfg.model
                    outcome = 'harness_error'; success = $false; detail = $_.Exception.Message; held_out = [bool]$task.held_out
                    app_commit = $appCommit; exe_time = $exeTime; run_dir = $runDir; ts = (Get-Date -Format o) }
            }
            if ($row.est_usd) { $spent += [double]$row.est_usd }
            Add-Content -Path $resultsFile -Value ($row | ConvertTo-Json -Compress) -Encoding UTF8
        }
    }
    Write-L1 ("estimated spend this tier: {0:N3} USD" -f $spent)
} finally {
    Stop-Process -Id $navPid -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 800
    # The .env copy may hold API keys; the logs and the feed stay for analysis.
    Remove-Item -LiteralPath (Join-Path $data '.env') -Force -ErrorAction SilentlyContinue
    Write-L1 "Navisual stopped; results: $resultsFile; run folder: $runDir"
}
