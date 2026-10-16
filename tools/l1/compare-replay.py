"""Compare two replay-steps.ps1 outputs: the steps a follower change would act on differently.

    python tools/l1/compare-replay.py old.jsonl new.jsonl [run-root]
"""
import json
import os
import sys

old, new = sys.argv[1], sys.argv[2]
root = sys.argv[3] if len(sys.argv) > 3 else os.path.join(os.environ.get("TEMP", "."), "navisual-l1")


def load(p):
    d = {}
    for l in open(p, encoding="utf-8-sig"):
        if l.strip():
            r = json.loads(l)
            d[(r["dir"], r["line"])] = r["sig"]
    return d


o, n = load(old), load(new)
changed = 0
for k in sorted(o):
    if o[k] != n.get(k):
        changed += 1
        line = open(os.path.join(root, k[0], "feed.jsonl"), encoding="utf-8-sig").read().splitlines()[k[1] - 1]
        s = json.loads(line)
        print(k[0], "#", k[1], "|", s.get("target_text"), "|", s["instruction"][:110])
        for f in o[k]:
            if o[k][f] != (n.get(k) or {}).get(f):
                print("    ", f, ":", repr(o[k][f]), "->", repr((n.get(k) or {}).get(f)))
print("steps compared:", len(o), "changed:", changed)
