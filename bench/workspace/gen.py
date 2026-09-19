#!/usr/bin/env python3
"""Generate the `workspace` eval set (5 real repos x 12 cases = 60).

Use case: point hungjury at a real repository and ask repo-level questions —
exercises the workspace/facts path end-to-end (ToolPolicy::ReadOnly agents
reading files, judge-written facts with evidence hashes, stale invalidation).

Repos are sibling git checkouts of this repository (override the root with
BENCH_WS_ROOT). Adjust the list to whatever real repos you have locally:
  hungjury    rust CLI — tests+docs+examples, no CI            -> cli, health 2, ci false
  agentwiki   rust CLI — docs+CI+contributing                  -> cli, health 2, ci true
  bee-harness rust+node harness — rich docs+CI                 -> cli, health 2, ci true
  leasegate   rust workspace, vault/gateway contracts, no CI   -> service, health 1, ci false
  test-hermes static landing page (html)                       -> app, health 0, ci false

Questions:
  - purpose:     choice(cli|library|service|app)
  - code_health: score(0|1|2)   0 sparse/rough, 1 decent, 2 tests+docs+hygiene
  - has_ci:      noul           CI config present (e.g. .github/workflows)

Each case carries a different `hint` so retrieval/prompts vary while labels
stay repo-level.
"""
import json, os, random
from pathlib import Path

rng = random.Random(20260926)

# Repo root (this file is bench/workspace/gen.py) and the directory holding
# the sibling checkouts. Cases store *relative* workspace paths so the file
# stays machine-independent — run `hungjury` from the repo root.
REPO_ROOT = Path(__file__).resolve().parents[2]
WS_ROOT = Path(os.environ.get("BENCH_WS_ROOT", REPO_ROOT.parent))

Q = {
    "purpose": {
        "type": "choice",
        "id": "ws.purpose",
        "instructions": "What kind of software this repository mainly is",
        "criteria": {
            "cli": "A command-line tool / binary users run locally",
            "library": "A library/SDK consumed by other code",
            "service": "A long-running server, daemon or gateway",
            "app": "An end-user application or static site",
        },
    },
    "code_health": {
        "type": "score",
        "id": "ws.code_health",
        "instructions": "Overall repository hygiene",
        "criteria": [
            "Sparse or rough — few tests, docs, or structure",
            "Decent — reasonable structure, some tests or docs",
            "Well-maintained — tests, docs, examples, CI hygiene",
        ],
    },
    "has_ci": {
        "type": "noul",
        "id": "ws.has_ci",
        "instructions": "The repo has CI configuration (e.g. .github/workflows, .gitlab-ci.yml)",
    },
}

REPOS = [
    ("hungjury",    "hungjury",    "cli",     2, False),
    ("agentwiki",   "agentwiki",   "cli",     2, True),
    ("bee-harness", "bee-harness", "cli",     2, True),
    ("leasegate",   "leasegate",   "service", 1, False),
    ("test-hermes", "test-hermes", "app",     0, False),
]

HINTS = [
    "focus on what the project does and how it is built",
    "look at the test and documentation setup",
    "check for CI/CD configuration",
    "look at the main entrypoints and module layout",
    "check dependencies and packaging",
    "assess overall code organization and maturity",
]

cases = []
for name, dirname, purpose, health, ci in REPOS:
    ws = os.path.relpath(WS_ROOT / dirname, REPO_ROOT)
    if not (WS_ROOT / dirname).is_dir():
        raise SystemExit(f"missing sibling repo: {WS_ROOT / dirname}")
    for i in range(12):
        hint = HINTS[i % len(HINTS)]
        cases.append({
            "state": {"workspace": ws, "hint": f"Repository '{name}': {hint}"},
            "questions": Q,
            "expected": {"purpose": purpose, "code_health": health, "has_ci": ci},
        })

rng.shuffle(cases)
with open("bench/workspace/cases.jsonl", "w") as f:
    for c in cases:
        f.write(json.dumps(c, ensure_ascii=False) + "\n")
from collections import Counter
print(f"wrote {len(cases)}")
print(Counter(c["expected"]["purpose"] for c in cases))
print("health:", Counter(c["expected"]["code_health"] for c in cases),
      "| has_ci:", sum(c["expected"]["has_ci"] for c in cases))
