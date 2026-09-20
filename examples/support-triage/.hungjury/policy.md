# Support triage policy

Deterministic rules for routing tickets. When signals conflict, apply
these in order rather than your own rubric.

## department

- The FIRST concrete request in the message is primary.
  "Charged twice… and the app keeps crashing" → `billing`;
  "Fix the crash… also it billed me twice" → `technical`.
- Pre-purchase or plan questions ("is X included?", "upgrade cost?")
  → `sales` even when the message also mentions a bug.

## urgency

- `true` only for an explicit deadline or blocking impact
  ("renewal Friday", "blocking payroll", "launch in 6 hours").
- Courtesy urgency does NOT count: "ASAP would be nice",
  "whenever you can", "no rush" → `false`.

## frustration

- `2` — strong language OR polite wording that names hard consequences
  (chargeback, legal deadline, audit closing).
- `1` — mild annoyance, civil frustration.
- `0` — calm, just stating facts.
