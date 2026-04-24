# qedgen v2.7.0 test-fixture bundle — issue #8

Accompanies https://github.com/QEDGen/solana-skills/issues/8. Contributed
by @lmvdz as a gist; copied verbatim into the repo so v2.7.1 fixes can
drive against committed regressions. Original gist:
https://gist.github.com/lmvdz/639804a0585317cb56cb14d2620e0ade

## Contents

| File | Purpose |
|---|---|
| `pool.qedspec` | Anonymised failing spec — 13 handlers, 29 error variants, folded-state model over 4 Anchor account types. Triggers findings #1–#8 in combination (feature-interaction surface). |
| `repro-01-u16-type.qedspec` | Finding #1 isolated — Lean `map_type` missing U16/U32/I8..I64 |
| `repro-02-composite-or-parens.qedspec` | Finding #2 isolated — composite guards mis-parenthesize `∨`/`→` |
| `repro-03-duplicate-theorem.qedspec` | Finding #3 isolated — duplicate `aborts_if_E` theorem when multiple requires share an error |
| `repro-04-liveness-params.qedspec` | Finding #4 isolated — liveness witness drops handler parameters |
| `repro-05-uninterpreted-helper.qedspec` | Finding #5 isolated — uninterpreted helpers in requires/ensures never declared |
| `repro-06-cover-witness-bool.qedspec` | Finding #6 isolated — cover-witness uses `"0"` for Bool and non-Nat types |
| `repro-07-pubkey-literal-assign.qedspec` | Finding #7 isolated — `qedgen check` accepts `Pubkey := <int>` silently |
| `repro-08-pubkey-literal-compare.qedspec` | Finding #8 isolated — `qedgen check` accepts `state.<Pubkey> != <int>` silently |

Each minimal repro has an inline header comment linking it to the
finding number, the expected-vs-actual behavior, and the fix site in
`crates/qedgen/src/`.

## Verification protocol

```bash
# All 9 files pass `qedgen check` (the bugs are downstream of check):
for f in examples/regressions/issue-8/*.qedspec; do
  echo "=== $f ==="
  ./bin/qedgen check --spec "$f" 2>&1 | tail -1
done

# Each minimal repro currently fails lake build in a distinct way on
# v2.7.0. To reproduce per-repro:
#   ./bin/qedgen init --name reproN --spec repro-NN-*.qedspec --output-dir /tmp/reproN
#   ./bin/qedgen codegen --spec repro-NN-*.qedspec --lean --lean-output /tmp/reproN/Spec.lean
#   cd /tmp/reproN && lake build
```

Note on `pool.qedspec`: this is the "worked-around" version — each
known workaround (U8 flags instead of Bool, `side <= 1` instead of
`side == 0 or side == 1`, no uninterpreted helpers in guards, no
Pubkey-literal assignments, no parameterized liveness) has been
applied so that `qedgen check` + `qedgen codegen --lean` +
`lake build` all pass cleanly. It's a **positive** regression: a
realistic 13-handler spec that *does* build on v2.7.0 after the
workarounds, and should continue to build after the fixes land. To
reproduce the original failing versions, undo any single workaround
(e.g. add a `Bool` state field, or a `requires ... or ...` guard)
and re-run.

## Working-spec provenance

The original spec was authored against a deployed Anchor program on
Solana — ~13 handlers for a pool protocol with encrypted-state
rotation, SPL vaults, a two-step withdraw flow, a running-hash
transparency log, and admin-gated graph PDA pinning. Every technical
element in `pool.qedspec` (the U8 flag discriminators, the single-admin
auth model, the `Operation` constructor shape, the 4-account folded
state) is structurally faithful to the underlying program. Project-
specific identifiers in comments (internal ADR numbers, concern IDs,
Gap IDs) have been replaced with descriptive phrases — the technical
content is unchanged.

## Environment (original report)

- qedgen v2.7.0 (release binary, sha256-verified)
- Lean `4.15.0` / Lake `5.0.0-1165156`
- Target: Anchor 0.32.1 Solana program
