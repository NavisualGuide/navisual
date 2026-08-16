# Resolve a real, visible, top-level window by UIA class name — built after this class of bug
# cost real time twice in one session:
#
#   1. `(Get-Process WINWORD).MainWindowHandle` returned an MSO_BORDEREFFECT_WINDOW_CLASS shadow
#      window (empty title) once a recovery document existed. It "enumerated to zero elements"
#      because it genuinely had none — and that was read as a code bug for the better part of an
#      hour before the mistake was found.
#   2. A second run picked up a stale/wrong handle because a prior probe's window had since
#      closed, and nothing printed what was actually being pointed at.
#
# This does two things nothing before this script did: (a) filters to windows that could
# plausibly be a real document/app window (matching class, visible, non-empty title) instead of
# trusting the first thing the OS hands back, and (b) ALWAYS prints what it found, so a wrong
# pick is visible in one line instead of an hour of confused re-testing.
#
# Usage:
#   powershell -File tools\get-target-hwnd.ps1                    # Word (OpusApp), the common case
#   powershell -File tools\get-target-hwnd.ps1 -ClassName XLMAIN  # Excel
#   powershell -File tools\get-target-hwnd.ps1 -ClassName Chrome_WidgetWin_1
#
# To use the result in the SAME shell (so cargo test picks it up), dot-source it:
#   . .\tools\get-target-hwnd.ps1
#   # $env:NAVISUAL_TEST_HWND is now set if exactly one match was found.
#
# Run normally (not dot-sourced) it only prints — env vars set in a child process do not reach
# the parent shell, which is a PowerShell fact, not a bug in this script.
#
# ASCII only — PowerShell 5.1 reads a UTF-8 .ps1 as ANSI and a stray em-dash breaks parsing.

param(
    [string]$ClassName = "OpusApp",
    [switch]$All
)

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class GTH {
    public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr hWnd, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr hWnd, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
}
"@

$matches = New-Object System.Collections.Generic.List[object]
$cb = [GTH+EnumProc]{
    param($h, $l)
    if (-not [GTH]::IsWindowVisible($h)) { return $true }
    $cls = New-Object System.Text.StringBuilder 128
    [void][GTH]::GetClassName($h, $cls, 128)
    if ($cls.ToString() -ne $ClassName) { return $true }
    $title = New-Object System.Text.StringBuilder 256
    [void][GTH]::GetWindowText($h, $title, 256)
    if ($title.Length -eq 0) { return $true }   # the shadow/border-effect class of bug
    # NOT $pid - that collides with PowerShell's own read-only automatic $PID variable (this
    # script's process id), which is exactly the kind of silent-wrong-value bug this script
    # exists to prevent. Cost a re-run to catch, which is the whole reason it's called out here.
    $winPid = 0
    [void][GTH]::GetWindowThreadProcessId($h, [ref]$winPid)
    $matches.Add([pscustomobject]@{ Hwnd = [int64]$h; Title = $title.ToString(); Pid = $winPid })
    return $true
}
[void][GTH]::EnumWindows($cb, [IntPtr]::Zero)

if ($matches.Count -eq 0) {
    Write-Host "No visible, titled window with class '$ClassName' found." -ForegroundColor Red
    Write-Host "If the app is running, check the class name is right (a shadow/border window has no title and is correctly excluded here)."
    return
}

Write-Host "Found $($matches.Count) window(s) with class '$ClassName':"
foreach ($m in $matches) {
    Write-Host ("   hwnd={0,-10} pid={1,-8} '{2}'" -f $m.Hwnd, $m.Pid, $m.Title)
}

if ($matches.Count -gt 1 -and -not $All) {
    Write-Host ""
    Write-Host "More than one match - not guessing which one. Pass -All to list them, or narrow by title yourself." -ForegroundColor Yellow
    return
}

if ($matches.Count -eq 1) {
    $env:NAVISUAL_TEST_HWND = "$($matches[0].Hwnd)"
    Write-Host ""
    Write-Host "NAVISUAL_TEST_HWND set to $($matches[0].Hwnd) (only takes effect if this script was dot-sourced)." -ForegroundColor Green
}
