# Pinocchio fixtures (v2.19)

Minimal Pinocchio-shaped Rust programs used as the v2.19 audit eval set
for `qedgen probe --program <path>`. Each fixture pairs a hand-authored
program with a `expected_findings.json` golden file the CI diffs
against.

These are **synthesized** to match the maintained Pinocchio probe taxonomy in
`references/probes/pinocchio/`. They are NOT verbatim
vendor checkouts — the upstream `solana-program/token/pinocchio` (a.k.a.
p-token) and `solana-program/associated-token-account/pinocchio` (a.k.a.
p-ata) are the inspiration, but our fixtures are scoped down to a
single handler each so the probe can be exercised without a Pinocchio
toolchain on the eval host.

## Fixtures

| Fixture | Mirrors | Patterns exercised |
|---|---|---|
| `ptoken-transfer/` | p-token `processor/shared/transfer.rs` | `SetAmountArith`, `SetLamportsArith`, `BorrowUnchecked`, `// SAFETY:` claim chain |
| `ptoken-close-account/` | p-token `processor/close_account.rs` | `SetLamportsArith` (sweep), lifecycle state reset, mutable-borrow aliasing |
| `pata-create/` | p-ata `processor/create.rs` | `IndexedAccountAccess`, missing PDA derivation, cross-program-invoke obligation |

## Greenfield codegen example (not an audit fixture)

`vault-greenfield/` is a different kind of fixture: a `.qedspec` that
`qedgen codegen --target pinocchio` turns into a buildable crate. It has no
`expected_findings.json` — it is the source for the Pinocchio scaffold
compile gate (`codegen_smoke::vault_pinocchio_scaffold_compiles`). See its
own README.

## Shared harness

`_harness/` ships the reproducer primitives every Miri repro imports
from. The shared components are:

- `account.rs` — synthesizes Pinocchio `AccountInfo` from `Vec<u8>` for
  direct-call Miri tests
- `adversarial.rs` — input-negation primitives keyed to SAFETY-comment
  strategies (G1)
- `invariants.rs` — assertion primitives the Miri repros bracket
  handler calls with (G3)
- `state.rs` — `capture_global_state` / pre-post diffing

## Running

```bash
# Audit the headline fixture:
qedgen probe --program crates/qedgen/tests/fixtures/pinocchio-fixtures/ptoken-transfer | tee /tmp/findings.json

# Diff against the golden file:
diff <(jq -S '.findings[] | {category, severity, handler, category_tag}' /tmp/findings.json) \
     <(jq -S '.findings[] | {category, severity, handler, category_tag}' \
        crates/qedgen/tests/fixtures/pinocchio-fixtures/ptoken-transfer/expected_findings.json)
```

## Why synthesized rather than vendored

Synthesizing keeps the v2.19 ship self-contained:
1. No upstream-license attribution work needed (these are our shapes).
2. No supply-chain risk — no fetching p-token at a specific commit.
3. Trivial to adjust when site-enumerator regex tweaks land.

When real-world p-token / p-ata adoption signals come in, vendoring at
a pinned commit becomes a separate v2.19.x patch.
