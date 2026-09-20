#!/usr/bin/env python3
"""Generate the `adversarial` eval set (~60 cases).

Use case: support tickets engineered to split the jury — contradictory
signals of roughly equal weight. Same question ids as `support` so judge
rulings from the train arm can generalize to the test arm.

Labeling policy (deterministic even when humans might disagree):
  * department : the FIRST concrete request in the message is primary
                 ("charged twice... and the app keeps crashing" -> billing;
                  "fix the crash... also it billed me twice" -> technical)
  * plan-limit ambiguity ("is this a bug or a tier cap?") -> sales when the
                 user is asking whether behavior is intended
  * frustration: strong language -> 2, mild -> 1, none -> 0 (polite wording
                 with a threat still counts as strong if it names consequences)
  * is_urgent  : explicit deadline/blocking only; courtesy "ASAP would be
                 nice / whenever you can" -> false
"""
import json, random

rng = random.Random(20260925)

Q = {
    "department": {
        "type": "choice",
        "id": "support.department",
        "instructions": "Which team should handle this ticket",
        "criteria": {
            "billing": "Payment, charge, refund or subscription money issues",
            "technical": "Bugs, crashes, errors or integration problems",
            "sales": "Pricing, plan or account questions before purchase — including 'is this a bug or an intended plan limit / what does the upgrade cost?'",
        },
    },
    "frustration": {
        "type": "score",
        "id": "support.frustration",
        "instructions": "How frustrated the customer appears",
        "criteria": [
            "Calm or neutral — 'fyi', 'just checking in'; purely factual, no annoyance markers at all",
            "Mild annoyance or civil complaint ('a bit annoying', 'kind of a pain', 'would appreciate a fix') — no strong wording, no penalties named",
            "Strong wording ('ridiculous', 'unacceptable', 'beyond frustrated') OR names a penalty that lands if unresolved (chargeback, legal obligation, staff sitting idle, escalation threat). A deadline alone counts as urgency, not frustration",
        ],
    },
    "is_urgent": {
        "type": "noul",
        "id": "support.is_urgent",
        "instructions": "Real urgency: an explicit deadline OR a consequence that will occur if unresolved (blocking launch, chargeback, staff idle, audit). Courtesy phrases ('ASAP would be nice', 'no rush', 'whenever you can') do NOT count.",
    },
}

cases = []
def add(state, dept, frust, urg):
    cases.append({"state": state, "questions": Q,
                  "expected": {"department": dept, "frustration": frust, "is_urgent": urg}})

MILD = ["A bit annoying.", "Would appreciate a fix.", "Kind of a pain."]
STRONG = ["This is ridiculous.", "Unacceptable, honestly.", "Beyond frustrated now."]

# ---------- A: billing-first vs technical-first, both signals present (24) ----------
bill_first = [
    "I was charged {amt} twice and I want a refund — also, the checkout page crashes half the time which is probably what caused it. {tone} {urg}",
    "Please refund the duplicate {amt} charge from Monday. While you're at it, your payment API timed out three times before it went through. {tone} {urg}",
    "Money first: my card shows two {amt} debits for one order. FYI the app also froze mid-checkout. {tone} {urg}",
]
tech_first = [
    "The checkout page crashes every other attempt — and by the way it also charged me {amt} twice on the one that half-worked. {tone} {urg}",
    "Your payment API keeps timing out (500s). One retry apparently did charge my card {amt} — sort that out after the API is fixed. {tone} {urg}",
    "Fix the crash in checkout first — it's why I got billed {amt} twice in the first place. {tone} {urg}",
]
for i in range(12):
    t = rng.choice(bill_first)
    fr = rng.choices([1, 2], weights=[6, 4])[0]
    urg = rng.random() < 0.4
    add(t.format(amt=rng.choice(["$29", "$79", "$149"]), tone=rng.choice(MILD if fr == 1 else STRONG),
                 urg="Blocking our launch tomorrow." if urg else "No huge rush."),
        "billing", fr, urg)
for i in range(12):
    t = rng.choice(tech_first)
    fr = rng.choices([1, 2], weights=[6, 4])[0]
    urg = rng.random() < 0.4
    add(t.format(amt=rng.choice(["$29", "$79", "$149"]), tone=rng.choice(MILD if fr == 1 else STRONG),
                 urg="Blocking our launch tomorrow." if urg else "No huge rush."),
        "technical", fr, urg)

# ---------- B: bug-vs-plan ambiguity -> sales when asking "intended?" (12) ----------
amb_t = [
    "Exports stop at exactly 1,000 rows — is that a bug or a {plan} plan limit? If it's a limit, what's the upgrade cost? {tone} {urg}",
    "The API returns 'rate limit exceeded' after ~50 calls on {plan}. Bug, or is the tier actually capped there? {tone} {urg}",
    "My dashboard shows 'feature locked — upgrade required' for SSO even though I thought {plan} included it. Bug in entitlement, or wrong expectation? {tone} {urg}",
    "Storage uploads fail past 5GB on {plan} — is that a defect or the documented cap? If cap, what tier removes it? {tone} {urg}",
]
for i in range(12):
    t = rng.choice(amb_t)
    fr = rng.choices([0, 1], weights=[5, 5])[0]
    urg = rng.random() < 0.3
    add(t.format(plan=rng.choice(["Pro", "Team", "Starter"]),
                 tone=rng.choice(MILD) if fr == 1 else "",
                 urg="Needed for a board demo Thursday." if urg else ""),
        "sales", fr, urg)

# ---------- C: urgency-signal traps (12) — courtesy ASAP -> false; buried deadline -> true ----------
courtesy = [
    "Whenever you get a chance — ASAP would be lovely but honestly no deadline: the export button on the report page 404s. {tone}",
    "No rush at all (though sooner is always nicer!): the dark-mode toggle resets on refresh. {tone}",
    "Totally fine if this takes a while — the saved-filters dropdown occasionally duplicates entries. {tone}",
]
for i in range(6):
    t = rng.choice(courtesy)
    fr = rng.choices([0, 1], weights=[6, 4])[0]
    add(t.format(tone=rng.choice(MILD) if fr == 1 else ""), "technical", fr, False)

buried = [
    ("Just a heads-up, nothing major — oh and our contract renewal is Friday so we'd need the {amt} double-charge fixed before then. {tone}", "billing"),
    ("Minor thing: the invoice PDF shows the wrong VAT number. Our audit closes tomorrow though, so it does matter by then. {tone}", "billing"),
    ("Small bug, big deadline: profile photos won't upload and our public launch is in 6 hours. {tone}", "technical"),
]
for i in range(6):
    t, dept = rng.choice(buried)
    fr = rng.choices([0, 1], weights=[5, 5])[0]
    add(t.format(amt=rng.choice(["$49", "$120"]), tone=rng.choice(MILD) if fr == 1 else ""),
        dept, fr, True)

# ---------- D: politeness vs consequence (12) — courteous wording + named stakes ----------
polite_threat = [
    "Hello! Kindly refund the {amt} duplicate charge — our accounting flags every mismatch and this will escalate to a chargeback if unresolved.",
    "Good day. The login outage persists; our 200-agent support desk is idle until it's fixed. Warm regards.",
    "Hi team, gentle reminder: data export has been broken 9 days. Legal requires it for the case file due Monday.",
]
for i in range(12):
    t = rng.choice(polite_threat)
    # named hard consequences -> frustration 2 (per policy: consequences named = strong)
    add(t.format(amt=rng.choice(["$60", "$210"])), "billing" if "charge" in t or "refund" in t.lower() else "technical",
        2, True)

# ---------- E: information-free states (4) ----------
# Only `department` should abstain -> hung: an empty ticket genuinely IS
# calm (frustration 0) and not urgent (false) — those are defensible
# defaults, not guesses. `department` has no defensible value.
empty_states = [
    "ok",
    "(empty ticket — no message body)",
    "test",
    "following up",
]
for t in empty_states:
    add(t, "hung", 0, False)

rng.shuffle(cases)
with open("bench/adversarial/cases.jsonl", "w") as f:
    for c in cases:
        f.write(json.dumps(c, ensure_ascii=False) + "\n")
from collections import Counter
print(f"wrote {len(cases)}")
print(Counter(c["expected"]["department"] for c in cases))
print("urgent:", sum(1 for c in cases if c["expected"]["is_urgent"] is True),
      "| frustration:", Counter(c["expected"]["frustration"] for c in cases))
