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

- `true` for an explicit deadline or blocking impact
  ("renewal Friday", "blocking payroll", "launch in 6 hours") — a
  *named consequence that will occur if unresolved* also counts:
  "escalates to a chargeback", "200 agents idle until fixed",
  "audit closes tomorrow" → `true`.
- Courtesy urgency does NOT count: "ASAP would be nice",
  "whenever you can", "no rush" → `false`.

## frustration

- `2` — strong wording ("ridiculous", "unacceptable", "beyond
  frustrated") OR polite wording that names a *penalty landing if
  unresolved*: chargeback, legal obligation, staff sitting idle,
  escalation threat. A deadline alone is urgency, not frustration.
- `1` — mild annoyance or civil complaint: "a bit annoying",
  "kind of a pain", "would appreciate a fix".
- `0` — calm or neutral, purely factual, no annoyance markers.
