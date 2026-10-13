<#
L1 step 3 (harness-plan.md): the harness on against off, on one tier's model, interleaved.

Each repetition runs the fifteen tasks once per arm, and the order flips every repetition
(on/off, off/on, on/off ...) so that neither arm always runs first -- a slower afternoon on the
provider's side, or anything else that drifts with the clock, falls on both. Every run of
run-l1.ps1 starts its own app and data folder; the rows land in one results folder, each
carrying its harness value.

    .\tools\l1\run-l1-harness.ps1 -Tier speed -Repeat 3
    .\tools\l1\run-l1-harness.ps1 -Tier regular -Repeat 3 -OutName harness-flash-20261013
    .\tools\l1\run-l1-harness.ps1 -Tasks 'ex-zip,np-replace' -OutName harness-luna-20261013-rerun

Pure ASCII.
#>
param(
    [ValidateSet('speed', 'regular', 'smart')] [string]$Tier = 'speed',
    [int]$Repeat = 3,
    [string]$OutName = ('harness-' + (Get-Date -Format 'yyyyMMdd')),
    # Per run-l1.ps1 invocation, i.e. per arm per repetition.
    [double]$MaxUsdPerRun = 2.0,
    # Comma-separated task ids, or all. A subset re-runs tasks on both arms after a follower fix.
    [string]$Tasks = 'all'
)
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$out = Join-Path (Split-Path -Parent $repo) "navisual-internal\eval\results\l1\$OutName"
New-Item -ItemType Directory -Force -Path $out | Out-Null
for ($r = 1; $r -le $Repeat; $r++) {
    $arms = @('on', 'off')
    if ($r % 2 -eq 0) { $arms = @('off', 'on') }
    foreach ($arm in $arms) {
        Write-Host "##### repetition $r of $Repeat -- harness $arm"
        & (Join-Path $PSScriptRoot 'run-l1.ps1') -Tier $Tier -Tasks $Tasks -Repeat 1 -Harness $arm -OutDir $out -MaxUsd $MaxUsdPerRun
    }
}
Write-Host "##### done: $out"
