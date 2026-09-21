# Benchmark

Multi-domain evaluation of `hungjury`: does a jury of cheap CLI agents +
local memory approach a single strong judge, across different hypothetical
use cases?

- Method: `hungjury eval <cases>` per domain — deterministic train/test
  split (seed 42, 50/50), arms `jury`, `jury_memory`, `judge` (cold),
  `judge_informed` (sees ballots + memory), `go` verdict from the
  Phase 2 criteria.
- Jury: `claude:haiku`, `codex:gpt-5.6-terra@low`, `devin:swe-2-medium`
  (1 sample each). Judge: `claude:opus@high`. Escalation `sync` in training.
- Memory per domain is isolated (`HUNGJURY_HOME=bench/<domain>/home`), so
  rulings/facts never leak between domains.
- Cases are synthetic and deterministically generated (`bench/<d>/gen.py`,
  fixed seed); labels follow an explicit written policy per domain below.

## Metrics

Per arm, from `report.json`:

| field | meaning |
|---|---|
| `accuracy` | correct / decided (hung questions excluded from denominator) |
| `hung_rate` | hung / all questions |
| `calls` | spawned CLI calls (juror attempts incl. retries + judge calls) |
| `wall_ms_mean` / `wall_ms_p95` | per-case decision wall-clock (parallelism inflates absolute values) |

`go` = memory closed ≥50% of the jury→judge accuracy gap, **or**
hung-rate drop ≥30% with no accuracy regression.

## Domains & labeling policy

### `support` — English support-ticket triage (n=120, reused from Phase 2)

Ticket → `department` choice(billing|technical|sales), `frustration` score
(0 calm / 1 frustrated / 2 angry), `is_urgent` noul.
Policy: money wrongly moved → billing; broken behavior blocking money flow →
technical; pre-purchase/plan questions → sales; urgency only for explicit
deadline/blocking/ASAP.

### `pr_review` — pull-request triage

PR title + description + diff-stat → `subsystem` choice(frontend|backend|
infra|docs), `risk` score (0 safe / 1 moderate / 2 auth-payments-migrations-
prod-config-data-deletion), `needs_senior` noul (true ⇔ risk 2, or
schema/auth at risk ≥1). Subsystem = dominant file area in the diff-stat.

### `log_triage` — production error routing

Log/stack/alert excerpt → `component` choice(api|db|auth|frontend|worker),
`severity` score (0 warning / 1 retried-errors / 2 crash-outage-data-loss),
`user_facing` noul (end users hit it directly). Component = the service
named in the trace, including root-cause indirection (API 500s caused by an
exhausted DB pool → `db`).

### `email_intent` — inbound message triage

Email/forum post → `intent` choice(bug|feature|question|churn), `sentiment`
score (0 positive / 1 neutral / 2 negative), `respond_today` noul (churn,
angry bug, or account-at-risk → true).

### `multilingual` — same task as `support`, vi + mixed vi/en

Identical question ids and labeling policy as `support`; states generated
from Vietnamese and half-English templates. Tests whether rulings/memory
and jury agreement transfer across language.

### `adversarial` — engineered disagreement

Same question ids as `support`. Cases carry deliberately contradictory
signals: money-loss+bug in one sentence, "is this a bug or a plan cap?",
courtesy-ASAP vs buried deadlines, polite wording with named consequences.
Policy: first concrete request = department; "is it intended?" = sales;
explicit deadlines only = urgent; named hard consequences = frustration 2.
Goal: maximize jury hung-rate so the judge/memory arms have something to
fix — the previous run had 0% hung everywhere.

### `workspace` — real-repository questions

5 local git repos × 12 cases (`state.workspace` + varying `hint`):
`hungjury` (cli, health 2, no CI), `agentwiki` (cli, 2, CI),
`bee-harness` (cli, 2, CI), `leasegate` (service, 1, no CI),
`test-hermes` (static landing app, 0, no CI).
Questions: `purpose` choice(cli|library|service|app), `code_health` score
(0 sparse / 1 decent / 2 tests+docs+CI-hygiene), `has_ci` noul. Exercises
`ToolPolicy::ReadOnly` file reading and judge-written `ws:` facts.

## Results

Seed 42, 50/50 split (30 train / 30 test for the 60-case domains; 60/60 for
`support`). Accuracy on the test arm; `calls`/`wall` measured per case on
that arm.

| domain | jury | jury + memory | judge | go |
|---|---|---|---|---|
| `support` (n=120) | 91% | **95%** | 96% | **yes** (78% of gap) |
| `pr_review` | **80%** | 73% | 62% | no |
| `log_triage` | **87%** | **87%** | 80% | no |
| `email_intent` | 93% | 94% | **97%** | no (33% of gap) |
| `multilingual` | 88% | **93%** | **93%** | **yes** (100% of gap) |
| `adversarial` | **80%** | 68% | 69% | no |
| `workspace` | **87%** | **87%** | 38% | no |

| domain | jury calls | judge calls | jury wall/case | judge wall/case |
|---|---|---|---|---|
| `pr_review` | 90 | 30 | 26s | 20s |
| `log_triage` | 90 | 30 | 23s | 20s |
| `email_intent` | 90 | 30 | 21s | 15s |
| `multilingual` | 90 | 30 | 23s | 19s |
| `adversarial` | 90 | 30 | 29s | 18s |
| `workspace` | 93 | 30 | 65s | 30s |

### Findings

1. **The jury ensemble meets or beats a lone strong judge on 5 of 7
   domains.** The eval's `judge` arm answers every question alone (no
   ballots, no memory); three cheap ballots aggregated is a strong
   baseline. `go` only fires where a real jury→judge gap exists
   (`support`, `email_intent`, `multilingual`).

2. **Memory helps exactly where the judge's rulings agree with the task
   policy.** `support` +78% of gap, `multilingual` +100% of gap — judge
   rulings from the train arm generalized to the test arm, including
   across language (Vietnamese/mixed tickets, same question ids).

3. **Memory hurts where judge and label policy diverge** — the sharpest
   new finding. On `pr_review` (risk-rubric thresholds) and `adversarial`
   (first-request-wins policy), judge verdicts followed its own sensible
   but *different* rubric; rulings then pulled jurors toward it
   (73% vs 80%, 68% vs 80%). Memory faithfully teaches the judge's
   interpretation — when the judge is wrong relative to ground truth,
   memory is faithfully wrong.

4. **Hung rate ~0% everywhere** (1 question total, workspace mem arm).
   With 3 ballots and confidence ≥ 0.5, a 2–1 split always decides; a
   hang needs a 1-1-1 split or ballot failures. The adversarial set
   produced disagreement but not hangs, so `hung_rate_drop` remains
   unexercised — measuring it needs ≥5 jurors, higher thresholds, or
   genuinely 3-way-split cases.

5. **Workspace decisions work end-to-end** (jury 87% reading real repos,
   ~65s/case ≈ 3× text domains; facts written with evidence hashes).
   The judge arm's 38% is largely label ambiguity (`service` vs `cli`
   for `leasegate`/`bee-harness`, `code_health` scale) plus light repo
   exploration (~30s/case) — a manual judge probe on `bee-harness`
   answered all three questions correctly.

6. **Cost matched the estimate**: ~90 jury + ~30 judge spawned calls per
   test arm (+ ~100–120 for training) ≈ 600/domain, ~3,600 total.

## Reproduce

```bash
cargo build
python3 bench/<domain>/gen.py          # deterministic cases.jsonl
bench/run_bench.sh <domain>            # ~600 calls; writes bench/<d>/report.json

# overrides / variants
SEED=7 REPORT=report_s7.json bench/run_bench.sh <domain>
BENCH_HOME=$PWD/bench/<d>/home_hung LABEL=<d>-hung REPORT=report_hung.json \
  bench/run_bench.sh <domain> --hung-threshold 0.8

python3 bench/summarize.py             # mean±stdev per arm across report*.json
```

See `bench/README.md` for the full list and `bench/bench.toml` for the
limits used (daily_cap 5000, max_concurrency 8).

## Cost

~600 spawned CLI calls per domain (30 train decides × 3–4 calls + 30 test ×
3 arms). Roughly 3,600 calls for the six new domains; `support` reuses the
Phase 2 report. Call logs live in `bench/<d>/home/calls.jsonl`.

---

# Rerun — contested-guard + judge_informed arm

Second round on the two domains where memory *hurt* (`pr_review`,
`adversarial`), plus an `adversarial` variant at `--hung-threshold 0.8` to
exercise escalation. New arms/fields: `judge_informed` (judge sees juror
ballots + aggregated answers + memory — production-faithful), `mismatches`,
`calls_by_backend`, `config` echo. Fresh `BENCH_HOME` per run.

Guard semantics (`judge::commit_judge` / `judge::conflict_keys`): a ruling
or precedent is written `contested` — kept for audit, excluded from
retrieval — when the judge answered a question the jury had already
*decided* (not hung under the threshold) with a different verdict.
Rulings on genuinely hung keys stay `active`: resolving them is what
escalation is for. `learn --audit` re-judges all keys and demotes
conflicting `source=judge` rulings the same way.

## Results (seed 42, 30 train / 30 test, fresh memory)

| arm | pr_review r1 | pr_review r2 | adversarial r1 | adversarial r2 | adversarial-hung |
|---|---|---|---|---|---|
| jury | 80% | 72% | 80% | 77% | 80% |
| jury+memory | 73% | **76%** | 68% | 69% | 67% |
| judge (cold) | 62% | 63% | 69% | 68% | 66% |
| judge_informed | — | 61% | — | 70% | 67% |
| train escalations | ? | 4/30 | ? | 3/30 | **13/30** |

Contested entries written: `pr_review` 0, `adversarial` 1 ruling
(`support.frustration` — judge overrode a decided jury), `adversarial-hung` 0.

## Reading it

- **Run-to-run variance is real**: identical `jury` arm moved 80→72% on
  `pr_review` between rounds (same cases, same seed — model-side
  nondeterminism). Swings of ±5–8 pts at n=90 are noise; the memory
  *direction* flip on `pr_review` (−7 pts → +3 pts) is suggestive, not
  proof.
- **`judge_informed` ≈ `judge`** on both domains (61 vs 63, 70 vs 68):
  seeing ballots doesn't move the judge here — its rubric genuinely
  diverges from the label policy, it isn't missing information.
- **The guard fired but couldn't fix the main poisoning path.** On
  `adversarial`, judge rulings were written for *hung* keys — the guard
  only contests overrides of decided juries, so wrong-policy lessons on
  hung questions still go `active` and still drag `jury_memory` down
  (69 vs 77). At threshold 0.8, 13/30 train cases escalated → 28 rulings
  → `jury_memory` −13 pts. **More escalation ⇒ more poisoning when the
  judge's policy is wrong.** The durable fix is feedback/`learn --audit`
  demotion over time, or a judge prompt aligned to the task policy.
- **Hung coverage**: threshold 0.8 drove 43% of train cases to the judge
  (vs ~10% at 0.5), producing 28 rulings + 14 precedents — escalation and
  memory-write paths well exercised. Test-arm `hung_rate` stayed 0%
  because the seed-42 test half happened to draw unanimous-vote cases;
  hung-rate movement needs a test split that actually splits jurors.

### Aligned-ceiling run: `adversarial` + `--policy-file`

Same domain, fresh `home_policy*`, `policy.md` injected into **both**
juror and judge prompts — two seeds (42, 7):

| arm | no policy (mean±sd) | + policy (mean±sd) |
|---|---|---|
| jury | 78.3±2.4% | **86.7±1.6%** |
| jury+memory | 68.3±0.8% | 85.0±0.8% |
| judge (cold) | 68.3±0.8% | **85.0%** (85.6/84.4) |
| judge_informed | — | **86.1%** (85.6/86.7) |

- **Judge +17 pts** across seeds — confirms the gap was *policy
  mismatch*, not model capacity. The durable fix for memory poisoning
  is aligning the judge prompt with the task's labeling policy.
- Memory penalty shrank to **−1.7 pts mean** (seed 42: −3.4, seed 7:
  0.0) — inside the noise band. Policy-aligned judges write rulings
  that don't hurt.
- **Provisional trust fired in production**: the seed-42 db shows one
  ruling at `trust=0.4` (hung-key escalation, awaiting confirmation)
  beside one at 0.8.
- `min_quorum=2` active; `hung_rate` 0 again on both test halves.

## Limitations (unchanged)

- Labels are a written policy per domain, not objective ground truth —
  "judge disagrees" ≠ "judge is wrong".
- 30 test cases/domain ⇒ wide confidence intervals; use multi-seed runs
  (`summarize.py`) before reading small deltas.
- `judge` (cold) answers blind — kept for historical comparability;
  `judge_informed` is the fairer ceiling.

---

# Appendix — microbenchmarks (2026-09-19/20)

## Single-call latency floor

Small classification prompt, schema-enforced, three CLIs run in
parallel (2026-09-19):

| Juror | Wall-clock |
|---|---|
| `devin -p` (default model) | 6.1s |
| `claude -p --model haiku`, tools off + minimal system prompt | 6.3s |
| `claude -p --model haiku`, default | 7.4s |
| `codex exec`, `gpt-5.6-sol` effort low | 8.6s |

In parallel the wait equals the slowest juror. Memory can't lower this
floor; it helps speed by **reducing escalations to the strong tier** and
**reducing repo-exploration time**.

## Juror model selection — codex tiers (2026-09-20)

20-case support-triage, single juror, `--escalate off --no-cache
--no-memory`:

| Model `@low` | Wall (20 cases) | Accuracy | department | is_urgent | frustration |
|---|---|---|---|---|---|
| `gpt-5.6-terra` | 30.8s | 90% | 20/20 | 19/20 | 15/20 |
| `gpt-5.6-luna` | 31.3s | 90% | 19/20 | 19/20 | 16/20 |
| `gpt-5.6-sol` | 28.6s | 92% | 20/20 | 19/20 | 16/20 |

The three models are within noise of each other (sol ahead by exactly 1
key); misses cluster on `frustration` boundary labels — label noise, not
a model gap. Default juror is `codex:gpt-5.6-terra@low`; the high tier
(`gpt-5.6-sol@high`, `claude:opus@high`) is reserved for the **judge**,
where headroom pays because the judge decides the hard/hung cases.

## Backend throughput

Setup: 1 juror per run, `--min-quorum 1 --escalate off --no-cache
--no-memory`, adversarial cases (3 questions/case, one call answers all),
`max_concurrency=6` (default).

| Juror | single decide | batch@10 | batch@20 | batch@50 | ≈/case @50 |
|---|---|---|---|---|---|
| `devin:swe-2-medium` | 5.3s | 12.4s | 21.5s | 49.5s | ~1.0s |
| `codex:gpt-5.6-terra@low` | 7.2s | 17.5s | 36.6s | 74.5s | ~1.5s |
| `claude:haiku` | 8.8s | 48.7s | 63.4s | 115.1s | ~2.3s |

Reading: a single call costs ~5–9s regardless of backend (CLI spawn +
roundtrip dominate); in batch, throughput ≈ `max_concurrency` ÷ latency.
`devin:swe-2-medium` is both the fastest and free. Faster still: raise
`[limits] max_concurrency` (mind per-CLI rate limits), or enable
cache/memory to collapse repeat cases to ~0s.

## Execution modes — concurrent vs packed

`batch` has three execution modes worth distinguishing:

- **single** (`decide`): one case, one call per juror.
- **concurrent** (`batch`, default `--pack 1`): N cases → N calls per
  juror, parallelized up to `max_concurrency`.
- **packed** (`batch --pack N`): N same-questions cases share **one**
  call per juror; the juror returns per-item ballots
  `{"<item-id>": {answers}}`. Vote/quorum/hung/escalation still computed
  per item — only ballot *collection* is packed.

Direct measurements:

| Comparison | Result |
|---|---|
| `decide` ×10 sequential vs `batch` (devin, 10 cases) | 80.8s vs 14.2s — **~5.7× faster** |
| `batch` vs `batch --pack 12` (all-devin jury, 12 emails) | ~33s/36 calls vs **9.2s/3 calls** — accuracy on par (94% vs 92–94% across two runs; evidence committed at `examples/email-classification/results-pack12.jsonl`) |

Rule: always `batch` for N>1; add `--pack N` when the per-item states
are short (each item's text lands in one shared prompt — keep N small,
4–12, for long states). Packed and unpacked results never share cache
entries (pack size is in the key); workspace states are rejected under
`--pack`. Per-item hung detection is preserved — in the email spike the
empty `"?"` mail hung inside the pack while its 11 neighbours decided.
