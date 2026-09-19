#!/usr/bin/env python3
"""Generate the `pr_review` eval set (~60 cases).

Use case: triage incoming pull requests.

State: PR title + description + diff-stat text.

Questions:
  - subsystem:    choice(frontend|backend|infra|docs)
  - risk:         score(0|1|2)
  - needs_senior: noul

Labeling policy:
  * subsystem   : dominant file area in the diff-stat
                  (web/ *.tsx *.css -> frontend; api/ server/ migrations/ *.rs *.py -> backend;
                   .github/ deploy/ Dockerfile terraform/ -> infra; docs/ *.md -> docs)
  * risk        : 2 touches auth/payments/migrations/prod-config or deletes data paths;
                  1 mixed subsystems or public-API change; 0 docs/typo/rename-only
  * needs_senior: true when risk==2, or schema/auth changes at risk>=1; false for docs/typo
"""
import json, random

rng = random.Random(20260921)

Q = {
    "subsystem": {
        "type": "choice",
        "id": "pr.subsystem",
        "instructions": "Which subsystem this PR mostly touches",
        "criteria": {
            "frontend": "Web UI, components, styles, client code",
            "backend": "Server APIs, business logic, database, migrations",
            "infra": "CI/CD, deployment, containers, cloud config",
            "docs": "Documentation, README, comments only",
        },
    },
    "risk": {
        "type": "score",
        "id": "pr.risk",
        "instructions": "Blast radius if this PR ships a bug",
        "criteria": [
            "Safe: docs, typos, renames, no behavior change",
            "Moderate: logic changes, API changes, mixed areas",
            "High: auth, payments, migrations, prod config, data deletion",
        ],
    },
    "needs_senior": {
        "type": "noul",
        "id": "pr.needs_senior",
        "instructions": "A senior engineer should review before merge (auth/payments/schema/high risk)",
    },
}

cases = []
def add(state, sub, risk, senior):
    cases.append({"state": state, "questions": Q,
                  "expected": {"subsystem": sub, "risk": risk, "needs_senior": senior}})

FE = ["web/src/components/Checkout.tsx", "web/src/pages/settings.tsx", "web/styles/theme.css", "web/src/hooks/useCart.ts"]
BE = ["api/src/routes/orders.rs", "server/services/billing.py", "api/src/models/user.rs", "migrations/0042_add_index.sql", "server/workers/email.py"]
IF = [".github/workflows/ci.yml", "deploy/terraform/main.tf", "Dockerfile", "deploy/k8s/values.yaml"]
DO = ["docs/architecture.md", "README.md", "docs/api.md", "CHANGELOG.md"]

def stat(files):
    lines = [f" {f} | {rng.randint(2,120)} +{'-'*0}" for f in files]
    return "\n".join(lines) + f"\n {len(files)} files changed"

# ---------- clear frontend (12) ----------
fe_t = [
    ("Fix checkout button alignment on mobile", "The pay button wraps on small screens; tighten flex layout.", 0, False),
    ("Add dark mode toggle to settings page", "New switch component + persisted preference.", 1, False),
    ("Refactor useCart hook into smaller selectors", "No behavior change, better memoization.", 1, False),
    ("Localize checkout flow strings (vi/en)", "Move hardcoded strings into i18n catalog.", 0, False),
]
for i in range(12):
    t, d, risk, sen = rng.choice(fe_t)
    add(f"PR: {t}\n\n{d}\n\n```\n{stat(rng.sample(FE, rng.randint(1,3)))}\n```",
        "frontend", risk, sen)

# ---------- clear backend (12) ----------
be_t = [
    ("Return 409 on duplicate order submit", "Idempotency key check before insert.", 1, False),
    ("Speed up /v1/orders list endpoint", "Add covering index + drop N+1 query.", 1, False),
    ("Retry failed webhook deliveries with backoff", "3 attempts, exponential backoff, dead-letter after.", 1, False),
    ("Validate VAT id format on signup", "Server-side format check + error copy.", 0, False),
]
for i in range(12):
    t, d, risk, sen = rng.choice(be_t)
    add(f"PR: {t}\n\n{d}\n\n```\n{stat(rng.sample(BE, rng.randint(1,3)))}\n```",
        "backend", risk, sen)

# ---------- clear infra (8) ----------
if_t = [
    ("Cache cargo artifacts in CI", "sccache + keyed cache; cuts build 40%.", 0, False),
    ("Bump node version in Dockerfile to 22", "Align with .nvmrc.", 0, False),
    ("Add staging deploy workflow", "Manual trigger, same steps as prod.", 1, False),
]
for i in range(8):
    t, d, risk, sen = rng.choice(if_t)
    add(f"PR: {t}\n\n{d}\n\n```\n{stat(rng.sample(IF, rng.randint(1,3)))}\n```",
        "infra", risk, sen)

# ---------- clear docs (8) ----------
do_t = [
    ("Document rate-limit headers", "Explain X-RateLimit-* semantics with examples.", 0, False),
    ("Fix typos in architecture guide", "", 0, False),
    ("Add migration runbook for v2", "Step-by-step rollback procedure.", 0, False),
]
for i in range(8):
    t, d, risk, sen = rng.choice(do_t)
    add(f"PR: {t}\n\n{d}\n\n```\n{stat(rng.sample(DO, rng.randint(1,3)))}\n```",
        "docs", risk, sen)

# ---------- high-risk backend (12) -> needs_senior ----------
hr_t = [
    ("Rotate JWT signing keys", "New kid header + dual-verify window for old tokens.", 2),
    ("Migrate payments table to cents integers", "Backfill + dual-write; drops float column.", 2),
    ("Delete inactive accounts older than 2 years", "Soft-delete sweep + purge job.", 2),
    ("Switch session store to redis-cluster", "Cookie format unchanged but storage moves.", 2),
    ("Tighten CORS to explicit origin allowlist", "Removes wildcard; may break old embeds.", 2),
]
for i in range(12):
    t, d, risk = rng.choice(hr_t)
    files = rng.sample(BE, rng.randint(1,2)) + (rng.sample(IF,1) if rng.random()<0.3 else [])
    add(f"PR: {t}\n\n{d}\n\n```\n{stat(files)}\n```", "backend", risk, True)

# ---------- mixed-subsystem borderline (8) -> label by dominant files ----------
for i in range(8):
    if rng.random() < 0.5:
        files = rng.sample(FE, 2) + rng.sample(BE, 1)
        sub = "frontend"
    else:
        files = rng.sample(BE, 2) + rng.sample(FE, 1)
        sub = "backend"
    add(f"PR: Checkout revamp part {i+1}\n\nTouches UI and order endpoint together.\n\n```\n{stat(files)}\n```",
        sub, 1, False)

rng.shuffle(cases)
with open("bench/pr_review/cases.jsonl", "w") as f:
    for c in cases:
        f.write(json.dumps(c, ensure_ascii=False) + "\n")
from collections import Counter
print(f"wrote {len(cases)}")
print(Counter(c["expected"]["subsystem"] for c in cases))
print("risk:", Counter(c["expected"]["risk"] for c in cases),
      "| senior:", sum(c["expected"]["needs_senior"] for c in cases))
