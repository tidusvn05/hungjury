# Inbox triage policy

## folder

- **work** = business collaboration sent to the recipient personally:
  colleagues, clients, meeting invites, review requests, project
  threads. A human-written forward from a colleague is `work` even when
  the forwarded content is a promotion — the message to you is personal.
- **personal** = human-written mail from friends, family, acquaintances.
- **promotions** = opt-in bulk marketing: sales, deals, newsletters,
  product announcements, webinar invites. Recognizable branding +
  unsubscribe link, no deception.
- **updates** = automated service notifications about your own accounts:
  order confirmations, shipping, receipts, calendar reminders, security
  alerts, statements.
- **spam** = unsolicited or deceptive mail: phishing, scams, fake
  warnings, mass outreach pretending to know you.

Order matters: check **spam** markers FIRST (deception, credential asks,
spoofed domains), then work/personal (human, direct), then updates
(automated, about you), then promotions (bulk, marketing).

## action_required

`true` only when the mail legitimately expects the recipient to DO
something: reply, RSVP, pay, review, reset a compromised password.
Requests inside spam/phishing are NOT actions — never surface a
malicious request as an action. Newsletters and receipts are FYI —
`false` even when they contain buttons.

## priority

- 0: can wait or archive — newsletters, receipts, FYI forwards.
- 1: read today — actionable, no imminent deadline.
- 2: act now — deadline within ~24h, blocking someone, or a security
  alert affecting the recipient.
