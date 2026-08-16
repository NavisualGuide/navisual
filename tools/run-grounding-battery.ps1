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

# Several of Navisual's own button names carry a non-ASCII glyph prefix (fullwidth plus,
# arrows, etc. -- e.g. "＋ New task"). Windows PowerShell 5.1 reading a BOM-less UTF-8
# .ps1 file does not reliably parse those bytes back into the right codepoint. Match on
# the plain-ASCII substring instead of the exact glyph-prefixed name.
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
    if ($attempt -gt 1) { Write-Output "[$ModelLabel] $cell -> retry attempt $attempt" }

    $priorCount = 0
    if (Test-Path $LogPath) { $priorCount = (Get-Content $LogPath | Measure-Object -Line).Lines }

    if (-not (Invoke-ByNameRetry "New task")) {
        Write-Output "[$ModelLabel] $cell -> ERROR (New task button not found after retry)"
        continue
    }
    Start-Sleep -Seconds 2

    $edit = Find-EditControlRetry
    if (-not $edit) { Write-Output "[$ModelLabel] $cell -> ERROR (no edit control found after retry)"; continue }

    # THE REAL BUG, found 2026-08-16 after hours chasing focus/paste-vs-type/process-boundary
    # theories, none of which were it: "$cell?" in a double-quoted PowerShell string is NOT
    # "$cell followed by a literal ?" -- PowerShell tries to resolve a variable literally named
    # "cell?", finds nothing, and the WHOLE interpolation silently evaluates to empty. Confirmed
    # directly: "$x?" -> "", "${x}?" -> "G3?". Every automated submission truncated to "Where is
    # cell" while every manually hardcoded literal ("Where is cell G3?", no variable) worked --
    # a one-character PowerShell footgun, not a Navisual bug, not a UI Automation timing issue.
    $taskText = "Where is cell ${cell}?"

    # Real click on the task box's own BoundingRectangle (not SetFocus(), which is
    # accessibility-only and does not reliably grant genuine OS keyboard focus -- confirmed
    # live via GetForegroundWindow() mismatch) -- matches demo-video-production/demo_take.ps1's
    # proven pattern.
    $rect = $edit.Current.BoundingRectangle
    Click-At ([int]($rect.X + $rect.Width / 2)) ([int]($rect.Y + $rect.Height / 2))
    Start-Sleep -Milliseconds 300
    [System.Windows.Forms.SendKeys]::SendWait("^a")
    Start-Sleep -Milliseconds 200
    [System.Windows.Forms.Clipboard]::SetText($taskText)
    Start-Sleep -Milliseconds 150
    [System.Windows.Forms.SendKeys]::SendWait("^v")
    Start-Sleep -Milliseconds 600

    # Verify before Enter -- read back and confirm it matches before submitting anything.
    $readback = $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
    if ($readback -ne $taskText) {
        Write-Output "[$ModelLabel] $cell -> WARN readback mismatch ('$readback' != '$taskText'), re-pasting"
        Click-At ([int]($rect.X + $rect.Width / 2)) ([int]($rect.Y + $rect.Height / 2))
        Start-Sleep -Milliseconds 200
        [System.Windows.Forms.SendKeys]::SendWait("^a")
        Start-Sleep -Milliseconds 200
        [System.Windows.Forms.Clipboard]::SetText($taskText)
        Start-Sleep -Milliseconds 150
        [System.Windows.Forms.SendKeys]::SendWait("^v")
        Start-Sleep -Milliseconds 600
        $readback = $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
        if ($readback -ne $taskText) {
            Write-Output "[$ModelLabel] $cell -> ERROR (readback still wrong after re-paste: '$readback'), skipping submit"
            continue
        }
    }

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
        $root = Get-NavisualRoot
        $needsInput = Find-ByContains $root "needs input"
        $status = if ($needsInput) { "NEEDS_INPUT" } else { "TIMEOUT" }
        Write-Output "[$ModelLabel] $cell -> $status (attempt $attempt)"
        Invoke-ByNameRetry "New task" | Out-Null
        Start-Sleep -Milliseconds 500
        if ($attempt -eq 3) {
            $results += [PSCustomObject]@{ cell = $cell; status = $status; kind = $null; model = $null }
        }
    } else {
        Start-Sleep -Milliseconds 300
        $line = Get-Content $LogPath -Encoding UTF8 | Select-Object -Last 1 | ConvertFrom-Json
        # A locate happening at all is NOT proof it targeted the right cell -- the model can
        # guess a plausible-but-wrong fallback ("A1", "Name Box") and still produce a clean
        # hit_adapter/hit_selection. Record target_text and flag any mismatch explicitly.
        $targetMatches = $line.target_text -eq $cell
        $status = if ($targetMatches) { "OK" } else { "OK_BUT_WRONG_TARGET" }
        $results += [PSCustomObject]@{
            cell = $cell; status = $status; kind = $line.final_decision.kind
            target_text = $line.target_text
            model = $line.model; provider = $line.provider
            ai_bbox = ($line.ai_bbox | ConvertTo-Json -Compress)
            final_bbox = ($line.final_bbox | ConvertTo-Json -Compress)
            ttft = $line.ai_ttft_ms; elapsed = $line.ai_elapsed_ms
        }
        Write-Output "[$ModelLabel] $cell -> $status kind=$($line.final_decision.kind) target_text=$($line.target_text) model=$($line.model) ttft=$($line.ai_ttft_ms)ms"
        $cellSucceeded = $true
    }

    Start-Sleep -Milliseconds 500
  }
}

$outPath = "C:\Users\fujin\AppData\Local\Temp\claude\C--Users-fujin-claude-code-root\7601dbe4-7045-4f3e-aa70-51a09ad7d6fc\scratchpad\grounding_$ModelLabel.json"
$results | ConvertTo-Json -Depth 5 | Set-Content -Path $outPath -Encoding UTF8
Write-Output "Saved results to $outPath"
