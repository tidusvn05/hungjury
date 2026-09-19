#!/usr/bin/env bash
# Run one benchmark domain through `hungjury eval`.
# Usage: bench/run_bench.sh <domain> [extra eval flags]
# Each domain gets an isolated HUNGJURY_HOME (own memory/cache/quota log).
set -euo pipefail
cd "$(dirname "$0")/.."

D="${1:?usage: run_bench.sh <domain>}"
shift

HOME_DIR="$PWD/bench/$D/home"
mkdir -p "$HOME_DIR"
export HUNGJURY_HOME="$HOME_DIR"

BIN=./target/debug/hungjury
[ -x "$BIN" ] || { echo "build first: cargo build"; exit 1; }

echo "== bench/$D  home=$HOME_DIR =="
"$BIN" --config bench/bench.toml eval "bench/$D/cases.jsonl" \
  --train-frac 0.5 --report "bench/$D/report.json" --seed 42 --label "$D" "$@"
