<#
.SYNOPSIS
Run the replay harness (src-tauri/src/eval.rs) against fixtures, on BYOK keys.

.DESCRIPTION
Sends each fixture through the app's own request path and scores the answer. Every call
spends real money on the BYOK key for that provider, so the harness stops before the call
that would cross -MaxUsd. -DryRun spends nothing: it writes the exact prompt and the exact
downscaled image each fixture would send, for inspection.

Targets are explicit provider:model pairs on purpose. The tier -> model mapping lives in
navisual-internal/docs/server-plan.md and nowhere else; a copy of it in this script would
drift the first time routing moved.

Stop `npm run tauri dev` first: it holds the cargo build lock and this would wait on it.

.EXAMPLE
.\tools\run-replay.ps1 -Targets "gemini:gemini-3.8-flash" -DryRun

.EXAMPLE
.\tools\run-replay.ps1 -Targets "openai:gpt-5.6-luna,gemini:gemini-3.8-flash,openai:gpt-5.6-terra" -MaxUsd 0.50

.EXAMPLE
.\tools\run-replay.ps1 -Rescore ..\navisual-internal\eval\results\s1-openai-gpt-5.6-luna-20261006-012853.jsonl
Re-judges stored responses with the current fixtures and scoring rules. Free: no model call.
#>
param(
    [string]$Targets = '',
    [string]$Battery = 's1',
    [string]$EvalDir = '',
    [int]$Repeat = 1,
    [string]$Only = '',
    [double]$MaxUsd = 1.0,
    [string]$Reasoning = '',
    [switch]$DryRun,
    [string]$Rescore = '',
    # A deliberate prompt change for an ablation: with_rect, expect (H1); eval.rs checks the
    # list. Empty = the app's own prompt, which is what every non-ablation run must use.
    [string]$Variant = '',
    # H1 use B on the S2 fixtures: OCR each after-screen for the text its expectation quotes.
    # Free: no model call.
    [switch]$UseB
)

# Not 'Stop': cargo writes its progress to stderr, and Windows PowerShell 5.1 turns each
# stderr line into an error record once the caller redirects stream 2 -- 'Stop' would end
# the run at the first 'Compiling' line. Failures are checked explicitly below instead.
$repo = Split-Path -Parent $PSScriptRoot
if (-not $EvalDir) {
    $EvalDir = Join-Path (Split-Path -Parent $repo) 'navisual-internal\eval'
}
if (-not (Test-Path (Join-Path $EvalDir $Battery))) {
    throw "No fixtures at $(Join-Path $EvalDir $Battery). Generate them first (node eval/gen-s1.mjs in navisual-internal)."
}

$manifest = Join-Path $repo 'src-tauri\Cargo.toml'
$env:NAVISUAL_EVAL_DIR = $EvalDir
$env:NAVISUAL_EVAL_BATTERY = $Battery
if ($UseB) {
    cargo test --release --manifest-path $manifest --lib eval::use_b_local_check -- --ignored --nocapture --test-threads=1
    exit $LASTEXITCODE
}
if ($Rescore) {
    $env:NAVISUAL_EVAL_RESCORE = (Resolve-Path $Rescore).Path
    cargo test --release --manifest-path $manifest --lib eval::replay_battery -- --ignored --nocapture --test-threads=1
    $code = $LASTEXITCODE
    $env:NAVISUAL_EVAL_RESCORE = ''
    if ($code -ne 0) { throw "re-score failed (exit $code)" }
    return
}
$env:NAVISUAL_EVAL_RESCORE = ''
if (-not $Targets) { throw "Give -Targets provider:model[,provider:model...] or -Rescore <results.jsonl>" }
foreach ($t in ($Targets -split ',')) {
    $pair = $t.Trim() -split ':', 2
    if ($pair.Count -ne 2) { throw "Target '$t' is not provider:model" }
    $env:NAVISUAL_EVAL_DIR = $EvalDir
    $env:NAVISUAL_EVAL_BATTERY = $Battery
    $env:NAVISUAL_EVAL_PROVIDER = $pair[0]
    $env:NAVISUAL_EVAL_MODEL = $pair[1]
    $env:NAVISUAL_EVAL_REPEAT = "$Repeat"
    $env:NAVISUAL_EVAL_ONLY = $Only
    $env:NAVISUAL_EVAL_MAX_USD = "$MaxUsd"
    $env:NAVISUAL_EVAL_REASONING = $Reasoning
    $env:NAVISUAL_EVAL_DRY_RUN = $(if ($DryRun) { '1' } else { '' })
    $env:NAVISUAL_EVAL_OUT = ''
    $env:NAVISUAL_EVAL_VARIANT = $Variant
    Write-Host "=== $($pair[0]) $($pair[1]) ==="
    # Release build: the downscale is several times slower unoptimised (CLAUDE.md rule 5),
    # and nothing about a debug build is closer to what ships.
    cargo test --release --manifest-path $manifest --lib eval::replay_battery -- --ignored --nocapture --test-threads=1
    if ($LASTEXITCODE -ne 0) { throw "harness failed for $t (exit $LASTEXITCODE)" }
}
