# Summarize locate_log.jsonl -- hit/miss/timing per framework and strategy.
#
# Usage:
#   .\tools\analyze-locate-log.ps1                 # default log location
#   .\tools\analyze-locate-log.ps1 -Path other.jsonl -Last 200
#   .\tools\analyze-locate-log.ps1 -ShowMisses     # list every non-hit locate
#
# Reads the rolling JSONL trace written by locator/trace.rs (schema:
# LocateTrace -- final_decision.kind, a11y.framework/cached/element_count,
# ocr.strategy_used/corroboration, elapsed_ms).

param(
    [string]$Path = "$env:APPDATA\com.navisual.app\locate_log.jsonl",
    [int]$Last = 0,          # 0 = all entries
    [switch]$ShowMisses
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

Write-Output ("Entries: {0}   ({1})" -f $entries.Count, $Path)
Write-Output ""

# -- Final decision breakdown -----------------------------------------------
Write-Output "-- Final decision --"
$entries | Group-Object { $_.final_decision.kind } | Sort-Object Count -Descending | ForEach-Object {
    Write-Output ("  {0,-26} {1,5}  ({2:P0})" -f $_.Name, $_.Count, ($_.Count / $entries.Count))
}
Write-Output ""

# -- Per-framework breakdown ------------------------------------------------
Write-Output "-- Per framework (a11y path) --"
Write-Output ("  {0,-8} {1,5} {2,7} {3,7} {4,7} {5,8} {6,8} {7,9} {8,7}" -f `
    "fw", "n", "a11y%", "ocr%", "miss%", "med-a11y", "p90-a11y", "med-total", "cached")
$entries | Group-Object { if ($_.a11y.framework) { $_.a11y.framework } else { "(none)" } } |
    Sort-Object Count -Descending | ForEach-Object {
    $g = $_.Group
    $n = $g.Count
    $a11yHit = @($g | Where-Object { $_.final_decision.kind -eq "hit_a11y" }).Count
    $ocrHit  = @($g | Where-Object { $_.final_decision.kind -eq "hit_ocr" }).Count
    $miss    = $n - $a11yHit - $ocrHit
    $cached  = @($g | Where-Object { $_.a11y.cached }).Count
    Write-Output ("  {0,-8} {1,5} {2,6:P0} {3,6:P0} {4,6:P0} {5,7}ms {6,7}ms {7,8}ms {8,7}" -f `
        $_.Name, $n, ($a11yHit / $n), ($ocrHit / $n), ($miss / $n), `
        (Percentile ($g | ForEach-Object { $_.a11y.elapsed_ms }) 0.5), `
        (Percentile ($g | ForEach-Object { $_.a11y.elapsed_ms }) 0.9), `
        (Percentile ($g | ForEach-Object { $_.elapsed_ms }) 0.5), $cached)
}
Write-Output ""

# -- Lazy-tree signal: cached find returned 0 elements ----------------------
$lazy = @($entries | Where-Object { $_.a11y.element_count -eq 0 })
Write-Output ("Lazy-tree signals (cached find saw 0 elements): {0}" -f $lazy.Count)
Write-Output ""

# -- OCR strategy breakdown (when OCR produced the winner) ------------------
Write-Output "-- OCR winning strategy --"
$ocrEntries = @($entries | Where-Object { $_.ocr.strategy_used })
if ($ocrEntries.Count -eq 0) {
    Write-Output "  (none)"
} else {
    $ocrEntries | Group-Object { $_.ocr.strategy_used } | Sort-Object Count -Descending | ForEach-Object {
        Write-Output ("  {0,-14} {1,5}" -f $_.Name, $_.Count)
    }
}
$rejected = @($entries | Where-Object {
    $_.final_decision.kind -in @("rejected_uncorroborated", "rejected_by_hit_test")
})
Write-Output ("  corroboration/hit-test rejections: {0}" -f $rejected.Count)
Write-Output ""

# -- Misses (optional detail) -----------------------------------------------
if ($ShowMisses) {
    Write-Output "-- Non-hit locates --"
    $entries | Where-Object { $_.final_decision.kind -notin @("hit_a11y", "hit_ocr") } | ForEach-Object {
        $ts = [DateTimeOffset]::FromUnixTimeMilliseconds($_.timestamp_ms).LocalDateTime.ToString("MM-dd HH:mm:ss")
        Write-Output ("  {0}  [{1}] fw={2} elems={3} {4}ms  target='{5}'" -f `
            $ts, $_.final_decision.kind, $_.a11y.framework, $_.a11y.element_count, $_.elapsed_ms, $_.target_text)
    }
}
