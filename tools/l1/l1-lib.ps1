# L1 live harness -- shared helpers. Dot-source: . "$PSScriptRoot\l1-lib.ps1"
# Pure ASCII (CLAUDE.md rule 12).
#
# The driver is a follower, not a person (harness-plan.md, L1): it does what the pointer and the
# instruction say, through real mouse and keyboard input, and nothing else. Every decision it takes
# is written to the run log so a failure can be told apart from a driver mistake.

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public delegate bool L1EnumProc(IntPtr h, IntPtr l);
public struct L1Rect { public int Left, Top, Right, Bottom; }
public struct L1Point { public int X, Y; }
public static class L1W {
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
    [DllImport("user32.dll")] public static extern bool EnumWindows(L1EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll", EntryPoint="GetWindowLongW")] public static extern int GetWindowLong(IntPtr h, int i);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out L1Rect r);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool repaint);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out L1Point p);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, IntPtr e);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, IntPtr extra);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", SetLastError=true)] public static extern uint SendInput(uint n, INPUT[] inputs, int size);
    [DllImport("kernel32.dll")] public static extern uint SetThreadExecutionState(uint flags);

    [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
    [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
    [StructLayout(LayoutKind.Explicit)] public struct INPUTUNION { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }
    [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public INPUTUNION u; }

    // Text as Unicode key events, one character at a time: what a person typing produces.
    public static void TypeText(string s) {
        foreach (char c in s) {
            INPUT[] a = new INPUT[2];
            a[0].type = 1; a[0].u.ki.wScan = c; a[0].u.ki.dwFlags = 0x0004;
            a[1].type = 1; a[1].u.ki.wScan = c; a[1].u.ki.dwFlags = 0x0004 | 0x0002;
            SendInput(2, a, Marshal.SizeOf(typeof(INPUT)));
            System.Threading.Thread.Sleep(15);
        }
    }

    public static string Title(IntPtr h) { var sb = new StringBuilder(512); GetWindowTextW(h, sb, 512); return sb.ToString(); }
    public static string Class(IntPtr h) { var sb = new StringBuilder(256); GetClassNameW(h, sb, 256); return sb.ToString(); }
}
"@ -ErrorAction SilentlyContinue

# A run that lasts all night must not meet the screen saver, a locked screen or sleep: every task
# after it would fail on a desktop it cannot see. Held while this process runs; released at exit.
function Keep-Awake { [L1W]::SetThreadExecutionState([uint32]2147483651) | Out-Null }

# Physical pixels, as the locator reports them (LocateResult.bbox: physical, virtual desktop).
[L1W]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null

$global:L1AE = [System.Windows.Automation.AutomationElement]
$global:L1TS = [System.Windows.Automation.TreeScope]
$global:L1CT = [System.Windows.Automation.ControlType]

# ---------------------------------------------------------------------------------------------
# Logging

$global:L1Log = $null
function Write-L1([string]$msg) {
    $line = "{0} {1}" -f (Get-Date -Format "HH:mm:ss.fff"), $msg
    Write-Host $line
    if ($global:L1Log) { Add-Content -Path $global:L1Log -Value $line -Encoding UTF8 }
}

# ---------------------------------------------------------------------------------------------
# Windows

# Every visible top-level window, filtered by $Where (called with one window object).
# The predicate runs in this function's scope chain (PowerShell scriptblocks are not closures), so
# every local here carries an __l1 prefix: a caller's `$h` once matched this loop's own `$h`, the
# filter held for every window, and a teardown closed all of the founder's Explorer windows
# (2026-10-11).
function Get-TopWindows([scriptblock]$Where) {
    $global:L1Hits = New-Object System.Collections.Generic.List[IntPtr]
    $__l1cb = [L1EnumProc]{
        param($__l1hw, $__l1lp)
        if ([L1W]::IsWindowVisible($__l1hw)) { $global:L1Hits.Add($__l1hw) }
        return $true
    }
    [L1W]::EnumWindows($__l1cb, [IntPtr]::Zero) | Out-Null
    $__l1out = @()
    foreach ($__l1h in $global:L1Hits) {
        $__l1pid = [uint32]0
        [L1W]::GetWindowThreadProcessId($__l1h, [ref]$__l1pid) | Out-Null
        $__l1o = [pscustomobject]@{ Hwnd = $__l1h; Pid = [int]$__l1pid; Title = [L1W]::Title($__l1h); Class = [L1W]::Class($__l1h); ExStyle = [L1W]::GetWindowLong($__l1h, -20) }
        if (-not $Where -or (& $Where $__l1o)) { $__l1out += $__l1o }
    }
    return $__l1out
}

function Get-WindowRect([IntPtr]$h) {
    $r = New-Object L1Rect
    [L1W]::GetWindowRect($h, [ref]$r) | Out-Null
    return $r
}

# The Navisual panel: the process's visible window that is not click-through (the overlay is).
function Get-PanelWindow([int]$NavPid) {
    Get-TopWindows { param($w) $w.Pid -eq $NavPid -and (($w.ExStyle -band 0x20) -eq 0) -and $w.Title -eq 'Navisual' } | Select-Object -First 1
}

# Bring a window forward the way a person does: a real click on its title bar.
function Focus-ByTitleBar([IntPtr]$h) {
    $r = Get-WindowRect $h
    $x = [int](($r.Left + $r.Right) / 2)
    $y = $r.Top + 12
    Click-Point $x $y
    Start-Sleep -Milliseconds 300
    if ([L1W]::GetForegroundWindow() -ne $h) {
        [L1W]::SetForegroundWindow($h) | Out-Null
        Start-Sleep -Milliseconds 200
    }
    # Leave the mouse where a person's rests, over the work area, not on the title bar: Blender sends
    # a key to the area under the mouse (F3 on the title bar searched nothing, live 2026-10-11).
    [L1W]::SetCursorPos($x, [int](($r.Top + $r.Bottom) / 2)) | Out-Null
    return ([L1W]::GetForegroundWindow() -eq $h)
}

# Windows that were open before a run's setup. Close-Window never touches them, whatever a task's
# own filter says -- the guard for the 2026-10-11 teardown that closed the founder's windows.
$global:L1Protected = @()
function Protect-OpenWindows { $global:L1Protected = @(Get-TopWindows | ForEach-Object { [long]$_.Hwnd }) }

# Ask a window to close (WM_CLOSE): goes to that window only, whatever has focus.
function Close-Window([IntPtr]$hwnd) {
    if ($global:L1Protected -contains [long]$hwnd) {
        Write-Host "refused to close window $hwnd '$([L1W]::Title($hwnd))': it was open before the run"
        return
    }
    [L1W]::PostMessage($hwnd, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
}

# Close a window until it is gone: Explorer takes WM_CLOSE as "close this tab" when it has two.
function Close-WindowFully([IntPtr]$hwnd, [int]$tries = 6) {
    for ($i = 0; $i -lt $tries; $i++) {
        if (-not (Get-TopWindows { param($x) $x.Hwnd -eq $hwnd })) { return $true }
        Close-Window $hwnd
        if ($global:L1Protected -contains [long]$hwnd) { return $false }
        Start-Sleep -Milliseconds 700
    }
    return -not (Get-TopWindows { param($x) $x.Hwnd -eq $hwnd })
}

# ---------------------------------------------------------------------------------------------
# Mouse and keyboard

function Click-Point([int]$x, [int]$y, [string]$Button = 'left', [switch]$Double) {
    # Travel there as a hand does rather than jump: GTK and Paint keep a submenu open only while the
    # pointer's path stays on its way into it (trial, 2026-10-11: GIMP's Flip Horizontally and
    # Paint's PNG picture clicks landed on closed menus).
    $p = New-Object L1Point
    [L1W]::GetCursorPos([ref]$p) | Out-Null
    for ($i = 1; $i -le 10; $i++) {
        [L1W]::SetCursorPos([int]($p.X + ($x - 6 - $p.X) * $i / 10), [int]($p.Y + ($y - $p.Y) * $i / 10)) | Out-Null
        Start-Sleep -Milliseconds 18
    }
    # Arrive, then settle inside: XAML menu items ignore a click with no hover first (seen live).
    [L1W]::SetCursorPos($x - 6, $y) | Out-Null
    Start-Sleep -Milliseconds 120
    [L1W]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 220
    $down = 0x2; $up = 0x4
    if ($Button -eq 'right') { $down = 0x8; $up = 0x10 }
    $n = 1; if ($Double) { $n = 2 }
    for ($i = 0; $i -lt $n; $i++) {
        [L1W]::mouse_event($down, 0, 0, 0, [IntPtr]::Zero)
        Start-Sleep -Milliseconds 60
        [L1W]::mouse_event($up, 0, 0, 0, [IntPtr]::Zero)
        Start-Sleep -Milliseconds 90
    }
}

$global:L1VK = @{
    'ctrl' = 0x11; 'control' = 0x11; 'alt' = 0x12; 'shift' = 0x10; 'win' = 0x5B; 'windows' = 0x5B;
    'enter' = 0x0D; 'return' = 0x0D; 'tab' = 0x09; 'esc' = 0x1B; 'escape' = 0x1B; 'space' = 0x20;
    'delete' = 0x2E; 'del' = 0x2E; 'backspace' = 0x08; 'home' = 0x24; 'end' = 0x23;
    'up' = 0x26; 'down' = 0x28; 'left' = 0x25; 'right' = 0x27; 'pageup' = 0x21; 'pagedown' = 0x22;
    'insert' = 0x2D; 'backquote' = 0xC0; '`' = 0xC0; 'minus' = 0xBD; '-' = 0xBD; 'plus' = 0xBB; '=' = 0xBB;
    '+' = 0xBB; ',' = 0xBC; '.' = 0xBE; '/' = 0xBF; ';' = 0xBA; '[' = 0xDB; ']' = 0xDD
}

function Get-VkCode([string]$k) {
    $k = $k.Trim().ToLower() -replace '\s', ''
    if ($global:L1VK.ContainsKey($k)) { return $global:L1VK[$k] }
    if ($k -match '^f([0-9]{1,2})$') { return 0x6F + [int]$Matches[1] }
    if ($k.Length -eq 1 -and $k -match '[a-z0-9]') { return [int][char]$k.ToUpper() }
    return $null
}

# "Ctrl+Shift+N", "Alt+F4", "Enter".
function Send-Combo([string]$combo) {
    $parts = $combo -split '\+' | Where-Object { $_.Trim() -ne '' }
    if ($combo.Trim().EndsWith('++')) { $parts += '+' }
    $codes = @()
    foreach ($p in $parts) {
        $c = Get-VkCode $p
        if ($null -eq $c) { return $false }
        $codes += [byte]$c
    }
    # Navigation keys are extended keys. Sent without the flag they are the numeric keypad's, and
    # with Num Lock on, Shift+End then moves instead of selecting (live, 2026-10-11: the model kept
    # asking for a selection that never appeared).
    $ext = @(0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x2D, 0x2E, 0x5B)
    foreach ($c in $codes) { $f = 0; if ($ext -contains $c) { $f = 1 }; [L1W]::keybd_event($c, 0, $f, [IntPtr]::Zero); Start-Sleep -Milliseconds 30 }
    Start-Sleep -Milliseconds 50
    [array]::Reverse($codes)
    foreach ($c in $codes) { $f = 2; if ($ext -contains $c) { $f = 3 }; [L1W]::keybd_event($c, 0, $f, [IntPtr]::Zero); Start-Sleep -Milliseconds 20 }
    return $true
}

# Text goes in as Unicode key events by default: they pass an input method untouched, where virtual
# keys would land in a Chinese IME's composition. Blender (GHOST) ignores Unicode events, so a task
# can ask for virtual keys instead ("text_input": "keys" in task.json).
$global:L1TextMode = 'unicode'
function Send-Text([string]$s) {
    if ($global:L1TextMode -ne 'keys') { [L1W]::TypeText($s); return }
    foreach ($ch in $s.ToCharArray()) {
        $c = [string]$ch
        if ($c -cmatch '[A-Z]') { Send-Combo "Shift+$c" | Out-Null }
        elseif ($c -match '[a-z0-9]') { Send-Combo $c | Out-Null }
        elseif ($c -eq ' ') { Send-Combo 'Space' | Out-Null }
        elseif ($c -eq '.') { [L1W]::keybd_event(0xBE, 0, 0, [IntPtr]::Zero); [L1W]::keybd_event(0xBE, 0, 2, [IntPtr]::Zero) }
        elseif ($c -eq '-') { [L1W]::keybd_event(0xBD, 0, 0, [IntPtr]::Zero); [L1W]::keybd_event(0xBD, 0, 2, [IntPtr]::Zero) }
        else { [L1W]::TypeText($c) }
        Start-Sleep -Milliseconds 25
    }
}

# Press, move, release: a person dragging across text or along a slider.
function Drag-Between([int]$x0, [int]$y0, [int]$x1, [int]$y1) {
    [L1W]::SetCursorPos($x0, $y0) | Out-Null
    Start-Sleep -Milliseconds 200
    [L1W]::mouse_event(0x2, 0, 0, 0, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 120
    for ($i = 1; $i -le 12; $i++) {
        [L1W]::SetCursorPos([int]($x0 + ($x1 - $x0) * $i / 12), [int]($y0 + ($y1 - $y0) * $i / 12)) | Out-Null
        Start-Sleep -Milliseconds 30
    }
    Start-Sleep -Milliseconds 120
    [L1W]::mouse_event(0x4, 0, 0, 0, [IntPtr]::Zero)
}

# Mouse wheel over a point: positive notches scroll up, negative down.
function Scroll-At([int]$x, [int]$y, [int]$notches) {
    [L1W]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 150
    $delta = [BitConverter]::ToUInt32([BitConverter]::GetBytes([int](120 * [math]::Sign($notches))), 0)
    for ($i = 0; $i -lt [math]::Abs($notches); $i++) {
        [L1W]::mouse_event(0x0800, 0, 0, $delta, [IntPtr]::Zero)
        Start-Sleep -Milliseconds 60
    }
}

# ---------------------------------------------------------------------------------------------
# UIA

function Find-ByName($root, [string]$name, [string]$type = '') {
    $c = New-Object System.Windows.Automation.PropertyCondition ($global:L1AE::NameProperty), $name
    $all = $root.FindAll($global:L1TS::Descendants, $c)
    foreach ($e in $all) {
        if (-not $type -or $e.Current.ControlType.ProgrammaticName -eq "ControlType.$type") { return $e }
    }
    return $null
}

function Find-ByType($root, [string]$type) {
    $c = New-Object System.Windows.Automation.PropertyCondition ($global:L1AE::ControlTypeProperty), ($global:L1CT::$type)
    return @($root.FindAll($global:L1TS::Descendants, $c))
}

function Invoke-Element($e) {
    $e.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
}

# ---------------------------------------------------------------------------------------------
# The Navisual panel, driven as a person would through its own controls.

function Get-PanelRoot([int]$NavPid) {
    $w = Get-PanelWindow $NavPid
    if (-not $w) { return $null }
    return $global:L1AE::FromHandle($w.Hwnd)
}

# Type into the task box and press its button ("Guide me", or the follow-up button mid-task).
function Send-PanelText([int]$NavPid, [string]$text) {
    $root = Get-PanelRoot $NavPid
    if (-not $root) { return "no panel" }
    $box = Find-ByType $root 'Edit' | Select-Object -First 1
    if (-not $box) { return "no task box" }
    $box.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($text)
    Start-Sleep -Milliseconds 300
    $btn = Find-ByType $root 'Button' | Where-Object {
        $n = $_.Current.Name
        $n -eq 'Guide me' -or $n -like '*Follow up*' -or $n -like '*Send answer*' -or $n -like '*Send' -or $n -like 'Reply*'
    } | Select-Object -First 1
    if (-not $btn) { return "no send button" }
    $name = $btn.Current.Name
    Invoke-Element $btn
    return "sent via '$name'"
}

function Start-PanelNewTask([int]$NavPid) {
    $root = Get-PanelRoot $NavPid
    if (-not $root) { return "no panel" }
    $btn = Find-ByType $root 'Button' | Where-Object { $_.Current.Name -match 'New task' } | Select-Object -First 1
    if (-not $btn) { return "no New task button (already fresh?)" }
    Invoke-Element $btn
    Start-Sleep -Milliseconds 500
    return "new task"
}

function Send-NextHotkey { Send-Combo 'Ctrl+Backquote' | Out-Null }

# ---------------------------------------------------------------------------------------------
# The step feed (NAVISUAL_STEP_FEED): one JSON line per response the panel receives.

# The app appends to the feed while this reads it. Open it shared and retry: a plain ReadAllLines
# hit the writer's handle mid-append and the exception ended a whole tier of the scored pass
# (2026-10-11, after 10 of 45 runs). A partial last line is left for the next read.
function Get-FeedLines([string]$path) {
    if (-not (Test-Path $path)) { return @() }
    for ($i = 0; $i -lt 10; $i++) {
        try {
            $fs = [System.IO.File]::Open($path, 'Open', 'Read', 'ReadWrite, Delete')
            try {
                $sr = New-Object System.IO.StreamReader($fs, [System.Text.Encoding]::UTF8)
                $text = $sr.ReadToEnd()
            } finally { $fs.Dispose() }
            $lines = @($text -split "`r?`n" | Where-Object { $_.Trim() -ne '' })
            if ($lines.Count -gt 0 -and -not $text.EndsWith("`n")) { $lines = @($lines | Select-Object -First ($lines.Count - 1)) }
            return $lines
        } catch { Start-Sleep -Milliseconds 100 }
    }
    return @()
}

function Wait-FeedLine([string]$path, [int]$afterCount, [int]$timeoutSec) {
    $deadline = (Get-Date).AddSeconds($timeoutSec)
    while ((Get-Date) -lt $deadline) {
        $lines = @(Get-FeedLines $path)
        if ($lines.Count -gt $afterCount) {
            return [pscustomobject]@{ Count = $lines.Count; Item = ($lines[$afterCount] | ConvertFrom-Json) }
        }
        Start-Sleep -Milliseconds 250
    }
    return $null
}

# ---------------------------------------------------------------------------------------------
# What a step asks the follower to do, read from the step alone.

function Get-StepActions($step) {
    $ins = [string]$step.instruction
    $a = [ordered]@{ click = $null; button = 'left'; double = $false; seq = @(); scroll = 0; scrollOnly = $false; unsupported = $null; clickPos = 0; named = $false }
    if ($step.located -and $step.located.bbox) {
        $b = $step.located.bbox
        $cx = [int]($b.x + $b.width / 2)
        # A box much wider than tall -- a list row, a long text field -- is aimed at near its label,
        # as a person does. Its centre can be blank row space: right-clicking an Explorer row there
        # opened the background menu ten times running (trial, 2026-10-11).
        if ($b.width -ge 250 -and $b.width -ge 6 * $b.height) { $cx = [int]($b.x + [math]::Min(60, $b.width * 0.2)) }
        $a.click = @($cx, [int]($b.y + $b.height / 2))
    }
    # As verbs only: "in the open right-click menu, point to Compress to..." is not a right-click.
    if ($ins -match '(?i)\bright[- ]click(?!\s+menu)\b') { $a.button = 'right' }
    if ($ins -match '(?i)\bdouble[- ]click\b(?!\s+(?:on\s+)?(?:it|the\s+\w+)\s+to\s+rename)') { $a.double = $true }
    # A drag across one target (selecting its text) runs from its left edge to its right; a drag from
    # one thing onto another needs two points the step does not give, so the follower cannot do it.
    $a.dragAcross = $false
    if ($ins -match '(?i)\bdrag\b') {
        if ($ins -match '(?i)\bdrag\s+(?:your\s+mouse\s+|the\s+(?:mouse|pointer)\s+)?(?:across|over|through)\b|\bdrag\s+to\s+select\b|\bselect\b.*\bby\s+dragging\b' -and $step.located) { $a.dragAcross = $true }
        else { $a.unsupported = 'drag between two places' }
    }
    # "Scroll down", "scroll farther down", "scroll all the way down", "scroll to the bottom". The
    # founder watched "all the way down" go unread: only two words were allowed between.
    $dir = 0
    if ($ins -match '(?i)\bscroll\b(?:\s+[a-z]+){0,6}?\s+(down|up)\b') { if ($Matches[1] -ieq 'down') { $dir = -1 } else { $dir = 1 } }
    elseif ($ins -match '(?i)\bscroll\b(?:\s+[a-z]+){0,4}?\s+to\s+(?:the|its)\s+(bottom|end|top)\b') { if ($Matches[1] -ieq 'top') { $dir = 1 } else { $dir = -1 } }
    if ($dir -ne 0) {
        $far = $ins -match '(?i)\b(all the way|to (the|its) (bottom|end|top)|until\b.{0,60}\b(bottom|end|footer|last))'
        $a.scroll = $dir * $(if ($far) { 15 } else { 8 })
    }
    # "Scroll down over the report" with a pointer on the report: the act is the scroll, not a click.
    if ($a.scroll -ne 0 -and $ins -notmatch '(?i)\b(click|select|choose|tick|double-click)\b') { $a.scrollOnly = $true }
    # Keys and text, in the order the instruction gives them ("press G, then Z, type 2, press Enter").
    $mods = '(?:ctrl|control|alt|shift|win)'
    $key = '(?:f[0-9]{1,2}|enter|return|tab|esc|escape|delete|del|backspace|home|end|space|up|down|left|right|page ?up|page ?down|[a-z0-9`=\-])'
    $combo = '(?<combo>\b' + $mods + '(?:\s*\+\s*' + $mods + ')*\s*\+\s*' + $key + '\b)'
    # "press G, Z, 2, Enter": a run of keys after one verb, each pressed in turn.
    $press = '(?<press>\b(?:press|hit|tap)\s+(?:the\s+)?(?<pk>' + $key + '(?:\s*(?:,\s*(?:and\s+|then\s+)?|\s+then\s+|\s+and\s+)' + $key + ')*)(?:\s+key)?\b)'
    # "press G, then Z": a lone capital after "then" is a key. Not "then press G" -- that belongs to
    # the press run, which would otherwise lose everything after G.
    # Only where the letter stands alone as a key ("then Z," / "then Z to"): "then I'll guide you"
    # pressed I inside GIMP's open menu (trial, 2026-10-11). I and A are never read this way.
    $then = '(?<then>\bthen\s+(?<tk>(?-i:[B-HJ-Z]))(?=\s*(?:,|\.|;|$)|\s+(?:and|then|to)\b))'
    $named = '(?:f[0-9]{1,2}|enter|tab|esc|escape|delete|backspace|home|end|space|page ?up|page ?down)'
    $with = '(?<with>\b(?:with|using)\s+(?:the\s+)?(?<wk>' + $named + ')(?:\s+key)?\b)'
    $type = '(?<type>\b(?:type|enter|paste|write)\s+(?:[a-z]+\s+){0,4}?(?:["\u201C\u2018''](?<tq>[^"\u201D\u2019'']{1,200})["\u201D\u2019'']|(?<tn>-?[0-9]+(?:\.[0-9]+)?)\b))'
    # Ribbon key tips, "Alt+JH" or "Alt, J, H": Alt alone, then each letter in turn.
    # Key tips are capitals: "Alt+JH" as a run, or single letters after "Alt," joined by commas, "then"
    # or "and" ("Press Alt, then J and H"). Lower-case runs are words: "Alt, then" once pressed T, H, E, N.
    $tips = '(?<tips>\balt\s*\+\s*(?<tl>(?-i:[A-Z0-9]{2,4}))\b|\balt\s*,\s*(?:then\s+)?(?<tl>(?-i:[A-Z0-9])(?:\s*(?:,\s*(?:then\s+|and\s+)?|,?\s+then\s+|,?\s+and\s+)(?-i:[A-Z0-9])){1,3})\b)'
    $seq = New-Object System.Collections.ArrayList
    foreach ($m in [regex]::Matches($ins, '(?i)' + $tips + '|' + $combo + '|' + $press + '|' + $then + '|' + $with + '|' + $type)) {
        $pos = $m.Index
        if ($m.Groups['tips'].Success) {
            $letters = ($m.Groups['tl'].Value -replace '(?i)[\s,]|\bthen\b|\band\b', '')
            if ($letters -match '(?i)^(f[0-9]{1,2}|tab|esc|enter|space|home|end|up|down|del)$') {
                # Alt+F4, Alt+Tab: a chord, not key tips.
                [void]$seq.Add(@{ kind = 'keys'; value = "Alt+$letters"; pos = $pos })
            } else {
                [void]$seq.Add(@{ kind = 'keys'; value = 'Alt'; pos = $pos })
                $i = 1
                foreach ($ch in $letters.ToCharArray()) { [void]$seq.Add(@{ kind = 'keys'; value = [string]$ch; pos = $pos + 0.01 * $i }); $i++ }
            }
        }
        elseif ($m.Groups['combo'].Success) { [void]$seq.Add(@{ kind = 'keys'; value = ($m.Groups['combo'].Value -replace '\s', ''); pos = $pos }) }
        elseif ($m.Groups['press'].Success) {
            $i = 0
            foreach ($k in ($m.Groups['pk'].Value -split '\s*,\s*(?:and\s+|then\s+)?|\s+then\s+|\s+and\s+')) {
                # Sort-Object is not stable: keys of one run keep their order by a small step each.
                if ($k.Trim()) { [void]$seq.Add(@{ kind = 'keys'; value = $k.Trim(); pos = $pos + 0.01 * $i }); $i++ }
            }
        }
        elseif ($m.Groups['then'].Success) { [void]$seq.Add(@{ kind = 'keys'; value = $m.Groups['tk'].Value; pos = $pos }) }
        elseif ($m.Groups['with'].Success) { [void]$seq.Add(@{ kind = 'keys'; value = $m.Groups['wk'].Value; pos = $pos }) }
        elseif ($m.Groups['type'].Success) {
            $t = $m.Groups['tq'].Value; if (-not $t) { $t = $m.Groups['tn'].Value }
            [void]$seq.Add(@{ kind = 'text'; value = $t; pos = $pos })
        }
    }
    # Where the click falls among them: where the instruction names the target, else at its first
    # click verb, else first. "Select the title with Home, then Shift+End. Click the Font Size box,
    # replace its value with 20" -- the keys come before the click (live, 2026-10-11).
    $clickPos = 0
    $tt = [string]$step.target_text
    if ($tt -and $ins.IndexOf($tt, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) { $clickPos = $ins.IndexOf($tt, [System.StringComparison]::OrdinalIgnoreCase) }
    elseif ($ins -match '(?i)\b(click|select|choose|tick|open|double-click|right-click)\b') { $clickPos = $ins.IndexOf($Matches[0], [System.StringComparison]::OrdinalIgnoreCase) }
    $a.clickPos = $clickPos
    $a.named = [bool]$tt
    # Rule 11 puts any text to type on the clipboard: the first "type ..." pastes it; with no such
    # verb, the clipboard is pasted right after the click.
    if ($step.clipboard) {
        $first = $null
        foreach ($t in $seq) { if ($t.kind -eq 'text') { $first = $t; break } }
        if ($first) { $first.kind = 'paste'; $first.value = [string]$step.clipboard }
        else {
            $pp = $clickPos + 1
            if ($ins -match '(?i)\b(paste|type)\b') { $pp = $ins.IndexOf($Matches[1], [System.StringComparison]::OrdinalIgnoreCase) }
            # "press Ctrl+A, and replace it with colour": the text goes where "with" stands, after the
            # keys before it (pasting first, then selecting, was the trial's order).
            elseif ($ins -match '(?i)\breplace\b[^.]*?\bwith\b') { $pp = $Matches[0].Length + $ins.IndexOf($Matches[0], [System.StringComparison]::OrdinalIgnoreCase) - 4 }
            [void]$seq.Add(@{ kind = 'paste'; value = [string]$step.clipboard; pos = $pp })
        }
    }
    # "Replace the text in the File name field with small.png": a person selects what is there first.
    # Clicked into, the field holds a caret and pasting appends (garden.pngsmall.png, trial).
    if (@($seq | Where-Object { $_.kind -eq 'paste' }).Count -gt 0 -and
        $ins -match '(?i)\b(replace|select all|overwrite)\b' -and $ins -match '(?i)\b(field|box|name|value|text)\b' -and
        @($seq | Where-Object { $_.kind -eq 'keys' -and $_.value -match '(?i)^ctrl\+a$' }).Count -eq 0) {
        $pt = $seq | Where-Object { $_.kind -eq 'paste' } | Select-Object -First 1
        [void]$seq.Add(@{ kind = 'keys'; value = 'Ctrl+A'; pos = -1; before = $pt })
    }
    # "Enter <text> in the Subject field": the text goes into the target, so after the click on it.
    foreach ($t in $seq) {
        if ($t.kind -in 'paste', 'text' -and $t.pos -lt $clickPos) {
            $between = $ins.Substring($t.pos, $clickPos - $t.pos)
            if ($between -match '(?i)\b(in|into|to)\s+(the\s+)?[^.;]*$') { $t.pos = $clickPos + 0.5 }
        }
    }
    # Text for a field the step does not point at is not the follower's to type: "enter colour in the
    # Find box, then enter color in the Replace box" with the pointer on Find put both words into
    # Find (trial, 2026-10-11). A later text that names a different box or field is skipped.
    $a.skipped = @()
    $texts = @($seq | Where-Object { $_.kind -in 'paste', 'text' } | Sort-Object { $_.pos })
    for ($k = 1; $k -lt $texts.Count; $k++) {
        $tail = $ins.Substring([math]::Min([int]$texts[$k].pos, $ins.Length))
        if ($tail.Length -gt 90) { $tail = $tail.Substring(0, 90) }
        if ($tail -match '(?i)\b(?:in|into)\s+the\s+(.{1,40}?)\s+(?:box|field)\b' -and (-not $tt -or $Matches[1] -notmatch [regex]::Escape($tt))) {
            $a.skipped += "$($texts[$k].value) (for the $($Matches[1]) field)"
            [void]$seq.Remove($texts[$k])
        }
    }
    # A pointer is not an order to click. Click when the instruction asks for one, when it types into
    # the control it points at, or when it is only keys-free pointing ("Turn on Word wrap"). "Now press
    # Ctrl+A to select all three" with the pointer on one file got a click after the select-all, and
    # the selection was gone (trial, 2026-10-11). "to select" is a purpose, not an order.
    $verb = $ins -match '(?i)(?<!\bto\s)\b(click|tap|double-click|right-click|choose|open|tick|untick|check|uncheck|pick|select|hover|point to)\b'
    # Typing into the control pointed at: "in/into the <target>" or "the <target> box/field". Merely
    # naming it is not enough -- "The cylinder is already added. Press G, then Z, type 2" clicked the
    # Cylinder row, and Blender then sent the keys to the outliner (trial, 2026-10-11).
    $intoTarget = $false
    if ($tt -and @($seq | Where-Object { $_.kind -in 'paste', 'text' }).Count -gt 0) {
        $e = [regex]::Escape($tt)
        $intoTarget = ($ins -match "(?i)\b(?:in|into|to)\s+(?:the\s+)?(?:empty\s+|blank\s+|focused\s+)?$e") -or ($ins -match "(?i)$e\W{0,3}\s+(?:box|field)\b")
    }
    $keysOrText = @($seq).Count -gt 0
    if ($a.click -and -not $verb -and -not $intoTarget -and $keysOrText) { $a.click = $null; $a.pointerOnly = $true }
    # The opposite case: the step says to click something and there is no pointer at all. Text meant
    # for that thing must not go wherever focus happens to be ("Click in the blank rectangle to the
    # right of Entries, then paste" with no pointer pasted Yes/No into the focused control, trial).
    $a.clickWithoutPointer = (-not $step.located) -and $verb
    # The select-all sits just before the paste it serves, wherever that paste ended up.
    foreach ($t in $seq) { if ($t.before) { $t.pos = $t.before.pos - 0.001 } }
    $a.seq = @($seq | Sort-Object { $_.pos })
    return [pscustomobject]$a
}
