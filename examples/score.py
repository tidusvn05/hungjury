#!/usr/bin/env python3
"""Score a batch results.jsonl against `expected` labels in cases.jsonl.

Usage: score.py cases.jsonl results.jsonl

expected values: string (choice), bool (noul), int (score), or "hung"
(key must land in `hung` to count correct). Per-case and per-question
accuracy printed; exit 0 always — this is a reporter, not a gate.
"""
import json, sys
from collections import Counter

def val(ans):
    if "choice" in ans and ans["choice"] is not None:
        return ans["choice"]
    if "noul" in ans and ans["noul"] is not None:
        return bool(ans["noul"] >= 0.5)
    if "score" in ans and ans["score"] is not None:
        return round(ans["score"])
    return None

def main(cases_path, results_path):
    expected = {}
    for line in open(cases_path):
        c = json.loads(line)
        expected[c.get("case")] = c.get("expected", {})
    ok = Counter(); tot = Counter(); hung_n = 0; err = 0
    rows = []
    for line in open(results_path):
        r = json.loads(line)
        case = r.get("case")
        if "error" in r:
            err += 1
            rows.append((case, "ERROR", r["error"][:60]))
            continue
        hung = set(r.get("hung") or [])
        if hung:
            hung_n += 1
        marks = []
        for q, want in (expected.get(case) or {}).items():
            tot[q] += 1
            if want == "hung":
                good = q in hung
                got = "hung" if good else "decided"
            elif q in hung:
                good, got = False, "hung"
            else:
                got = val(r["answers"].get(q, {}))
                good = got == want
            ok[q] += good
            marks.append(f"{q}:{'✓' if good else '✗'}({got}!={want})" if not good else f"{q}:✓")
        rows.append((case, r.get("decided_by", "?"), " ".join(marks)))
    for case, by, marks in rows:
        print(f"  {case:<18} {by:<5} {marks}")
    n_ok, n_tot = sum(ok.values()), sum(tot.values())
    print(f"\naccuracy {n_ok}/{n_tot} = {100*n_ok/max(n_tot,1):.0f}%"
          f"   hung-cases {hung_n}   errors {err}")
    for q in tot:
        print(f"  {q}: {ok[q]}/{tot[q]}")

if __name__ == "__main__":
    main(*sys.argv[1:3])
