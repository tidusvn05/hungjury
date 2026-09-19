#!/usr/bin/env python3
"""Generate the `email_intent` eval set (~60 cases).

Use case: triage inbound customer/community messages.

State: an email / forum post / DM.

Questions:
  - intent:        choice(bug|feature|question|churn)
  - sentiment:     score(0|1|2)  0=positive, 1=neutral, 2=negative
  - respond_today: noul          should a human answer within 24h

Labeling policy:
  * intent        : dominant ask (reports broken behavior -> bug; asks for capability -> feature;
                    how-to/unclear -> question; cancel/leaving/competitor -> churn)
  * sentiment     : pleased/grateful -> 0; matter-of-fact -> 1; angry/disappointed -> 2
  * respond_today : true for churn, angry bug reports, or anything saying account is at risk;
                    false for casual questions/feature ideas
"""
import json, random

rng = random.Random(20260923)

Q = {
    "intent": {
        "type": "choice",
        "id": "msg.intent",
        "instructions": "What the sender primarily wants",
        "criteria": {
            "bug": "Reports something broken or misbehaving",
            "feature": "Requests a new capability or improvement",
            "question": "Asks how something works or whether it exists",
            "churn": "Signals leaving, cancelling, or moving to a competitor",
        },
    },
    "sentiment": {
        "type": "score",
        "id": "msg.sentiment",
        "instructions": "Emotional tone of the message",
        "criteria": [
            "Positive — pleased, grateful, friendly",
            "Neutral — matter-of-fact",
            "Negative — angry, disappointed, threatening to leave",
        ],
    },
    "respond_today": {
        "type": "noul",
        "id": "msg.respond_today",
        "instructions": "A human should reply within 24h (churn risk, angry, or blocked)",
    },
}

cases = []
def add(state, intent, sent, today):
    cases.append({"state": state, "questions": Q,
                  "expected": {"intent": intent, "sentiment": sent, "respond_today": today}})

PRODUCTS = ["the dashboard", "sync", "the mobile app", "exports", "notifications", "search"]

# ---------- bug (18) ----------
bug_t = [
    ("Hi — {p} hasn't worked for two days. Data just never loads, spinning forever. Tried Chrome and Safari. {tone}", 1),
    ("Bug report: {p} shows yesterday's numbers even after refresh. Our team relies on this daily. {tone}", 1),
    ("Something's off: {p} throws 'Unexpected error' whenever I click save. Losing work here. {tone}", 2),
    ("Your {p} double-posts comments when I hit enter quickly. Annoying but not blocking. {tone}", 1),
]
POS = ["Love the product otherwise!", "Thanks for building this.", ""]
NEU = ["", "Let me know what you need.", "Happy to share more details."]
NEG = ["This is the third time this month.", "Really disappointed.", "About ready to give up."]
for i in range(18):
    t, sent = rng.choice(bug_t)
    tone = [rng.choice(POS), rng.choice(NEU), rng.choice(NEG)][sent]
    today = sent == 2 or rng.random() < 0.4
    add(t.format(p=rng.choice(PRODUCTS), tone=tone), "bug", sent, today)

# ---------- feature (14) ----------
feat_t = [
    ("Would love an API endpoint to bulk-update tags — doing it one by one is painful. {tone}", 1),
    ("Feature idea: dark mode for {p}. Eyes would thank you. {tone}", 0),
    ("Any chance of a Slack integration? We live in Slack and would pay extra. {tone}", 1),
    ("Suggestion: let us export {p} history as PDF for audits. {tone}", 1),
]
for i in range(14):
    t, sent = rng.choice(feat_t)
    tone = [rng.choice(POS), rng.choice(NEU), rng.choice(NEG)][sent]
    add(t.format(p=rng.choice(PRODUCTS), tone=tone), "feature", sent, False)

# ---------- question (16) ----------
ques_t = [
    ("Quick question: does {p} support SSO via Okta? Couldn't find it in the docs.", 1),
    ("Is there a way to undo a bulk import? Made a mess yesterday.", 1),
    ("How do permissions work for guest users on {p}? Inviting a contractor soon.", 1),
    ("Do you have a changelog or release notes page? Just curious what's new.", 0),
    ("Confused: what's the difference between archive and delete on {p}?", 1),
]
for i in range(16):
    t, sent = rng.choice(ques_t)
    add(t.format(p=rng.choice(PRODUCTS)), "question", sent, False)

# ---------- churn (12) ----------
churn_t = [
    ("We've decided to move to Linear — please cancel our workspace effective end of month.", 1, True),
    ("Cancelling. Your pricing doubled and the product hasn't kept up. Disappointed.", 2, True),
    ("How do I export ALL our data before we close the account? Leaving next week.", 1, True),
    ("After the outage last week our CEO lost confidence — evaluating alternatives now.", 2, True),
]
for i in range(12):
    t, sent, today = rng.choice(churn_t)
    add(t, "churn", sent, today)

rng.shuffle(cases)
with open("bench/email_intent/cases.jsonl", "w") as f:
    for c in cases:
        f.write(json.dumps(c, ensure_ascii=False) + "\n")
from collections import Counter
print(f"wrote {len(cases)}")
print(Counter(c["expected"]["intent"] for c in cases))
print("sentiment:", Counter(c["expected"]["sentiment"] for c in cases),
      "| today:", sum(c["expected"]["respond_today"] for c in cases))
