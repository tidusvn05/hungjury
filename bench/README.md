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

### Multi-seed + summary (the recommended default)

Single runs carry ±5–8 pts of noise at n=90 decided questions — run at
least 3 seeds before trusting a swing smaller than that:

```bash
for s in 42 7 1337; do SEED=$s REPORT=report_s$s.json bench/run_bench.sh pr_review; done
python3 bench/summarize.py            # mean±stdev per arm, grouped by label
```

### Hung-rate coverage

3 jurors + `--hung-threshold 0.5` almost never hangs (a 2–1 split decides).
Two ways to exercise escalation + memory-write on hung keys:

```bash
# Higher threshold: low-confidence majorities escalate too.
LABEL=adversarial-hung REPORT=report_hung.json BENCH_HOME=$PWD/bench/adversarial/home_hung \
  bench/run_bench.sh adversarial --hung-threshold 0.8

# Even juror count: a 1–1 (or 2–2) split hangs at any threshold.
LABEL=adversarial-2j REPORT=report_2j.json BENCH_HOME=$PWD/bench/adversarial/home_2j \
  bench/run_bench.sh adversarial --jurors claude:haiku,codex:gpt-5.6-terra@low
```

(Use a fresh `BENCH_HOME` per variant so trained memory can't leak
between runs.)

## Reports

`bench/<domain>/report*.json` — per-arm `accuracy`, `hung_rate`, `calls`
(+ `calls_by_backend`), `wall_ms_mean`/`p95`, `mismatches` (≤20 samples),
and a `config` echo. Arms: `jury`, `jury_memory`, `judge` (cold — answers
blind), `judge_informed` (sees juror ballots + answers, like production
escalation).

### Policy alignment

Labels encode a *policy*; when the judge applies a different rubric its
rulings hurt `jury_memory` (see `docs/BENCHMARK.md`). Inject the domain
rubric into juror and judge prompts to measure the aligned ceiling:

```bash
SEED=42 REPORT=report_policy.json \
  bench/run_bench.sh pr_review --policy-file bench/pr_review/policy.md
```

Changing the policy file changes the cache key, so no stale decisions.

## Microbenchmarks (appendix runner)

`bench/microbench.sh` reproduces the raw numbers behind the
[`../docs/BENCHMARK.md`](../docs/BENCHMARK.md) appendix. Each measurement
appends one line to `bench/micro/<group>.jsonl`:

```
{"bench","variant","backend","model","n_cases","pack_size","samples",
 "wall_ms","calls","exit","timestamp","git_rev"}
```

`calls` is counted from the run's `$HUNGJURY_HOME/calls.jsonl` audit log.
Every measurement runs in a fresh `bench/micro/home-*/` dir (gitignored
via `bench/*/home*/`) that keeps `stdout.log`, `stderr.log` and the
batch `--out` file as evidence. Conditions mirror the appendix: one
juror per run, `--min-quorum 1`, `--escalate off --no-cache --no-memory`,
`max_concurrency=6` (binary default — bench.toml uses 8).

```bash
cargo build                          # or BIN=./target/release/hungjury
bench/microbench.sh latency          # 1 decide per backend, in parallel —   ~3 calls
bench/microbench.sh juror-models     # codex terra/luna/sol @low, 20 cases — ~60 calls
bench/microbench.sh throughput       # decide + batch@10/20/50, 3 backends — ~243 calls
bench/microbench.sh pack             # seq decides vs batch vs --pack N —     ~59 calls
bench/microbench.sh all              # everything —                          ~365 calls
```

**Quota warning:** every group spawns real juror CLIs and burns real
quota — `all` is ~365 calls (bounded by a generated config with
`daily_cap=5000`, same as `bench.toml`). `throughput` is the expensive
one; run groups individually if the day's budget is tight.

Overrides via env (see the script header for the full list):
`MAX_CONCURRENCY=8` to match `bench.toml` conditions, `JUROR_MODELS`,
`THROUGHPUT_BACKENDS`/`THROUGHPUT_SIZES`, `PACK_N`/`PACK_SAMPLES`,
`*_CASES` to point at a different `cases.jsonl`.
