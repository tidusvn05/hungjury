#!/usr/bin/env python3
"""Generate the `log_triage` eval set (~60 cases).

Use case: route production error logs/alerts to the owning component.

State: log excerpt / stack trace / alert text.

Questions:
  - component:   choice(api|db|auth|frontend|worker)
  - severity:    score(0|1|2)
  - user_facing: noul

Labeling policy:
  * component   : owning service named in the trace
                  (api-gateway/nginx/route handlers -> api; postgres/redis/pool -> db;
                   oauth/jwt/session/ldap -> auth; browser/react/webpack -> frontend;
                   celery/sidekiq/queue/job -> worker)
  * severity    : 2 crash/OOM/data-loss/5xx-storm; 1 errors with retries/timeouts; 0 warnings/degraded
  * user_facing : true when end users hit the failure (5xx to clients, login/checkout broken);
                  false for internal jobs/dashboards/metrics
"""
import json, random

rng = random.Random(20260922)

Q = {
    "component": {
        "type": "choice",
        "id": "log.component",
        "instructions": "Which component owns this failure",
        "criteria": {
            "api": "HTTP API layer, gateways, route handlers",
            "db": "Databases, caches, connection pools, queries",
            "auth": "Login, tokens, sessions, OAuth, permissions",
            "frontend": "Browser app, React, bundling, client runtime",
            "worker": "Background jobs, queues, schedulers, crons",
        },
    },
    "severity": {
        "type": "score",
        "id": "log.severity",
        "instructions": "How severe the incident is",
        "criteria": [
            "Warning or degraded — system keeps working",
            "Errors with retries/timeouts — partial impact",
            "Crash, data loss, or outage — immediate action",
        ],
    },
    "user_facing": {
        "type": "noul",
        "id": "log.user_facing",
        "instructions": "End users directly experience this failure (not internal/batch)",
    },
}

cases = []
def add(state, comp, sev, uf):
    cases.append({"state": state, "questions": Q,
                  "expected": {"component": comp, "severity": sev, "user_facing": uf}})

HOSTS = ["prod-eu-1", "prod-us-2", "edge-04", "api-12", "w-3"]

# ---------- api (14) ----------
api_t = [
    ("ALERT 5xx rate 34% on /v1/checkout (last 5m)\n{h} nginx -> upstream 502\nupstream prematurely closed connection", 2, True),
    ("ERROR {h} api-gateway: route POST /v1/orders -> 500\nNullPointerException at OrderController.java:182", 2, True),
    ("WARN {h} api: request /v1/search took 8.4s (p99 breach)", 1, True),
    ("ERROR {h} api: rate limiter rejected 120 req/s burst from client 9f21", 1, True),
]
for i in range(14):
    t, sev, uf = rng.choice(api_t)
    add(t.format(h=rng.choice(HOSTS)), "api", sev, uf)

# ---------- db (12) ----------
db_t = [
    ("FATAL {h} postgres: remaining connection slots are reserved for superuser\npool: 200/200 in use", 2, True),
    ("ERROR {h} redis: LOADING dataset into memory — reads failing for 40s", 1, True),
    ("WARN {h} pg: slow query 12s: SELECT ... FROM events WHERE tenant_id=$1 (seq scan)", 1, False),
    ("ERROR {h} mysql: Deadlock found when trying to get lock; restarting transaction\nin nightly_reconcile", 1, False),
]
for i in range(12):
    t, sev, uf = rng.choice(db_t)
    add(t.format(h=rng.choice(HOSTS)), "db", sev, uf)

# ---------- auth (10) ----------
auth_t = [
    ("ERROR {h} auth: jwt verify failed: signature expired (kid=2024-09)\nlogin attempts returning 401 at 98%", 2, True),
    ("WARN {h} authd: LDAP bind timeout to corp directory, failover to cache", 1, True),
    ("ERROR {h} oauth: state parameter mismatch on callback — possible CSRF probe", 1, True),
    ("WARN {h} auth: session gc removed 2.1M expired rows (normal)", 0, False),
]
for i in range(10):
    t, sev, uf = rng.choice(auth_t)
    add(t.format(h=rng.choice(HOSTS)), "auth", sev, uf)

# ---------- frontend (10) ----------
fe_t = [
    ("SENTRY {h} web: TypeError: Cannot read properties of undefined (reading 'total')\n  at CheckoutSummary (checkout.tsx:114)\n  affected users: 4,120 last hour", 2, True),
    ("SENTRY {h} web: ChunkLoadError: Loading chunk 847 failed after deploy", 1, True),
    ("SENTRY {h} web: Warning: Each child in a list should have a unique key prop", 0, False),
    ("SENTRY {h} web: ResizeObserver loop limit exceeded (harmless, recurring)", 0, False),
]
for i in range(10):
    t, sev, uf = rng.choice(fe_t)
    add(t.format(h=rng.choice(HOSTS)), "frontend", sev, uf)

# ---------- worker (8) ----------
wk_t = [
    ("ERROR {h} worker-7: celery task billing.reconcile crashed: KeyError 'invoice_id'\n  retry 3/3 -> dead letter", 1, False),
    ("CRIT {h} worker-2: OOMKilled while processing report_export (rss 8.2GB)", 2, False),
    ("WARN {h} scheduler: queue depth report_gen = 48k (lag 35m)", 1, False),
    ("ERROR {h} sidekiq: EmailWorker job failed: SMTP 451 temporary failure, will retry", 0, False),
]
for i in range(8):
    t, sev, uf = rng.choice(wk_t)
    add(t.format(h=rng.choice(HOSTS)), "worker", sev, uf)

# ---------- borderline: ambiguous component (6) ----------
border_t = [
    # api route failing *because* db pool exhausted -> db owns root cause
    ("ERROR {h} api: GET /v1/dashboard timing out (30s)\nupstream: pg pool exhausted: all 100 connections busy", "db", 2, True),
    # auth service down because its db is down -> db
    ("ERROR {h} auth: cannot reach postgres auth-db:5432, refusing logins", "db", 2, True),
    # frontend error caused by api returning 500 -> api
    ("SENTRY {h} web: GraphQL query failed: 500 Internal Server Error from /graphql", "api", 1, True),
]
for i in range(6):
    t, comp, sev, uf = rng.choice(border_t)
    add(t.format(h=rng.choice(HOSTS)), comp, sev, uf)

rng.shuffle(cases)
with open("bench/log_triage/cases.jsonl", "w") as f:
    for c in cases:
        f.write(json.dumps(c, ensure_ascii=False) + "\n")
from collections import Counter
print(f"wrote {len(cases)}")
print(Counter(c["expected"]["component"] for c in cases))
print("sev:", Counter(c["expected"]["severity"] for c in cases),
      "| user_facing:", sum(c["expected"]["user_facing"] for c in cases))
