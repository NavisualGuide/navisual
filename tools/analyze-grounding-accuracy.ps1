# Evaluate AI grounding accuracy: how close is each model's `target_bbox` to the
# actually-correct location, for locates where that location was established by a
# channel that doesn't lean on the bbox to find the target in the first place --
# so the comparison isn't grading a model against its own guess.
#
# Usage:
#   .\tools\analyze-grounding-accuracy.ps1
#   .\tools\analyze-grounding-accuracy.ps1 -Path other.jsonl -Last 500
#   .\tools\analyze-grounding-accuracy.ps1 -IncludeOcr        # see below
#   .\tools\analyze-grounding-accuracy.ps1 -ShowOutliers
#
# Requires: locate_log.jsonl logging enabled (Settings -> Developer, or
# DEBUG_LOCATE_LOG_FILE_ENABLED=true in .env) and model/provider/token
# attribution on LocateTrace (locator/trace.rs, added 2026-07-09 -- entries
# logged before that fall into the "(unknown)|(unknown)" bucket).
#
# Ground truth: hit_adapter (deterministic geometry, e.g. Excel GridPattern),
# hit_selection (the AI's own element pick, but only counted once it's been
# live-reverified via ElementFromPoint against the current tree -- see
# a11y::verify_context_element), and hit_a11y (name/role search) all resolve
# the target WITHOUT trusting the AI's bbox to find it. hit_ocr is excluded by
# default: its corroboration gate can accept a match BASED ON bbox proximity
# (`near_ai_bbox && bbox_decisive`), which would let a model's own bbox count
# as evidence for itself. -IncludeOcr pulls in OCR hits ONLY when they were
# independently corroborated (uia_interactive / isolation_ok / near_anchor),
# never bbox-only ones -- see locator/trace.rs's Corroboration struct.
#
# The accuracy metric is deliberately lenient (bbox-center-inside-final-bbox,
# not IoU): this system already treats the AI bbox as a coarse locate aid /
# tiebreaker, never the precision source (see CLAUDE.md Design Decision #1),
# so grading it against pixel-perfect overlap would be measuring something the
# system was never designed to guarantee.

param(
    [string]$Path = "$env:LOCALAPPDATA\com.navisual.app\locate_log.jsonl",
    [int]$Last = 0,          # 0 = all entries
    [switch]$IncludeOcr,
    [switch]$ShowOutliers
)

if (-not (Test-Path $Path)) {
    Write-Error "Log not found: $Path"
    exit 1
}

$entries = @()
foreach ($line in [System.IO.File]::ReadAllLines($Path, [System.Text.Encoding]::UTF8)) {
    if ($line.Trim().Length -eq 0) { continue }
    try { $entries += , (ConvertFrom-Json $line) } catch { }
}
if ($Last -gt 0 -and $entries.Count -gt $Last) {
    $entries = $entries[($entries.Count - $Last)..($entries.Count - 1)]
}
if ($entries.Count -eq 0) { Write-Output "No parseable entries."; exit 0 }

function Percentile($values, $p) {
    $sorted = @($values | Sort-Object)
    if ($sorted.Count -eq 0) { return 0 }
    $idx = [Math]::Min([Math]::Floor($sorted.Count * $p), $sorted.Count - 1)
    return [int]$sorted[$idx]
}

function RectCenter($rect) {
    if (-not $rect) { return $null }
    return [PSCustomObject]@{ x = $rect.x + $rect.width / 2.0; y = $rect.y + $rect.height / 2.0 }
}

function CenterInRect($cx, $cy, $rect) {
    return ($cx -ge $rect.x) -and ($cx -le ($rect.x + $rect.width)) `
        -and ($cy -ge $rect.y) -and ($cy -le ($rect.y + $rect.height))
}

$groundTruthKinds = @("hit_adapter", "hit_selection", "hit_a11y")

$candidates = @($entries | Where-Object {
    $kind = $_.final_decision.kind
    if ($groundTruthKinds -contains $kind) { return $true }
    if ($IncludeOcr -and $kind -eq "hit_ocr") {
        $c = $_.ocr.corroboration
        return $c -and ($c.uia_interactive -or $c.isolation_ok -or $c.near_anchor)
    }
    return $false
})

Write-Output ("Entries: {0}   ({1})" -f $entries.Count, $Path)
Write-Output ("Ground-truth-quality locates: {0}" -f $candidates.Count)
Write-Output ""

if ($candidates.Count -eq 0) {
    Write-Output "No ground-truth-quality hits in this log yet -- nothing to evaluate."
    exit 0
}

# -- Per-model grounding accuracy --------------------------------------------
Write-Output "-- Per model (provider|model) --"
Write-Output ("  {0,-34} {1,5} {2,7} {3,7} {4,9} {5,9} {6,7} {7,7} {8,9} {9,6}" -f `
    "model", "n", "bbox%", "hit%", "med-px", "p90-px", "in-tok", "out-tok", "med-ai-ms", "n-ai")

$candidates | Group-Object {
    $p = if ($_.provider) { $_.provider } else { "(unknown)" }
    $m = if ($_.model) { $_.model } else { "(unknown)" }
    "$p|$m"
} | Sort-Object Count -Descending | ForEach-Object {
    $g = $_.Group
    $n = $g.Count
    $bboxed = @($g | Where-Object { $_.ai_bbox -and $_.final_bbox })
    $bboxRate = if ($n -gt 0) { $bboxed.Count / $n } else { 0 }

    $dists = @()
    $hits = 0
    foreach ($e in $bboxed) {
        $ac = RectCenter $e.ai_bbox
        $fc = RectCenter $e.final_bbox
        $dx = $ac.x - $fc.x
        $dy = $ac.y - $fc.y
        $dists += [Math]::Sqrt($dx * $dx + $dy * $dy)
        if (CenterInRect $ac.x $ac.y $e.final_bbox) { $hits++ }
    }
    $hitRate = if ($bboxed.Count -gt 0) { $hits / $bboxed.Count } else { 0 }

    $inTok = @($g | Where-Object { $null -ne $_.input_tokens } | ForEach-Object { $_.input_tokens })
    $outTok = @($g | Where-Object { $null -ne $_.output_tokens } | ForEach-Object { $_.output_tokens })
    # ai_elapsed_ms is only set on guide()/send_correction() (real AI round-trips); a
    # next_step() row reuses a prior response and carries no new AI-latency sample, so it's
    # excluded here rather than silently coming through as 0.
    $aiMs = @($g | Where-Object { $null -ne $_.ai_elapsed_ms } | ForEach-Object { $_.ai_elapsed_ms })

    Write-Output ("  {0,-34} {1,5} {2,6:P0} {3,6:P0} {4,7}px {5,7}px {6,7} {7,7} {8,7}ms {9,6}" -f `
        $_.Name, $n, $bboxRate, $hitRate, `
        (Percentile $dists 0.5), (Percentile $dists 0.9), `
        (Percentile $inTok 0.5), (Percentile $outTok 0.5), `
        (Percentile $aiMs 0.5), $aiMs.Count)
}
Write-Output ""
Write-Output "bbox% = target_bbox present & usable (model omitted it, or it was degenerate/"
Write-Output "        whole-frame and rejected by ai_bbox_to_screen_rect, both count as absent)."
Write-Output "hit%  = of those, AI bbox center fell inside the verified final bbox (lenient --"
Write-Output "        see header comment on why this isn't IoU)."
Write-Output "med/p90-px = center-to-center distance in screen pixels; not normalized by"
Write-Output "        resolution, so only compare runs from similar screen setups."
Write-Output "in/out-tok = median tokens for the AI call that produced this step (None for"
Write-Output "        next_step -- it makes no AI call, so nothing new to attribute)."
Write-Output "med-ai-ms = AI round-trip latency (ai_elapsed_ms -- the same number"
Write-Output "        model_timings.csv records, attached directly to this locate). NOT locate"
Write-Output "        latency (A11y/OCR/etc.) -- that's the separate elapsed_ms field, and"
Write-Output "        usually much smaller. n-ai is the sample size behind med-ai-ms -- entries"
Write-Output "        logged before 2026-07-09 (or via next_step) have no ai_elapsed_ms, so n-ai"
Write-Output "        can be smaller than the model's total n above."
Write-Output ""

# -- Worst individual misses (optional) --------------------------------------
if ($ShowOutliers) {
    Write-Output "-- Worst bbox misses (largest center distance, ground-truth-quality only) --"
    $candidates | Where-Object { $_.ai_bbox -and $_.final_bbox } | ForEach-Object {
        $ac = RectCenter $_.ai_bbox
        $fc = RectCenter $_.final_bbox
        $dx = $ac.x - $fc.x
        $dy = $ac.y - $fc.y
        [PSCustomObject]@{
            dist  = [int][Math]::Sqrt($dx * $dx + $dy * $dy)
            model = "{0}|{1}" -f $(if ($_.provider) { $_.provider } else { "(unknown)" }), $(if ($_.model) { $_.model } else { "(unknown)" })
            target = $_.target_text
            kind  = $_.final_decision.kind
        }
    } | Sort-Object dist -Descending | Select-Object -First 20 | ForEach-Object {
        Write-Output ("  {0,6}px  {1,-30} target='{2}' ({3})" -f $_.dist, $_.model, $_.target, $_.kind)
    }
}
