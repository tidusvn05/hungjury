# PR review gate policy

Decide whether a human must look closely before merge.

## needs_review

- `true` — touches auth/crypto/payment paths, public API or schema
  changes, new dependencies, or logic you cannot verify by reading.
- `false` — docs, comments, test-only changes, mechanical renames,
  pure formatting.

## breaking

- `true` — public API signature/behaviour change, schema migration,
  removed config keys, protocol changes. Internal refactors don't count.

## risk

- `2` — money/auth/data-loss surface, or the diff can't be fully
  verified statically.
- `1` — logic change with test coverage.
- `0` — cosmetic or well-tested mechanical change.
