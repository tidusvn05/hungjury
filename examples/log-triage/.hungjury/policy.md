# CI failure triage policy

## flaky

- `true` — timeouts, network blips, runner OOM, known-flaky test names,
  "works on retry" signatures.
- `false` — assertion failures, compile errors, deterministic crashes,
  missing files/migrations.

## actionable

- `true` — the log shows something a developer can fix (assertion diff,
  compile error, missing migration, bad config).
- `false` — infra noise needing a retry, not a code change.

## severity

- `2` — release-blocking: all tests red, deploy pipeline down.
- `1` — a real failure in one lane.
- `0` — flaky/infra noise.
