# Labeling policy — adversarial support triage

Deterministic rules used by the benchmark labels. When signals conflict,
apply these in order rather than your own rubric.

## department

- The FIRST concrete request in the message is primary.
  "Charged twice… and the app keeps crashing" → `billing`;
  "Fix the crash… also it billed me twice" → `technical`.
- Bug-vs-plan-limit ambiguity ("is this a bug or a tier cap?") → `sales`
  when the user is asking whether the behavior is intended, or asking
  what an upgrade costs.

## frustration

- `2` — strong language OR polite wording that names hard consequences
  (chargeback, legal deadline, staff idle, audit closing).
- `1` — mild annoyance, civil frustration, no stakes named.
- `0` — calm, just stating facts.

## is_urgent

- `true` only for an explicit deadline or blocking impact
  ("renewal Friday", "launch in 6 hours", "blocking payroll").
- Courtesy urgency does NOT count: "ASAP would be nice",
  "whenever you can", "no rush" → `false`.
