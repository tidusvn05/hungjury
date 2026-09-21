# AGENTS.md — hungjury

Rust CLI: typed decisions (choice/score/noul) from unstructured state via a
parallel jury of headless CLI agents (claude/codex/devin), hung-jury
escalation to a judge, and SQLite/FTS5 precedent memory.

## Git conventions

- **Author identity — mandatory:** every commit must be authored by
  `Tidusvn05 <tidusvn05@gmail.com>`. Do NOT touch `git config`; set it
  per-command:

  ```bash
  GIT_AUTHOR_NAME="Tidusvn05" GIT_AUTHOR_EMAIL="tidusvn05@gmail.com" \
  GIT_COMMITTER_NAME="Tidusvn05" GIT_COMMITTER_EMAIL="tidusvn05@gmail.com" \
  git commit -m "..."
  ```

- **Remote:** `origin` = `git@github.com-tidusvn05:tidusvn05/hungjury.git`
  (SSH alias `github.com-tidusvn05` → key `~/.ssh/id_ed25519_tidusvn05`,
  account `tidusvn05`). Plain `github.com` uses a different key/account.
- **No `gh` CLI** — the local `gh` is authenticated as a different account
  with no write access to this repo. Git/SSH only.
- Push explicitly; never rewrite remote history/tags without asking.

## Verify

```bash
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check        # keep fmt-clean
```

## Release process

Release automation lives in `.github/workflows/release.yml`, triggered by
pushing a `v*` tag. The tag must point at a commit that **contains** the
workflow file, and two guards run before anything is built:

1. Tag version must equal `version` in `Cargo.toml`.
2. `CHANGELOG.md` must contain a `## [X.Y.Z] - DATE` section for that
   version — its body becomes the GitHub release notes.

Steps:

```bash
# 1. Bump version in Cargo.toml; move/add the "## [X.Y.Z] - YYYY-MM-DD"
#    section in CHANGELOG.md (keepachangelog format).
#    Then run `cargo check` so Cargo.lock records the new version —
#    the verify job uses --locked and fails if the lockfile is stale.
# 2. Commit with the Tidusvn05 identity above.
# 3. Tag (annotated) and push:
git tag -a vX.Y.Z -m "hungjury vX.Y.Z — short summary"
git push origin main vX.Y.Z
```

The workflow then runs `cargo test` + `clippy`, builds release binaries for
linux-x86_64, macOS-arm64, macOS-x86_64, windows-x86_64, generates
`checksums.txt` (sha256), and creates the GitHub release with the extracted
changelog section. Tags containing `-` (e.g. `v0.2.0-rc.1`) are marked
pre-release automatically.

If a tag was pushed before the workflow existed, re-point it at a commit
that has `.github/workflows/release.yml` and force-push the tag (ask first).

## Layout

- `src/` — CLI (`main.rs` dispatch), jury/`agent` backends, `judge.rs`,
  `memory/` (SQLite store + FTS5), `eval.rs`, `learn.rs`, `lint.rs`,
  `doctor.rs`, `batch.rs`, `config.rs`, `project_dir.rs`
- `examples/` — seven self-contained `.hungjury/` projects with rubrics +
  labeled `cases.jsonl` + `score.py`
- `bench/` — 7-domain generators + adversarial 64-case set + `run_bench.sh`
- `sdk/python`, `sdk/typescript` — thin wrappers over the binary
- `docs/USAGE.md` — full usage guide; `docs/DESIGN.md` — architecture;
  `docs/PLAN.md` — phase plan; `docs/REPORT-*.md` — dated reports;
  `docs/BENCHMARK.md` — benchmarks incl. throughput/pack appendix
- `CHANGELOG.md` — release notes source (keepachangelog)

## Eval notes

- `hungjury eval cases.jsonl --seeds N --seed S --audit-train K --label L`
  → writes `eval-L.json`; each seed uses a throwaway memory DB.
- `expected: "hung"` is a valid label (abstention cases).
- Exit `2` from decide/batch means hung keys — a valid outcome, not an error.
- Juror backends burn real CLI quota; project `[limits] daily_cap` gates it.
