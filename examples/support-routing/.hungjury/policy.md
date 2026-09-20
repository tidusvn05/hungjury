# Routing policy

## queue — pick exactly one, by intent precedence

1. **legal** — lawsuits, subpoenas, compliance/data-breach reports,
   GDPR/CCPA requests, legal letters. Legal risk always wins.
2. **manager** — explicit escalation ask ("get me a manager",
   "complaint to leadership") or repeated-failure frustration.
3. **billing** — charges, refunds, invoices, subscription changes,
   payment method issues.
4. **sales** — pricing/plan questions from non-customers, demos,
   upgrade-evaluation inquiries ("is X a bug or a plan limit?" is sales).
5. **technical** — bugs, errors, feature help, integrations.

When two intents appear, the one earlier in this list wins.

## priority

- 0 normal: how-to questions, feature requests, no impact stated.
- 1 elevated: real business impact or polite deadline —
  'we're blocked', 'needed by Friday', paying-customer complaint.
- 2 critical: production outage, data loss/exposure, legal action,
  chargeback deadline, security incident.

## vip

`true` only on explicit enterprise signals: "Enterprise plan",
"annual contract", "2000 seats", a named account/CSM. "I'm a paying
customer" or generic anger does NOT count.
