# hungjury (TypeScript/Node SDK)

Zero-dependency wrapper over the `hungjury` CLI binary — same API shape
as `sdk/python/hungjury.py`.

```js
const { decide, feedback } = require("./index.js");
// or, once packaged: require("hungjury")

const d = decide("The checkout crashes every other attempt. Ridiculous.", {
  severity: {
    type: "score",
    id: "support.severity",
    instructions: "How severe the issue is",
    criteria: [
      "Cosmetic — 'nit', 'fyi'; no user impact",
      "Bounded impact — 'annoying', 'sometimes fails'",
      "Blocking — 'broken', 'data loss', 'can't work'",
    ],
  },
}, {
  jurors: ["claude:haiku", "codex:gpt-5.6-terra@low"],
  judge: "claude:opus@high",
  escalate: "sync",
  cwd: "/path/to/project", // picks up .hungjury/ config, policy, memory
});

if (d.ok) {
  console.log(d.answers.severity); // { score: 2, confidence: ... }
} else if (d.exitCode === 2) {
  console.log("hung on:", d.hung); // route to a human
}

feedback(d.id, { severity: 2 }, { note: "confirmed by on-call" });
```

Notes:

- Latency is **seconds** (real CLI-agent calls) — batch/CI/triage use,
  not request-time code.
- `exitCode` mirrors the CLI: `0` decided, `1` error, `2` hung.
- Everything resolves exactly like the CLI does from `opts.cwd` —
  `.hungjury/` walk-up, profiles, namespace isolation.
