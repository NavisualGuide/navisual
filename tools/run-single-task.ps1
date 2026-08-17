param(
    [Parameter(Mandatory)][string]$ModelLabel,
    [Parameter(Mandatory)][string]$ScenarioLabel,
    [Parameter(Mandatory)][string]$TaskText,
    [switch]$IsFollowup,
    [int]$TimeoutSec = 45
)

# Reuses the primitives proven in run-grounding-battery.ps1 on 2026-08-16 (real-click focus,
# clipboard paste, readback verification before Enter) generalized to an arbitrary task string
# instead of the fixed "Where is cell X?" template, for the Vision/Reasoning batteries.

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class SingleTaskWin32 {
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
    [SingleTaskWin32]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 150
    [SingleTaskWin32]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 70
    [SingleTaskWin32]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 300
}

function Get-NavisualPanelHwnd {
    $navProcId = (Get-Process navisual-backend -ErrorAction Stop).Id
    $script:__foundHwnd2 = [IntPtr]::Zero
    $cb = {
        param($h, $l)
        if ([SingleTaskWin32]::IsWindowVisible($h)) {
            $sb = New-Object System.Text.StringBuilder 256
            [SingleTaskWin32]::GetWindowText($h, $sb, 256) | Out-Null
            if ($sb.ToString() -eq "Navisual") {
                [uint32]$ownerPid = 0
                [SingleTaskWin32]::GetWindowThreadProcessId($h, [ref]$ownerPid) | Out-Null
                if ($ownerPid -eq $navProcId) { $script:__foundHwnd2 = $h; return $false }
            }
        }
        return $true
    }
    [SingleTaskWin32]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
    return $script:__foundHwnd2
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

function Invoke-ByNameRetry($name, [int]$TimeoutSecInner = 6) {
    $deadline = (Get-Date).AddSeconds($TimeoutSecInner)
    while ((Get-Date) -lt $deadline) {
        $root = Get-NavisualRoot
        Warm-NavisualTree $root
        if (Invoke-ByName $root $name) { return $true }
        Start-Sleep -Milliseconds 400
    }
    return $false
}

function Find-EditControlRetry([int]$TimeoutSecInner = 6) {
    $deadline = (Get-Date).AddSeconds($TimeoutSecInner)
    while ((Get-Date) -lt $deadline) {
        $root = Get-NavisualRoot
        $edit = Find-EditControl $root
        if ($edit) { return $edit }
        Start-Sleep -Milliseconds 400
    }
    return $null
}

$LogPath = "$env:LOCALAPPDATA\com.navisual.app\locate_log.jsonl"
$PromptLogPath = "$env:LOCALAPPDATA\com.navisual.app\prompt_log.jsonl"

for ($attempt = 1; $attempt -le 3; $attempt++) {
    if ($attempt -gt 1) { Write-Output "[$ModelLabel/$ScenarioLabel] retry attempt $attempt" }

    $priorCount = 0
    if (Test-Path $LogPath) { $priorCount = (Get-Content $LogPath | Measure-Object -Line).Lines }
    $priorPromptCount = 0
    if (Test-Path $PromptLogPath) { $priorPromptCount = (Get-Content $PromptLogPath | Measure-Object -Line).Lines }

    if (-not $IsFollowup) {
        if (-not (Invoke-ByNameRetry "New task")) {
            Write-Output "[$ModelLabel/$ScenarioLabel] ERROR (New task button not found)"
            continue
        }
        Start-Sleep -Seconds 2
    }

    $edit = Find-EditControlRetry
    if (-not $edit) { Write-Output "[$ModelLabel/$ScenarioLabel] ERROR (no edit control found)"; continue }

    $rect = $edit.Current.BoundingRectangle
    Click-At ([int]($rect.X + $rect.Width / 2)) ([int]($rect.Y + $rect.Height / 2))
    Start-Sleep -Milliseconds 300
    [System.Windows.Forms.SendKeys]::SendWait("^a")
    Start-Sleep -Milliseconds 200
    [System.Windows.Forms.Clipboard]::SetText($TaskText)
    Start-Sleep -Milliseconds 150
    [System.Windows.Forms.SendKeys]::SendWait("^v")
    Start-Sleep -Milliseconds 600

    $readback = $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
    if ($readback -ne $TaskText) {
        Write-Output "[$ModelLabel/$ScenarioLabel] WARN readback mismatch ('$readback' != '$TaskText'), re-pasting"
        Click-At ([int]($rect.X + $rect.Width / 2)) ([int]($rect.Y + $rect.Height / 2))
        Start-Sleep -Milliseconds 200
        [System.Windows.Forms.SendKeys]::SendWait("^a")
        Start-Sleep -Milliseconds 200
        [System.Windows.Forms.Clipboard]::SetText($TaskText)
        Start-Sleep -Milliseconds 150
        [System.Windows.Forms.SendKeys]::SendWait("^v")
        Start-Sleep -Milliseconds 600
        $readback = $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
        if ($readback -ne $TaskText) {
            Write-Output "[$ModelLabel/$ScenarioLabel] ERROR (readback still wrong after re-paste: '$readback')"
            continue
        }
    }

    [System.Windows.Forms.SendKeys]::SendWait("{ENTER}")

    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    $newCount = -1
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 1000
        if (Test-Path $PromptLogPath) {
            $c = (Get-Content $PromptLogPath | Measure-Object -Line).Lines
            if ($c -gt $priorPromptCount) { $newCount = $c; break }
        }
    }

    if ($newCount -eq -1) {
        Write-Output "[$ModelLabel/$ScenarioLabel] TIMEOUT (attempt $attempt)"
        if ($attempt -eq 3) {
            $out = [PSCustomObject]@{ scenario = $ScenarioLabel; model = $ModelLabel; status = "TIMEOUT" }
            $outPath = "C:\Users\fujin\AppData\Local\Temp\claude\C--Users-fujin-claude-code-root\7601dbe4-7045-4f3e-aa70-51a09ad7d6fc\scratchpad\vision_$($ModelLabel)_$($ScenarioLabel).json"
            $out | ConvertTo-Json -Depth 5 | Set-Content -Path $outPath -Encoding UTF8
            Write-Output "Saved (timeout) to $outPath"
        }
        continue
    }

    Start-Sleep -Milliseconds 300
    $promptLine = Get-Content $PromptLogPath -Encoding UTF8 | Select-Object -Last 1 | ConvertFrom-Json
    $step = $promptLine.response.steps[0]

    $locateLine = $null
    if (Test-Path $LogPath) {
        $curCount = (Get-Content $LogPath | Measure-Object -Line).Lines
        if ($curCount -gt $priorCount) {
            $locateLine = Get-Content $LogPath -Encoding UTF8 | Select-Object -Last 1 | ConvertFrom-Json
        }
    }

    $out = [PSCustomObject]@{
        scenario         = $ScenarioLabel
        model            = $ModelLabel
        status           = "OK"
        needs_input      = $promptLine.response.needs_input
        goal             = $promptLine.response.goal
        state_summary    = $promptLine.response.state_summary
        instruction      = $step.instruction
        target_text      = $step.target_text
        target_element_id = $step.target_element_id
        target_bbox      = ($step.target_bbox -join ",")
        clipboard        = $step.clipboard
        locate_kind      = if ($locateLine) { $locateLine.final_decision.kind } else { $null }
        locate_target    = if ($locateLine) { $locateLine.target_text } else { $null }
        ttft             = if ($locateLine) { $locateLine.ai_ttft_ms } else { $null }
    }

    $outPath = "C:\Users\fujin\AppData\Local\Temp\claude\C--Users-fujin-claude-code-root\7601dbe4-7045-4f3e-aa70-51a09ad7d6fc\scratchpad\vision_$($ModelLabel)_$($ScenarioLabel).json"
    $out | ConvertTo-Json -Depth 5 | Set-Content -Path $outPath -Encoding UTF8
    Write-Output "[$ModelLabel/$ScenarioLabel] OK needs_input=$($out.needs_input) target_text=$($out.target_text) locate_kind=$($out.locate_kind)"
    Write-Output "  instruction: $($out.instruction)"
    Write-Output "Saved to $outPath"
    break
}
