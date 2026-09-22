# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-09-22

### Added

- Eval reports now carry audit fields — `train_escalations`,
  `rulings_written`, `contested_written`, `unscored` — with a test
  asserting the accounting invariant `decided + hung + unscored =
  total`.
- `bench/microbench.sh` — microbenchmark runner that writes raw
  `calls.jsonl` evidence per run.
- `docs/REPORT-2026-09-21.md` — audit of the published benchmark
  claims.
- Devin-only multi-seed benchmark reruns (3 seeds) for `email_intent`,
  `log_triage`, `multilingual`, `workspace`, and `support`, with
  per-seed reports and committed `home_devin_s*` evidence.

### Fixed

- Corrected benchmark claims in README and `docs/BENCHMARK.md`:
  `jury >= judge` holds on 4/7 domains (not 5/7), worst-case memory
  harm is -13.3pts with mean -11.1 (not -17), and the workspace domain
  hung 3/90 cases (not 1).

## [0.2.1] - 2026-09-21

### Added

- `examples/email-classification/results-pack12.jsonl` — committed
  evidence for `batch --pack 12`: 12 cases answered by 3 juror calls
  (per-item hung preserved: the empty mail hung inside the pack while
  its 11 neighbours decided).

### Changed

- Packed-mode docs now report both runs honestly (92–94% accuracy at
  ~9s/3 calls vs ~33s/36 calls unpacked) instead of the first run alone.

## [0.2.0] - 2026-09-21

### Added

- `hungjury batch --pack N` — prompt batching: up to N consecutive
  same-questions cases share one juror call (`{"<item_id>": {answers}}`
  per-item ballots). Vote/quorum/hung/escalation/memory/cache stay
  per-item; quota charges per call and estimated cost amortizes across
  the pack. Item ids carry a per-pack nonce so state content can't forge
  a neighbouring item's tag; a malformed item degrades to "no ballot"
  instead of failing the pack. Measured on the email-classification
  spike: same 94% accuracy at ~3.6× speed and 12× fewer calls.
  Workspace states are rejected (they can't share a prompt); `--pack 1`
  (default) is unchanged behaviour.
- `examples/email-classification` — inbox triage use case run entirely
  through the `devin` CLI (`swe-2-medium` + `gpt-5-6-luna-low` +
  `gemini-3-8-flash-low` jurors, `claude-opus-5-high` judge).

### Changed

- README reorganized into a landing page; detailed usage moved to
  `docs/USAGE.md`, architecture to `docs/DESIGN.md`, and all
  microbenchmarks (latency floor, codex juror tiers, backend throughput,
  concurrent-vs-packed) consolidated in `docs/BENCHMARK.md`.

## [0.1.0] - 2026-09-20

Initial release.

`hungjury` is a CLI that makes typed decisions (`choice`, `score`, `noul`)
from unstructured state using real headless CLI agents (`claude`, `codex`,
`devin`) as parallel jurors. Disagreement produces a *hung jury* that
escalates to a stronger judge; judge rulings are persisted to local
SQLite/FTS5 memory so future juries learn precedents and rarely need the
judge again.

### Added

#### Decision engine

- `hungjury decide` — typed answers with per-key confidence, decider
  attribution (`sources`), and escalation status; exit codes `0`
  (resolved), `2` (hung/abstained — a valid outcome), `1` (error).
- Parallel juror backends for `claude`, `codex`, and `devin` CLIs with
  model/effort selection (`judge = "claude:opus@high"`).
- `"abstain"` ballots on every question type — jurors can decline when
  no defensible answer exists; all-abstain leaves the key hung.
- Escalation policies: `sync` (judge answers immediately), `queue`
  (defer to `learn --queue`), `off`.
- Score-bimodal hung detection, quorum checks (`min_quorum`), and
  disagreement threshold (`hung_threshold`).
- Environment sanitization — billing/API-key variables are stripped
  before spawning child agent processes.
- Mutex-guarded daily call quota (`[limits] daily_cap`).

#### Memory

- Local SQLite/FTS5 store for rulings, precedents, workspace facts,
  contested entries, decisions, and feedback — with trust levels,
  provisional→active promotion, supersede chains, tombstones, and
  evidence hashes.
- `memory decisions` audit trail showing per-key sources and escalations.
- `memory list / review / export / import` — inspection, contested-entry
  review, and portable JSONL sync/merge across machines.
- Policy-file injection (`.hungjury/policy.md`) into every prompt;
  retrieved rulings are subordinated to policy ("policy wins").
- Retrieval injected into juror/judge prompts with trust labels.

#### Operations

- `init` — scaffolds `.hungjury/` with config, policy, and a lint-clean
  anchored `questions.json` skeleton; project discovery walks upward
  (`.hungjury/` or `hungjury.toml`).
- `batch` — JSONL workloads with per-line or shared `questions_file`.
- `learn --audit` / `learn --queue` — re-judge recent decisions into
  rulings/precedents, or drain the escalation queue; corrupt rows are
  logged and skipped instead of aborting.
- `feedback` — human overrides that demote/contest bad memory.
- `doctor` — read-only diagnostics (backends, quota, FTS5, memory
  engagement warnings); never mutates state.
- `lint` — deterministic rubric checks: abstract score levels without
  marker words, missing exclusion clauses, overlapping choice criteria.

#### Evaluation

- `eval` — four arms (`jury`, `jury_memory`, `judge`, `judge_informed`)
  on labeled JSONL, with `expected: "hung"` support for abstention
  cases.
- `--seeds N` multi-seed runs: each seed gets a throwaway memory DB,
  reports carry `aggregate` mean/min/max, `per_key_mean`, and a
  `go_pass` verdict; seed failures are recorded without losing
  completed seeds.
- `--audit-train K` — judge re-judges training decisions so memory is
  populated even when the jury never hangs.
- Per-key accuracy, memory-injection counts, latency, call counts, and
  estimated cost per arm; default report path `eval-<label>.json`.

#### SDKs & examples

- Python SDK (`sdk/python`) and zero-dependency TypeScript SDK
  (`sdk/typescript`) exposing `decide`, `systemOne`, `feedback`.
- Six runnable example projects: `support-triage`, `pr-review`,
  `log-triage`, `spam-filter`, `support-routing`, `content-moderation` —
  each with lint-clean rubrics and hand-labeled cases.
- Seven-domain synthetic benchmark suite plus a 64-case adversarial
  set (`bench/`).

### Notes

- Multi-seed adversarial eval (64 cases × 3 seeds, `--audit-train 8`):
  jury 95.7% mean, jury+memory 98.6%, judge 100%; memory delta +2.9pts
  with `go_pass` 3/3. All figures are on hand-labeled/synthetic
  distributions — validate on real data before production use.
- Known limitation: `promote_rulings` promotes provisional rulings at
  question scope rather than per-decision (memory entries carry no
  decision linkage); demote/contest paths remain available for
  correction.

[0.3.0]: https://github.com/tidusvn05/hungjury/releases/tag/v0.3.0
[0.1.0]: https://github.com/tidusvn05/hungjury/releases/tag/v0.1.0
