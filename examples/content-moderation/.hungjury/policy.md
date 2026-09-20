# Moderation policy

## action — precedence order

1. **escalate_human** — anything potentially illegal or safety-critical:
   offers of drugs/weapons, threats with plausible intent ('I know where
   you live'), CSAM, self-harm encouragement. Never auto-resolve these.
2. **remove** — severe but not illegal: targeted slurs/harassment,
   hate speech, graphic content, 'kill yourself'.
3. **warn** — borderline: profanity at a product ('this update is crap'),
   NSFW-adjacent links without explicit content, spam-adjacent self-promo.
4. **allow** — everything else, including harsh but civil criticism
   ('this product is garbage', 'worst support ever').

Disagreement between severity and action is a red flag: severe content
should not be 'warn', benign content should not be 'remove'.

## severity

- 0 benign: civil, criticism, profanity not aimed at a person.
- 1 borderline: insults, suggestive content, spammy promo.
- 2 severe: threats, hate speech, illegal offers, safety risk.

## illegal_or_safety

`true` only when the content may be illegal or a safety risk —
controlled substances, weapons, credible threats, minor safety,
self-harm. Profanity or rudeness alone does NOT count.
