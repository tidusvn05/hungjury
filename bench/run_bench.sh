#!/usr/bin/env bash
# Run one benchmark domain through `hungjury eval`.
# Usage: bench/run_bench.sh <domain> [extra eval flags]
# Env overrides: SEED (42), REPORT (report.json), LABEL (domain name),
# BENCH_HOME (bench/<domain>/home — set it to isolate memory for variants).
# Extra args are passed through to `hungjury eval` — global flags like
# --hung-threshold / --jurors work here too (clap global args).
set -euo pipefail
cd "$(dirname "$0")/.."

D="${1:?usage: run_bench.sh <domain>}"
shift

SEED="${SEED:-42}"
REPORT="${REPORT:-report.json}"
LABEL="${LABEL:-$D}"

HOME_DIR="${BENCH_HOME:-$PWD/bench/$D/home}"
mkdir -p "$HOME_DIR"
export HUNGJURY_HOME="$HOME_DIR"

BIN=./target/debug/hungjury
[ -x "$BIN" ] || { echo "build first: cargo build"; exit 1; }

echo "== bench/$D  home=$HOME_DIR  seed=$SEED  report=$REPORT  label=$LABEL =="
"$BIN" --config bench/bench.toml eval "bench/$D/cases.jsonl" \
  --train-frac 0.5 --report "bench/$D/$REPORT" --seed "$SEED" --label "$LABEL" "$@"
