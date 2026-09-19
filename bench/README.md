# bench/

Multi-domain benchmark for `hungjury` — hypothetical use cases, ~60 labeled
cases per domain. Methodology and results: [`../docs/BENCHMARK.md`](../docs/BENCHMARK.md).

## Layout

Each `<domain>/` holds:

- `gen.py` — deterministic case generator (fixed seed; documents the labeling policy)
- `cases.jsonl` — generated eval cases (`{"state", "questions", "expected"}`)
- `report.json` — `hungjury eval` output for this domain
- `home/` — isolated `HUNGJURY_HOME` used for the run (memory, cache, call log; gitignored artifacts are fine to delete)

## Domains

| domain | use case | questions |
|---|---|---|
| `support` | support-ticket triage (English, n=120 — Phase 2 dataset, reused) | department / frustration / is_urgent |
| `pr_review` | triage pull requests | subsystem / risk / needs_senior |
| `log_triage` | route production logs to owning component | component / severity / user_facing |
| `email_intent` | triage inbound customer/community messages | intent / sentiment / respond_today |
| `multilingual` | support triage in Vietnamese + mixed vi/en (same qids as `support` — direct comparison) | department / frustration / is_urgent |
| `adversarial` | tickets engineered to split the jury (same qids as `support`) | department / frustration / is_urgent |
| `workspace` | repo-level questions over 5 real local repos | purpose / code_health / has_ci |

## Running

```bash
cargo build
python3 bench/<domain>/gen.py        # regenerate cases.jsonl (deterministic)
bench/run_bench.sh <domain>          # ~600 spawned CLI calls, tens of minutes
```

`run_bench.sh` sets `HUNGJURY_HOME=bench/<domain>/home` so each domain has
isolated memory/cache/quota, and passes `--config bench/bench.toml`
(daily_cap 5000, max_concurrency 8). `support/` ships a precomputed
`report.json` from Phase 2 — rerunning it costs another ~600 calls.

To run a cheap pilot first: `bench/run_bench.sh <domain> --train-frac 0.05`
on a trimmed `cases.jsonl` (e.g. `head -6`).

### Overrides

`SEED`, `REPORT`, `LABEL` are environment variables; extra args go to
`hungjury eval` (global flags like `--hung-threshold`/`--jurors` too):

```bash
SEED=7  REPORT=report_s7.json  bench/run_bench.sh pr_review
LABEL=adversarial-hung REPORT=report_hung.json \
  bench/run_bench.sh adversarial --hung-threshold 0.8
```

### Multi-seed + summary

```bash
for s in 42 7 1337; do SEED=$s REPORT=report_s$s.json bench/run_bench.sh pr_review; done
python3 bench/summarize.py            # mean±stdev per arm, grouped by label
```

### Hung-rate coverage

3 jurors + `--hung-threshold 0.5` almost never hangs (a 2–1 split decides).
`--hung-threshold 0.8` forces escalation on low-confidence majorities —
use the `adversarial-hung` variant above to exercise the judge path.

## Reports

`bench/<domain>/report*.json` — per-arm `accuracy`, `hung_rate`, `calls`
(+ `calls_by_backend`), `wall_ms_mean`/`p95`, `mismatches` (≤20 samples),
and a `config` echo. Arms: `jury`, `jury_memory`, `judge` (cold — answers
blind), `judge_informed` (sees juror ballots + answers, like production
escalation).
