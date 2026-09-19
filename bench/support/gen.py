#!/usr/bin/env python3
"""Generate the labeled eval set for `hungjury eval` (Phase 2 go/no-go).

120 synthetic support tickets over 3 questions:
  - department: choice(billing|technical|sales)
  - frustration: score(0|1|2)
  - is_urgent: noul(bool)

Labels follow a fixed policy so a judge's rulings can generalize:
  * charged wrongly / money owed / refund        -> billing
  * payment cannot complete due to a bug         -> technical (fix root cause)
  * pricing/plan/quote questions                 -> sales
  * product defects with no money question       -> technical
  * frustration: 0 calm, 1 frustrated, 2 angry (tone markers)
  * is_urgent: explicit deadline/blocking/legal or "ASAP" language
"""
import json, random

rng = random.Random(20260919)

Q = {
    "department": {
        "type": "choice",
        "id": "support.department",
        "instructions": "Which team should handle this ticket",
        "criteria": {
            "billing": "Payment, charge, refund or subscription money issues",
            "technical": "Bugs, crashes, errors or integration problems",
            "sales": "Pricing, plan or account questions before purchase",
        },
    },
    "frustration": {
        "type": "score",
        "id": "support.frustration",
        "instructions": "How frustrated the customer appears",
        "criteria": [
            "Calm, just stating facts",
            "Frustrated but civil",
            "Very angry, strong language",
        ],
    },
    "is_urgent": {
        "type": "noul",
        "id": "support.is_urgent",
        "instructions": "The message conveys urgency or time-sensitivity (deadline, blocking, ASAP)",
    },
}

CALM = ["Just reporting.", "FYI.", "No rush on my side.", "Let me know."]
FRUST = [
    "This is really frustrating.",
    "Pretty annoyed at this point.",
    "Not happy about this.",
    "Getting tired of this issue.",
]
ANGRY = [
    "This is absolutely unacceptable!!",
    "I'm FURIOUS. Worst experience ever.",
    "RIDICULOUS. Fix this NOW.",
    "Unbelievable — I'm about to cancel everything.",
]
URG = [
    "This is blocking our month-end close — need it fixed ASAP.",
    "We're launching tomorrow; this is a hard blocker.",
    "Our legal deadline is Friday.",
    "Production is down for all users — urgent.",
    "Time-sensitive: payroll runs tonight.",
]
NOTURG = ["Whenever you get a chance.", "No particular deadline.", "Not urgent.", ""]

AMTS = ["$19", "$29", "$49", "$99", "$149", "$240", "$499", "$1,200"]
PLANS = ["Pro", "Team", "Business", "Starter", "Annual Pro"]
MONTHS = ["January", "March", "May", "August", "October"]
FEATS = ["invoices tab", "checkout page", "billing settings", "export button",
         "login form", "dashboard", "mobile app", "API playground"]

cases = []
def add(state, dept, frust, urg):
    cases.append({"state": state, "questions": Q,
                  "expected": {"department": dept, "frustration": frust, "is_urgent": urg}})

# ---------- clear billing (30) ----------
billing_t = [
    "I was charged twice for my {plan} subscription — {amt} on the 3rd and again on the 5th. Please refund the duplicate. {tone} {urg}",
    "My invoice for {month} shows {amt} but my plan is {plan} which should be cheaper. Can you correct the charge? {tone} {urg}",
    "I cancelled my subscription last month and I was still billed {amt}. I want a refund. {tone} {urg}",
    "There's a {amt} charge from your company on my card that I don't recognize. What is it for? {tone} {urg}",
    "I need a refund for the {plan} plan — I was charged {amt} but never activated the account. {tone} {urg}",
    "The renewal price went up without warning; I was charged {amt} instead of the old rate. Please honor the original price or refund. {tone} {urg}",
]
for i in range(30):
    t = rng.choice(billing_t)
    fr = rng.choices([0, 1, 2], weights=[3, 4, 3])[0]
    urg = rng.random() < 0.4
    tone = [rng.choice(CALM), rng.choice(FRUST), rng.choice(ANGRY)][fr]
    u = rng.choice(URG) if urg else rng.choice(NOTURG)
    add(t.format(plan=rng.choice(PLANS), amt=rng.choice(AMTS), month=rng.choice(MONTHS),
                 tone=tone, urg=u), "billing", fr, urg)

# ---------- clear technical (30) ----------
tech_t = [
    "The {feat} crashes every time I open it. Started after the last update. {tone} {urg}",
    "Getting HTTP 500 from your REST API on the /v1/orders endpoint since this morning. {tone} {urg}",
    "The Salesforce integration stopped syncing contacts two days ago. No error, just silent failure. {tone} {urg}",
    "Login with SSO fails with 'invalid_state' on Chrome and Firefox. {tone} {urg}",
    "Exported CSVs are corrupted — dates shift by one day for rows after row 500. {tone} {urg}",
    "The mobile app freezes on the splash screen for ~10s on Android 15. {tone} {urg}",
]
for i in range(30):
    t = rng.choice(tech_t)
    fr = rng.choices([0, 1, 2], weights=[4, 4, 2])[0]
    urg = rng.random() < 0.4
    tone = [rng.choice(CALM), rng.choice(FRUST), rng.choice(ANGRY)][fr]
    u = rng.choice(URG) if urg else rng.choice(NOTURG)
    add(t.format(feat=rng.choice(FEATS), tone=tone, urg=u), "technical", fr, urg)

# ---------- clear sales (15) ----------
sales_t = [
    "Does the {plan} plan include SSO? Comparing it with Team before we buy. {tone} {urg}",
    "Can you send a quote for 120 seats on {plan}? Also is there an education discount? {tone} {urg}",
    "What's the difference between {plan} and Enterprise for data retention? {tone} {urg}",
    "Is there a monthly billing option for {plan}, or is it annual only? {tone} {urg}",
    "We're evaluating tools — do you offer a trial extension past 14 days? {tone} {urg}",
]
for i in range(15):
    t = rng.choice(sales_t)
    fr = 0 if rng.random() < 0.8 else 1
    urg = rng.random() < 0.2
    tone = [rng.choice(CALM), rng.choice(FRUST), rng.choice(ANGRY)][fr]
    u = rng.choice(URG) if urg else rng.choice(NOTURG)
    add(t.format(plan=rng.choice(PLANS), tone=tone, urg=u), "sales", fr, urg)

# ---------- borderline: money lost + bug mentioned (20) -> billing ----------
border_b_t = [
    "Your app crashed during checkout and charged me {amt} twice. I want the duplicate refunded. {tone} {urg}",
    "A bug in your renewal flow billed me for {plan} twice this month — {amt} total. Refund please. {tone} {urg}",
    "The payment page errored out but the charge still went through ({amt}). Please reverse it. {tone} {urg}",
    "Your API timeout retried the payment call and my card shows two {amt} charges. Need one refunded. {tone} {urg}",
]
for i in range(20):
    t = rng.choice(border_b_t)
    fr = rng.choices([1, 2], weights=[5, 5])[0]
    urg = rng.random() < 0.5
    tone = [rng.choice(CALM), rng.choice(FRUST), rng.choice(ANGRY)][fr]
    u = rng.choice(URG) if urg else rng.choice(NOTURG)
    add(t.format(amt=rng.choice(AMTS), plan=rng.choice(PLANS), tone=tone, urg=u), "billing", fr, urg)

# ---------- borderline: money flow broken, fix bug first (15) -> technical ----------
border_t_t = [
    "I can't update my credit card — the billing settings page throws a JavaScript error and won't save. {tone} {urg}",
    "The 'Pay invoice' button returns a 500 error, so I'm unable to pay you. {tone} {urg}",
    "Checkout keeps failing with 'card declined' for every card I try — other sites work fine. {tone} {urg}",
    "Trying to downgrade to {plan} but the portal crashes before I can confirm. {tone} {urg}",
]
for i in range(15):
    t = rng.choice(border_t_t)
    fr = rng.choices([0, 1, 2], weights=[2, 5, 3])[0]
    urg = rng.random() < 0.5
    tone = [rng.choice(CALM), rng.choice(FRUST), rng.choice(ANGRY)][fr]
    u = rng.choice(URG) if urg else rng.choice(NOTURG)
    add(t.format(plan=rng.choice(PLANS), tone=tone, urg=u), "technical", fr, urg)

# ---------- borderline: usage/limits vs bug (10) -> sales or technical ----------
border_s_t = [
    ("We're hitting 'rate limit exceeded' on the {plan} trial — is that a bug or the plan cap? Do we need to upgrade? {tone} {urg}", "sales"),
    ("The export feature says 'upgrade required' — we thought it was included in {plan}. Is this a display bug or a plan thing? {tone} {urg}", "sales"),
    ("API quota seems to reset at random times on {plan} — bug or expected behavior for this tier? {tone} {urg}", "sales"),
]
for i in range(10):
    t, dept = rng.choice(border_s_t)
    fr = rng.choices([0, 1], weights=[6, 4])[0]
    urg = rng.random() < 0.3
    tone = [rng.choice(CALM), rng.choice(FRUST), rng.choice(ANGRY)][fr]
    u = rng.choice(URG) if urg else rng.choice(NOTURG)
    add(t.format(plan=rng.choice(PLANS), tone=tone, urg=u), dept, fr, urg)

rng.shuffle(cases)
with open("tests/data/cases.jsonl", "w") as f:
    for c in cases:
        f.write(json.dumps(c, ensure_ascii=False) + "\n")
print(f"wrote {len(cases)} cases")
from collections import Counter
print(Counter(c["expected"]["department"] for c in cases))
print("urgent:", sum(c["expected"]["is_urgent"] for c in cases),
      "| frustration:", Counter(c["expected"]["frustration"] for c in cases))
