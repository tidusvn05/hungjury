# Mail classification policy

## verdict

- **phishing** = asks for credentials/payment details (password, card,
  OTP, "verify your account") OR spoofs a known brand with a link to an
  unrelated domain OR password-protected archive attachment.
- **scam** = advance-fee/prize/lottery/investment-fraud: money promised
  if you pay or reply first. Not a credential ask — that is phishing.
- **promo** = legitimate bulk marketing: unsubscribe link, company
  branding, no deception. Automated digests/notifications are promo.
- **ham** = personal or transactional mail a recipient plausibly wants:
  invoices, order confirmations, replies, direct human messages.

Order matters: check phishing/scam markers BEFORE promo/ham. Urgency +
generic greeting + link is phishing even when it imitates a real brand.

## credential_risk

`true` only when the mail asks the recipient to enter or send
credentials, card numbers, OTP, or ID documents. Asking for a wire/fee
is scam — not credential risk.

## spam_score

- 0: clearly wanted — transactional or human-written, no pressure.
- 1: bulk or mildly suspicious — marketing, automated digests, vague
  business outreach.
- 2: clearly unwanted or dangerous — phishing, scam, threats,
  deceptive links.
