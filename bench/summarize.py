#!/usr/bin/env python3
"""Summarize bench reports — mean ± stdev per arm, grouped by label.

Globs bench/*/report*.json, groups rows by the report's `label`, and
prints per-arm accuracy / hung_rate / calls / latency. Multiple reports
with the same label (e.g. different seeds) are averaged.

Usage: python3 bench/summarize.py [glob]   # default: bench/*/report*.json
"""
import glob
import json
import statistics
import sys

ARMS = ["jury", "jury_memory", "judge", "judge_informed"]
METRICS = ["accuracy", "hung_rate", "calls", "wall_ms_mean", "wall_ms_p95"]


def fmt(vals, pct=False):
    if not vals:
        return "—"
    m = statistics.fmean(vals)
    if len(vals) == 1:
        return f"{m:5.1%}" if pct else f"{m:9.0f}"
    s = statistics.stdev(vals)
    return (f"{m:5.1%}±{s:4.1%}" if pct else f"{m:9.0f}±{s:6.0f}") + f" (n={len(vals)})"


def main():
    pattern = sys.argv[1] if len(sys.argv) > 1 else "bench/*/report*.json"
    groups = {}
    for path in sorted(glob.glob(pattern)):
        try:
            r = json.load(open(path))
        except (OSError, json.JSONDecodeError) as e:
            print(f"skip {path}: {e}", file=sys.stderr)
            continue
        label = r.get("label") or path.split("/")[-2]
        groups.setdefault(label, []).append((path, r))

    for label in sorted(groups):
        runs = groups[label]
        print(f"\n== {label} — {len(runs)} report(s) ==")
        for _, r in runs:
            cfg = r.get("config", {})
            print(
                f"  seed={r.get('seed')} test={r.get('test')} "
                f"jurors={','.join(cfg.get('jurors', []))} "
                f"hung={cfg.get('hung_threshold', '?')}"
            )
        for arm in ARMS:
            cols = {m: [] for m in METRICS}
            for _, r in runs:
                a = r.get("arms", {}).get(arm)
                if not a:
                    break
                for m in METRICS:
                    if m in a:
                        cols[m].append(a[m])
            else:
                print(
                    f"  {arm:15} acc={fmt(cols['accuracy'], True)} "
                    f"hung={fmt(cols['hung_rate'], True)} "
                    f"calls={fmt(cols['calls'])} "
                    f"wall_mean={fmt(cols['wall_ms_mean'])} "
                    f"p95={fmt(cols['wall_ms_p95'])}"
                )


if __name__ == "__main__":
    main()
