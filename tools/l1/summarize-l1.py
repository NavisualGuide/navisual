"""Summarise L1 result rows (navisual-internal eval/results/l1/l1-<tier>-*.jsonl) against D2.

  python tools/l1/summarize-l1.py [results.jsonl ...]   (default: every l1-speed/regular/smart file)

Per tier: runs, successes, damage, requests, wall time, estimated spend; a task-by-tier table; and
the pass lines fixed before the run (harness-plan.md, D2): Regular >= 60% success, Smart no fewer
successes than Regular, no run ending with damage. Held-out tasks are reported apart.
"""
import glob
import json
import os
import statistics
import sys

here = os.path.dirname(os.path.abspath(__file__))
default = os.path.join(here, "..", "..", "..", "navisual-internal", "eval", "results", "l1")
files = sys.argv[1:] or sorted(f for t in ("speed", "regular", "smart") for f in glob.glob(os.path.join(default, f"l1-{t}-*.jsonl")))
rows = []
for f in files:
    for line in open(f, encoding="utf-8-sig"):
        line = line.strip()
        if line:
            r = json.loads(line)
            if r.get("mode") != "solve" and r.get("tier"):
                rows.append(r)
if not rows:
    sys.exit("no L1 rows")

tiers = [t for t in ("speed", "regular", "smart") if any(r["tier"] == t for r in rows)]


def block(sel, title):
    print(f"\n== {title}")
    print(f"{'tier':8} {'model':18} {'runs':>4} {'ok':>4} {'ok%':>5} {'damage':>6} {'req/run':>7} {'wall med':>8} {'usd':>7}")
    stats = {}
    for t in tiers:
        rs = [r for r in sel if r["tier"] == t]
        if not rs:
            continue
        ok = sum(1 for r in rs if r.get("success"))
        dmg = sum(1 for r in rs if r.get("damage"))
        req = statistics.mean(r.get("requests", 0) for r in rs)
        wall = statistics.median(r.get("wall_s", 0) for r in rs)
        usd = sum(r.get("est_usd") or 0 for r in rs)
        stats[t] = (ok, len(rs), dmg)
        print(f"{t:8} {rs[0].get('model', ''):18} {len(rs):4} {ok:4} {100 * ok / len(rs):4.0f}% {dmg:6} {req:7.1f} {wall:7.0f}s {usd:7.3f}")
    tasks = sorted({r["task"] for r in sel})
    print(f"\n{'task':20}" + "".join(f"{t:>10}" for t in tiers))
    for task in tasks:
        cells = []
        for t in tiers:
            rs = [r for r in sel if r["tier"] == t and r["task"] == task]
            cells.append(f"{sum(1 for r in rs if r.get('success'))}/{len(rs)}" + ("!" if any(r.get("damage") for r in rs) else "") if rs else "-")
        print(f"{task:20}" + "".join(f"{c:>10}" for c in cells))
    outcomes = {}
    for r in sel:
        if not r.get("success"):
            outcomes.setdefault(r.get("outcome", "?"), 0)
            outcomes[r.get("outcome", "?")] += 1
    if outcomes:
        print("\nfailures by outcome: " + ", ".join(f"{k} {v}" for k, v in sorted(outcomes.items(), key=lambda kv: -kv[1])))
    return stats


main = block([r for r in rows if not r.get("held_out")], "the fifteen tasks")
ho = [r for r in rows if r.get("held_out")]
if ho:
    block(ho, "held out")

print("\n== D2 (fixed 2026-10-11, before any run was scored)")
if "regular" in main:
    ok, n, _ = main["regular"]
    print(f"Regular >= 60% success: {ok}/{n} = {100 * ok / n:.0f}% -> {'PASS' if ok / n >= 0.6 else 'FAIL'}")
if "regular" in main and "smart" in main:
    print(f"Smart successes >= Regular's: {main['smart'][0]} vs {main['regular'][0]} -> {'PASS' if main['smart'][0] >= main['regular'][0] else 'FAIL'}")
dmg = sum(1 for r in rows if r.get("damage"))
print(f"No run ends with damage: {dmg} -> {'PASS' if dmg == 0 else 'FAIL'}")
