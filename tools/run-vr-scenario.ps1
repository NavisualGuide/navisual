# Run one Vision/Reasoning scenario against a NAMED target app.
#
# Why this exists: run-single-task.ps1 real-clicks Navisual's own task box to type the
# task, which makes Navisual the foreground window. Navisual then auto-detects the target
# as the last non-Navisual foreground app -- which is whatever the OS happened to have
# foregrounded, NOT the app the scenario is about. Measured 2026-09-20: V1 ran against
# File Explorer (opened minutes earlier) and the model correctly replied "bring the
# spreadsheet into focus so I can see it". A correct answer to the wrong question.
#
# PROTOCOL (settled 2026-09-20 after three focus failures): every fixture app stays
# MINIMIZED between scenarios, and this script restores exactly one. The earlier layout
# had all five maximized to the identical rect, so a click at those coordinates landed on
# whichever window happened to be on top, and Windows' foreground lock refused to reorder
# them from a background process -- SetForegroundWindow measured returning False outright.
# Restoring the only minimized window sidesteps the z-order fight entirely: there is
# nothing on top of it to steal the click.
param(
    [Parameter(Mandatory)][string]$TitleMatch,   # substring of the target window title
    [Parameter(Mandatory)][string]$ModelLabel,
    [Parameter(Mandatory)][string]$ScenarioLabel,
    [Parameter(Mandatory)][string]$TaskText,
    [switch]$IsFollowup,
    [switch]$MinimizeAfter,                      # leave the desktop clean for the next scenario
    [int]$TimeoutSec = 60
)

Add-Type -TypeDefinition @"
using System; using System.Runtime.InteropServices; using System.Text;
public class VrWin32 {
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, UIntPtr e);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int X, int Y);
    public delegate bool EnumWindowsProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc f, IntPtr l);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int c);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder s, int c);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out VRRECT r);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [StructLayout(LayoutKind.Sequential)] public struct VRRECT { public int L, T, R, B; }
}
"@ -ErrorAction SilentlyContinue

function Get-VrTitle($h) {
    $sb = New-Object System.Text.StringBuilder 300
    [VrWin32]::GetWindowText($h, $sb, 300) | Out-Null
    return $sb.ToString()
}
function Get-VrPid($h) {
    $procId = [uint32]0
    [VrWin32]::GetWindowThreadProcessId($h, [ref]$procId) | Out-Null
    return $procId
}
function Get-VrClass($h) {
    $sb = New-Object System.Text.StringBuilder 300
    [VrWin32]::GetClassName($h, $sb, 300) | Out-Null
    return $sb.ToString()
}

# Match on title alone. Deliberately NOT filtered on the window rect: under this protocol
# the target is minimized, and Windows parks a minimized window at -32000,-32000, so the
# old "rect must be on screen" guard rejected every window this script now expects to find.
# A UWP app (Settings, Task Manager on Win11) publishes TWO top-level windows carrying
# the same title: the visible ApplicationFrameWindow host, and an internal
# Windows.UI.Core.CoreWindow. Measured 2026-09-20: the CoreWindow matched first and
# reported a plausible but wrong rect of (0,8)-(1920,1040), so the click landed on the
# desktop and Progman took the foreground. Collect every match and choose, rather than
# taking whichever EnumWindows happens to reach first.
$script:__cands = New-Object System.Collections.ArrayList
[VrWin32]::EnumWindows({
    param($h, $l)
    if ([VrWin32]::IsWindowVisible($h)) {
        $t = Get-VrTitle $h
        if ($t -like "*$TitleMatch*" -and $t -notlike "*Navisual*") {
            $script:__cands.Add([pscustomobject]@{ H = $h; T = $t; C = Get-VrClass $h }) | Out-Null
        }
    }
    return $true
}, [IntPtr]::Zero) | Out-Null

foreach ($c in $script:__cands) { Write-Output ("[focus] candidate '{0}' class={1}" -f $c.T, $c.C) }
$pick = $script:__cands | Where-Object { $_.C -ne "Windows.UI.Core.CoreWindow" } | Select-Object -First 1
$script:__hit = if ($pick) { $pick.H } else { [IntPtr]::Zero }

if ($script:__hit -eq [IntPtr]::Zero) { throw "TARGET NOT FOUND: no window matching '$TitleMatch'" }
$hwnd = $script:__hit
Write-Output ("[focus] target '{0}' class={1} iconic={2}" -f (Get-VrTitle $hwnd), (Get-VrClass $hwnd), [VrWin32]::IsIconic($hwnd))

# Restore, THEN read the rect. Reading it first gives the -32000 parking spot.
# The synthesized Alt gives this thread the "received input" status Windows' foreground
# lock tests for, which is the documented way through it. Alt is chosen because it is
# inert: pressed and released with no window command, it does nothing.
[VrWin32]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero)          # VK_MENU down
[VrWin32]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)          # VK_MENU up
Start-Sleep -Milliseconds 120
# SW_RESTORE only if actually minimized: on a MAXIMIZED window it un-maximizes,
# which shrank a maximized Word to a partly off-screen rect (measured 2026-09-20).
if ([VrWin32]::IsIconic($hwnd)) { [VrWin32]::ShowWindow($hwnd, 9) | Out-Null }   # SW_RESTORE
[VrWin32]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 900

$r = New-Object VrWin32+VRRECT
if (-not [VrWin32]::GetWindowRect($hwnd, [ref]$r)) { throw "FOCUS FAILED: GetWindowRect failed after restore." }
if ($r.L -le -30000) { throw "FOCUS FAILED: '$TitleMatch' is still minimized after SW_RESTORE." }

# The real click is what grants actual OS input focus; SetForegroundWindow alone does not
# (reference_navisual_taskbox_real_click.md). Title bar, horizontally centred.
$x = [int]($r.L + ($r.R - $r.L) * 0.5)
$y = [int]($r.T + 18)
[VrWin32]::SetCursorPos($x, $y) | Out-Null
Start-Sleep -Milliseconds 200
[VrWin32]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 80
[VrWin32]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 700

# Verify, rather than assume. A scenario run against the wrong app produces a plausible
# answer to a question nobody asked, which is the expensive failure. Name what actually
# holds the foreground so one failure is enough to diagnose.
$fg = [VrWin32]::GetForegroundWindow()
if ($fg -ne $hwnd) {
    # An elevated window holding the foreground cannot be displaced by a non-elevated
    # harness, and it poisons everything downstream: the real-click meant for the
    # Navisual panel never lands, so the task box keeps whatever it had and the run
    # reports a timeout that has nothing to do with the model. Name it here.
    $hint = ""
    try {
        $fgProc = Get-Process -Id ((Get-CimInstance Win32_Process -Filter ("ProcessId=" + (Get-VrPid $fg))).ProcessId) -ErrorAction Stop
        $null = $fgProc.MainModule.FileName
    } catch {
        $hint = " That window looks ELEVATED -- a non-elevated harness cannot take the foreground from it. Close it and re-run."
    }
    throw ("FOCUS FAILED: '{0}' did not take the foreground. Foreground is '{1}' (class={2}). Target rect ({3},{4})-({5},{6}), clicked ({7},{8}).{9}" -f
           $TitleMatch, (Get-VrTitle $fg), (Get-VrClass $fg), $r.L, $r.T, $r.R, $r.B, $x, $y, $hint)
}
Write-Output ("[focus] OK at ({0},{1}); rect ({2},{3})-({4},{5})" -f $x, $y, $r.L, $r.T, $r.R, $r.B)

# The target must not overlap Navisual's panel. If it does, the panel's own task box is
# underneath it, so the real-click that types the task lands on the target app instead
# and the harness silently drives the wrong window. Measured 2026-09-20 with Task
# Manager, which opens on whichever monitor it last used regardless of the fixture
# layout. Navisual also blanks its own panel out of every capture, so an overlapping
# target gets a hole punched through it even when the typing does work.
$script:__panel = [IntPtr]::Zero
[VrWin32]::EnumWindows({
    param($h, $l)
    if ([VrWin32]::IsWindowVisible($h) -and -not [VrWin32]::IsIconic($h)) {
        # EXACTLY "Navisual" -- the panel. "Navisual Screen Guide" is the transparent
        # click-through overlay and spans the whole virtual desktop by design, so
        # matching it would report an overlap against every possible target.
        if ((Get-VrTitle $h) -eq "Navisual") { $script:__panel = $h; return $false }
    }
    return $true
}, [IntPtr]::Zero) | Out-Null

if ($script:__panel -ne [IntPtr]::Zero) {
    $p = New-Object VrWin32+VRRECT
    if ([VrWin32]::GetWindowRect($script:__panel, [ref]$p)) {
        $overlapW = [Math]::Min($r.R, $p.R) - [Math]::Max($r.L, $p.L)
        $overlapH = [Math]::Min($r.B, $p.B) - [Math]::Max($r.T, $p.T)
        if ($overlapW -gt 0 -and $overlapH -gt 0) {
            throw ("LAYOUT: '{0}' at ({1},{2})-({3},{4}) overlaps the Navisual panel at ({5},{6})-({7},{8}) by {9}x{10}px. Move the target to the other monitor; the panel's task box is underneath it." -f
                   $TitleMatch, $r.L, $r.T, $r.R, $r.B, $p.L, $p.T, $p.R, $p.B, $overlapW, $overlapH)
        }
        Write-Output ("[focus] no overlap with panel at ({0},{1})-({2},{3})" -f $p.L, $p.T, $p.R, $p.B)
    }
}

$runArgs = @{
    ModelLabel    = $ModelLabel
    ScenarioLabel = $ScenarioLabel
    TaskText      = $TaskText
    TimeoutSec    = $TimeoutSec
}
if ($IsFollowup) { $runArgs["IsFollowup"] = $true }
& "$PSScriptRoot\run-single-task.ps1" @runArgs

if ($MinimizeAfter) {
    [VrWin32]::ShowWindow($hwnd, 6) | Out-Null               # SW_MINIMIZE
    Write-Output "[focus] '$TitleMatch' minimized; desktop clean for the next scenario."
}
