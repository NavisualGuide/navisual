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
}
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
    # Reset the target app to a neutral A1 selection
    Click-At -1500 25
    [System.Windows.Forms.SendKeys]::SendWait("^{HOME}")
    Start-Sleep -Milliseconds 400

    $priorCount = 0
    if (Test-Path $LogPath) { $priorCount = (Get-Content $LogPath | Measure-Object -Line).Lines }

    # Genuine click into the target app (updates Navisual's auto-follow chip)
    Click-At $FocusX $FocusY
    Start-Sleep -Milliseconds 300

    if (-not (Invoke-ByNameRetry "New task")) {
        Write-Output "[$ModelLabel] $cell -> ERROR (New task button not found after retry)"
        continue
    }
    Start-Sleep -Milliseconds 600

    $edit = Find-EditControlRetry
    if (-not $edit) { Write-Output "[$ModelLabel] $cell -> ERROR (no edit control found after retry)"; continue }
    $taskText = "Where is cell $cell?"
    $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($taskText)
    Start-Sleep -Milliseconds 300
    # UIA SetValue writes the DOM value directly but does not reliably fire the native
    # input event Svelte's binding listens to -- without this nudge the framework's
    # reactive $state can lag behind what's visibly in the box, and Guide me submits
    # the STALE (pre-SetValue) value instead of what's on screen. A real space+backspace
    # keystroke fires genuine input events without changing the text.
    $edit.SetFocus()
    Start-Sleep -Milliseconds 150
    [System.Windows.Forms.SendKeys]::SendWait(" ")
    Start-Sleep -Milliseconds 100
    [System.Windows.Forms.SendKeys]::SendWait("{BACKSPACE}")
    Start-Sleep -Milliseconds 300

    # Verify the text actually landed before submitting
    $readback = $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
    if ($readback -ne $taskText) {
        Write-Output "[$ModelLabel] $cell -> WARN readback mismatch ('$readback' != '$taskText'), retrying SetValue"
        $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($taskText)
        Start-Sleep -Milliseconds 300
    }

    if (-not (Invoke-ByNameRetry "Guide me")) {
        Write-Output "[$ModelLabel] $cell -> ERROR (Guide me button not found after retry)"
        continue
    }

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
        # Check whether it landed in needs_input (malformed/ambiguous submission) rather than a true timeout
        $root = Get-NavisualRoot
        $needsInput = Find-ByContains $root "needs input"
        $status = if ($needsInput) { "NEEDS_INPUT" } else { "TIMEOUT" }
        $results += [PSCustomObject]@{ cell = $cell; status = $status; kind = $null; model = $null }
        Write-Output "[$ModelLabel] $cell -> $status"
        # Reset to a clean session regardless so the next cell isn't cascaded
        Invoke-ByNameRetry "New task" | Out-Null
        Start-Sleep -Milliseconds 500
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
    }

    Start-Sleep -Milliseconds 500
}

$outPath = "C:\Users\fujin\AppData\Local\Temp\claude\C--Users-fujin-claude-code-root\7601dbe4-7045-4f3e-aa70-51a09ad7d6fc\scratchpad\grounding_$ModelLabel.json"
$results | ConvertTo-Json -Depth 5 | Set-Content -Path $outPath -Encoding UTF8
Write-Output "Saved results to $outPath"
