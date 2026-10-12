<#
.SYNOPSIS
L1's scored pass: every tier, the fifteen tasks three times each (interleaved), then the five
held-out tasks once each. Results go to their own folder; summarize-l1.py reads them.

.EXAMPLE
.\tools\l1\run-l1-final.ps1
.EXAMPLE
.\tools\l1\run-l1-final.ps1 -Tiers smart -OutName final-rerun
#>
param(
    [string[]]$Tiers = @('speed', 'regular', 'smart'),
    [int]$Repeat = 3,
    [string]$OutName = ('final-' + (Get-Date -Format 'yyyyMMdd'))
)
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$out = Join-Path (Split-Path -Parent $repo) "navisual-internal\eval\results\l1\$OutName"
New-Item -ItemType Directory -Force -Path $out | Out-Null
# Spend caps per tier, generous against the estimate (list prices, no cache discount).
$caps = @{ speed = 3.0; regular = 12.0; smart = 20.0 }
$heldOut = 'ho-ex-rename,ho-xl-condformat,ho-word-landscape,ho-web-stores,ho-lo-footer'
foreach ($t in $Tiers) {
    Write-Host "##### $t -- fifteen tasks x $Repeat"
    & (Join-Path $PSScriptRoot 'run-l1.ps1') -Tier $t -Tasks all -Repeat $Repeat -OutDir $out -MaxUsd $caps[$t]
    Write-Host "##### $t -- held out"
    & (Join-Path $PSScriptRoot 'run-l1.ps1') -Tier $t -Tasks $heldOut -Repeat 1 -OutDir $out -MaxUsd $caps[$t]
}
Write-Host "##### done; results in $out"
& python (Join-Path $PSScriptRoot 'summarize-l1.py') @(Get-ChildItem $out -Filter 'l1-*.jsonl' | ForEach-Object { $_.FullName })
