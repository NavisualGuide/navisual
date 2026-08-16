param(
    [Parameter(Mandatory)][string]$ModelLabel,
    [string[]]$Cells = @("G2","G3","Q20","M15","B25","S5","H2","I2"),
    [switch]$Append,
    [int]$FocusX = -1500,
    [int]$FocusY = 300
)

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class EvWin32 {
    [DllImport("user32.dll")] public static extern void mouse_event(uint dwFlags, uint dx, uint dy, uint dwData, UIntPtr dwExtraInfo);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int X, int Y);
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc lpEnumFunc, IntPtr lParam);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr hWnd, StringBuilder text, int count);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int X, int Y, int cx, int cy, uint uFlags);
    [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p);
    [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr hwnd, uint flags);
}
public struct POINT { public int X, Y; public POINT(int x, int y) { X = x; Y = y; } }
"@ -ErrorAction SilentlyContinue
Add-Type -AssemblyName System.Windows.Forms -ErrorAction SilentlyContinue
Add-Type -AssemblyName UIAutomationClient -ErrorAction SilentlyContinue
Add-Type -AssemblyName UIAutomationTypes -ErrorAction SilentlyContinue

function Click-At($x, $y) {
    [EvWin32]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 150
    [EvWin32]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 70
    [EvWin32]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 300
}

function Assert-FocusWindowOnTop {
    # A correctly-positioned window is not necessarily an ON-TOP window -- a fixed screen
    # coordinate can silently end up under whatever else the user brings forward later (e.g.
    # confirmed live: a VS Code window occupying the same secondary-monitor region ended up
    # above the test Excel window, so every "focus click" landed on VS Code instead -- Navisual
    # correctly reported VS Code as foreground and asked to bring Excel forward, which this
    # script's needs_input detector couldn't distinguish from an actual model failure). Verify
    # by hit-testing the real coordinate, not by trusting a window position checked earlier.
    param([int]$X, [int]$Y, [string]$ExpectedTitleSubstring)
    $pt = New-Object POINT($X, $Y)
    $hitHwnd = [EvWin32]::WindowFromPoint($pt)
    $rootHwnd = [EvWin32]::GetAncestor($hitHwnd, 2)  # GA_ROOT
    $sb = New-Object System.Text.StringBuilder 256
    [EvWin32]::GetWindowText($rootHwnd, $sb, 256) | Out-Null
    if ($sb.ToString().Contains($ExpectedTitleSubstring)) { return $true }

    # Wrong window on top at that point -- bring the real target forward via the
    # minimize->restore trick (a straight SetForegroundWindow from a script is frequently
    # denied by Windows' foreground-lock; documented in locator-bug-hunt's own SKILL.md).
    $excelHwnd = [IntPtr]::Zero
    $cb = { param($h, $l)
        if ([EvWin32]::IsWindowVisible($h)) {
            $s2 = New-Object System.Text.StringBuilder 256
            [EvWin32]::GetWindowText($h, $s2, 256) | Out-Null
            if ($s2.ToString().Contains($ExpectedTitleSubstring)) { $script:__excelHwnd = $h; return $false }
        }
        return $true
    }
    $script:__excelHwnd = [IntPtr]::Zero
    [EvWin32]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
    if ($script:__excelHwnd -eq [IntPtr]::Zero) { return $false }
    [EvWin32]::ShowWindow($script:__excelHwnd, 6) | Out-Null
    Start-Sleep -Milliseconds 200
    [EvWin32]::ShowWindow($script:__excelHwnd, 9) | Out-Null
    Start-Sleep -Milliseconds 400

    # Re-verify -- don't just assume the restore trick worked
    $hitHwnd2 = [EvWin32]::WindowFromPoint($pt)
    $rootHwnd2 = [EvWin32]::GetAncestor($hitHwnd2, 2)
    $sb2 = New-Object System.Text.StringBuilder 256
    [EvWin32]::GetWindowText($rootHwnd2, $sb2, 256) | Out-Null
    return $sb2.ToString().Contains($ExpectedTitleSubstring)
}

function Get-NavisualPanelHwnd {
    # Get-Process's MainWindowHandle is unreliable here: navisual-backend.exe owns BOTH
    # the panel ("Navisual") and the click-through overlay ("Tauri App", spanning the
    # whole virtual desktop), and Windows can hand MainWindowHandle to either one --
    # observed switching mid-session. Find the panel by exact title match instead.
    $navProcId = (Get-Process navisual-backend -ErrorAction Stop).Id
    $script:__foundHwnd = [IntPtr]::Zero
    $cb = {
        param($h, $l)
        if ([EvWin32]::IsWindowVisible($h)) {
            $sb = New-Object System.Text.StringBuilder 256
            [EvWin32]::GetWindowText($h, $sb, 256) | Out-Null
            if ($sb.ToString() -eq "Navisual") {
                [uint32]$ownerPid = 0
                [EvWin32]::GetWindowThreadProcessId($h, [ref]$ownerPid) | Out-Null
                if ($ownerPid -eq $navProcId) { $script:__foundHwnd = $h; return $false }
            }
        }
        return $true
    }
    [EvWin32]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
    return $script:__foundHwnd
}

function Get-NavisualRoot {
    $hwnd = Get-NavisualPanelHwnd
    if ($hwnd -eq [IntPtr]::Zero) { throw "Navisual panel window not found" }
    return [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
}

function Warm-NavisualTree($root) {
    $all = New-Object System.Windows.Automation.PropertyCondition ([System.Windows.Automation.AutomationElement]::IsEnabledProperty), $true
    $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $all) | Out-Null
    Start-Sleep -Milliseconds 400
}

function Find-ByName($root, $name) {
    $cond = New-Object System.Windows.Automation.PropertyCondition ([System.Windows.Automation.AutomationElement]::NameProperty), $name
    return $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
}

# Several of Navisual's own button names carry a non-ASCII glyph prefix (fullwidth plus,
# arrows, etc. -- e.g. "＋ New task"). Windows PowerShell 5.1 reading a BOM-less UTF-8
# .ps1 file does not reliably parse those bytes back into the right codepoint (confirmed
# live: the on-disk UTF-8 bytes were correct, but a script-file string literal containing
# the glyph never matched the real button name). Match on the plain-ASCII substring
# instead of the exact glyph-prefixed name -- sidesteps the encoding problem entirely.
function Find-ByContains($root, $substring) {
    $all = New-Object System.Windows.Automation.PropertyCondition ([System.Windows.Automation.AutomationElement]::IsEnabledProperty), $true
    $els = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $all)
    foreach ($el in $els) {
        if ($el.Current.Name -and $el.Current.Name.Contains($substring)) { return $el }
    }
    return $null
}

function Find-EditControl($root) {
    $cond = New-Object System.Windows.Automation.PropertyCondition ([System.Windows.Automation.AutomationElement]::ControlTypeProperty), ([System.Windows.Automation.ControlType]::Edit)
    return $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
}

function Invoke-ByName($root, $name) {
    $el = Find-ByContains $root $name
    if ($el) { $el.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); return $true }
    return $false
}

# Retries the whole (re-fetch root -> warm -> find) sequence, not just the find, since a
# stale root reference from before a DOM change can keep failing even as the real element
# exists -- observed live (New task button "not found" against a root fetched before the
# panel finished re-rendering from the previous step's response).
function Invoke-ByNameRetry($name, [int]$TimeoutSec = 6) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        $root = Get-NavisualRoot
        Warm-NavisualTree $root
        if (Invoke-ByName $root $name) { return $true }
        Start-Sleep -Milliseconds 400
    }
    return $false
}

function Find-EditControlRetry([int]$TimeoutSec = 6) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        $root = Get-NavisualRoot
        $edit = Find-EditControl $root
        if ($edit) { return $edit }
        Start-Sleep -Milliseconds 400
    }
    return $null
}

$LogPath = "$env:LOCALAPPDATA\com.navisual.app\locate_log.jsonl"
$results = @()
if ($Append) {
    $existingPath = "C:\Users\fujin\AppData\Local\Temp\claude\C--Users-fujin-claude-code-root\7601dbe4-7045-4f3e-aa70-51a09ad7d6fc\scratchpad\grounding_$ModelLabel.json"
    if (Test-Path $existingPath) {
        $loaded = Get-Content $existingPath -Encoding UTF8 | ConvertFrom-Json
        foreach ($item in $loaded) { $results += $item }
    }
}

foreach ($cell in $Cells) {
  $cellSucceeded = $false
  for ($attempt = 1; $attempt -le 3 -and -not $cellSucceeded; $attempt++) {
    if ($attempt -gt 1) { Write-Output "[$ModelLabel] $cell -> retry attempt $attempt (previous attempt's submission didn't produce a clean hit)" }
    # With the target explicitly PINNED to a specific window (Navisual's picker -> the exact
    # workbook window, a pin icon confirms it), Navisual submits against that window regardless
    # of what's actually on top or focused -- the auto-follow-chip/occlusion problem this
    # function exists for is moot for targeting purposes now. Still attempted, best-effort, only
    # so Ctrl+Home below has a chance of landing on Excel and resetting the selection; a failure
    # here is a minor data-quality nicety (stale selection in the screenshot), not a broken
    # target, so it's a warning, not a skip.
    if (-not (Assert-FocusWindowOnTop -X $FocusX -Y $FocusY -ExpectedTitleSubstring "Excel")) {
        Write-Output "[$ModelLabel] $cell -> WARN could not bring Excel on top for the A1 reset (pinned target is unaffected, continuing)"
    }

    # Reset the target app to a neutral A1 selection (best-effort -- see warning above)
    Click-At -1500 25
    [System.Windows.Forms.SendKeys]::SendWait("^{HOME}")
    Start-Sleep -Milliseconds 400

    $priorCount = 0
    if (Test-Path $LogPath) { $priorCount = (Get-Content $LogPath | Measure-Object -Line).Lines }

    if (-not (Invoke-ByNameRetry "New task")) {
        Write-Output "[$ModelLabel] $cell -> ERROR (New task button not found after retry)"
        continue
    }
    Start-Sleep -Milliseconds 1500

    $edit = Find-EditControlRetry
    if (-not $edit) { Write-Output "[$ModelLabel] $cell -> ERROR (no edit control found after retry)"; continue }
    $taskText = "Where is cell $cell?"

    # THE ACTUAL ROOT CAUSE (found by comparing against demo-video-production's
    # demo_take.ps1, a script proven reliable across 14 real recorded takes): UIA
    # ElementPattern.SetFocus() sets ACCESSIBILITY-level focus, which does not
    # reliably grant the panel genuine OS keyboard focus -- confirmed directly by a
    # clean isolated test (paste a known string, screenshot, read it back: with
    # SetFocus() alone the paste silently landed nowhere and the box was untouched;
    # GetForegroundWindow() confirmed Navisual was NOT actually the foreground
    # window despite SetFocus() having been called on its Edit element). Every
    # previous "fix" in this file (settle delays, Enter vs. button-click, per-char
    # SendKeys vs. SetValue) was chasing symptoms of this one cause and half-working
    # by accident whenever some other window happened to already hold focus loosely.
    # The actual fix, exactly as demo_take.ps1 does it: a REAL mouse click (SetCursorPos
    # + mouse_event, genuine synthetic hardware input) directly on the edit control's
    # own BoundingRectangle -- unlike SetForegroundWindow (blocked by Windows'
    # foreground-lock for background processes) or SetFocus() (accessibility-only), a
    # real click IS treated as genuine user input and reliably grants real focus.
    # Verified live: GetForegroundWindow() matched Navisual's hwnd immediately after.
    $rect = $edit.Current.BoundingRectangle
    Click-At ([int]($rect.X + $rect.Width / 2)) ([int]($rect.Y + $rect.Height / 2))
    Start-Sleep -Milliseconds 300
    [System.Windows.Forms.SendKeys]::SendWait("^a")
    Start-Sleep -Milliseconds 200
    [System.Windows.Forms.SendKeys]::SendWait("{DEL}")
    Start-Sleep -Milliseconds 300
    $taskText.ToCharArray() | ForEach-Object {
        [System.Windows.Forms.SendKeys]::SendWait([string]$_)
        Start-Sleep -Milliseconds 40
    }
    Start-Sleep -Milliseconds 500

    # Readback confirms the keystrokes actually landed (wrong focus / dropped chars) --
    # separate from, and confirmed NOT sufficient proof against, the submission-path bug below.
    $readback = $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
    if ($readback -ne $taskText) {
        Write-Output "[$ModelLabel] $cell -> WARN readback mismatch ('$readback' != '$taskText'), retyping"
        Click-At ([int]($rect.X + $rect.Width / 2)) ([int]($rect.Y + $rect.Height / 2))
        Start-Sleep -Milliseconds 200
        [System.Windows.Forms.SendKeys]::SendWait("^a")
        Start-Sleep -Milliseconds 200
        [System.Windows.Forms.SendKeys]::SendWait("{DEL}")
        Start-Sleep -Milliseconds 300
        $taskText.ToCharArray() | ForEach-Object {
            [System.Windows.Forms.SendKeys]::SendWait([string]$_)
            Start-Sleep -Milliseconds 40
        }
        Start-Sleep -Milliseconds 500
    }

    # Submit via Enter, not by clicking "Guide me" through UIA InvokePattern. Confirmed live:
    # clicking the button (even after a 3-6s settle) intermittently submitted a stale, truncated
    # copy of the text ("Where is cell" with the reference silently dropped) despite correct
    # UIA readback at the time of the click -- InvokePattern.Invoke() calls the handler through
    # the accessibility bridge, outside the browser's normal event queue, and can race ahead of
    # a still-pending reactive update from the last keystroke. Enter goes through the same
    # native keyboard event channel the typing itself used and was correct on every trial,
    # including with only a 500ms settle -- so this is not a slower-but-safer swap, it is a
    # strictly better one.
    [System.Windows.Forms.SendKeys]::SendWait("{ENTER}")

    $deadline = (Get-Date).AddSeconds(45)
    $newCount = -1
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 1000
        if (Test-Path $LogPath) {
            $c = (Get-Content $LogPath | Measure-Object -Line).Lines
            if ($c -gt $priorCount) { $newCount = $c; break }
        }
    }

    if ($newCount -eq -1) {
        # Check whether it landed in needs_input (malformed/ambiguous submission) rather than a
        # true timeout. This is the exact failure signature of the still-not-fully-root-caused
        # submission race (confirmed truncated server-side goal text on these) -- worth another
        # attempt rather than recording it as this cell's final answer, since a fresh attempt has
        # a real chance of landing clean (observed live: identical code succeeding and failing
        # across consecutive cells in the same run).
        $root = Get-NavisualRoot
        $needsInput = Find-ByContains $root "needs input"
        $status = if ($needsInput) { "NEEDS_INPUT" } else { "TIMEOUT" }
        Write-Output "[$ModelLabel] $cell -> $status (attempt $attempt)"
        # Reset to a clean session regardless so the next attempt/cell isn't cascaded
        Invoke-ByNameRetry "New task" | Out-Null
        Start-Sleep -Milliseconds 500
        if ($attempt -eq 3) {
            $results += [PSCustomObject]@{ cell = $cell; status = $status; kind = $null; model = $null }
        }
    } else {
        Start-Sleep -Milliseconds 300
        $line = Get-Content $LogPath -Encoding UTF8 | Select-Object -Last 1 | ConvertFrom-Json
        $results += [PSCustomObject]@{
            cell = $cell; status = "OK"; kind = $line.final_decision.kind
            model = $line.model; provider = $line.provider
            ai_bbox = ($line.ai_bbox | ConvertTo-Json -Compress)
            final_bbox = ($line.final_bbox | ConvertTo-Json -Compress)
            ttft = $line.ai_ttft_ms; elapsed = $line.ai_elapsed_ms
        }
        Write-Output "[$ModelLabel] $cell -> $($line.final_decision.kind) model=$($line.model) ttft=$($line.ai_ttft_ms)ms"
        $cellSucceeded = $true
    }

    Start-Sleep -Milliseconds 500
  }
}

$outPath = "C:\Users\fujin\AppData\Local\Temp\claude\C--Users-fujin-claude-code-root\7601dbe4-7045-4f3e-aa70-51a09ad7d6fc\scratchpad\grounding_$ModelLabel.json"
$results | ConvertTo-Json -Depth 5 | Set-Content -Path $outPath -Encoding UTF8
Write-Output "Saved results to $outPath"
