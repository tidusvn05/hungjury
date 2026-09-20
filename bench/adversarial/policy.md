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

- `2` — strong wording ("ridiculous", "unacceptable", "beyond
  frustrated") OR polite wording that names a *penalty landing if
  unresolved*: chargeback, legal obligation, staff sitting idle,
  escalation threat. A deadline alone is urgency, not frustration.
- `1` — mild annoyance or civil complaint, no stakes named:
  "a bit annoying", "kind of a pain", "would appreciate a fix".
- `0` — calm or neutral, purely factual, no annoyance markers.

## is_urgent

- `true` for an explicit deadline or blocking impact
  ("renewal Friday", "launch in 6 hours", "blocking payroll") — a
  *named consequence that will occur if unresolved* also counts:
  "escalates to a chargeback", "agents idle until fixed",
  "audit closes tomorrow" → `true`.
- Courtesy urgency does NOT count: "ASAP would be nice",
  "whenever you can", "no rush" → `false`.
