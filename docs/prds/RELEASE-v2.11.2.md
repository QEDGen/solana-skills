# QEDGen v2.11.2 — Harness loop closure + lint refinements

**Date:** 2026-04-29
**Type:** Patch release
**Predecessor:** v2.11.1 (Quasar IDL ratchet patch)

## Headline

The harness loop now closes end-to-end on every bundled example. v2.11 made
codegen produce compiling scaffolds on third-party Anchor programs; v2.11.2
finishes the same round-trip on QEDGen's own examples — agent-fill stage
is enforced, the codegen contract reaches `Program<Token>` for token
handlers, and the spec-completeness lint suite no longer fires false
positives that the spec author can't silence.

## What changed (user-facing)

### New: `handler_unfilled_todo` lint

`qedgen check --code <path>` now scans `#[qed(verified)]` handler bodies
for residual `todo!()` placeholders and emits one `CompletenessWarning`
per finding. The lint extracts the spec-declared events, transfers, and
CPIs to name what's missing at each site:

```
! [P2] [handler_unfilled_todo] handler `deposit` has an unfilled `todo!()`
  in src/instructions/deposit.rs — spec expects: emit `Deposited` event,
  token transfer `depositor_ta -> pool_vault`
```

Surfaces in both text output and JSON (when `--code` and `--json` are
both set, the lint findings merge into the standard JSON warnings array).
Fills the gap where `cargo check` type-checks `todo!()` (the `!` return
type), so a scaffolded program could ship with placeholder business
logic and no gate would catch it.

### Codegen fix: `Program<Token>` for token transfers (Quasar)

`program_type` for the Quasar target was unconditionally emitting
`Program<System>` for the `token_program` account regardless of
the spec's account annotations. That blocked every
`.transfer(&from, &to, &auth, amt).invoke()?` call (the `TokenCpi`
trait is implemented for `Program<Token>` only, not `Program<System>`).

The Anchor branch already discriminated on `name == "token_program"
|| account_type == Some("token")`. v2.11.2 shares the same check
across both targets. Bundled escrow + lending examples now compile
end-to-end with real token transfers.

### Lint refinements (eliminate false positives)

**`preserved_by_all_potential_violation` respects `requires` clauses.**
The boundary `build_counterexample` picks (e.g., `lhs=3, rhs=3` for
`≤`) is often unreachable in practice because of guards the local
effect-analyzer doesn't model — dedup bitmaps, lifecycle gates,
signer-bound bounds. The lint now skips when any `requires` clause
references a property field. Missing requires still warns (real bug
shape).

**`missing_effect` skips legitimately-effect-less handlers.** Three
new exclusions:

- handlers with `ensures` clauses (frame-condition declarations,
  e.g. `ensures state.V == old(state.V)`)
- synthetic match-arm handlers (`<parent>_case_<N>`, `<parent>_otherwise`
  — the codegen convention for expanding `match` blocks)
- top-level abort handlers (`!aborts_if.is_empty() || aborts_total`)

**`unchecked_quantifier` distinguishes state-forall from binder-forall.**
`forall s : <StateType>` (advice: drop the redundant wrapper, use
`state.<field>` directly) vs `forall i : <BinderType>` (advice: narrow
to U8/I8). Two distinct shapes, two distinct fixes.

### Per-slot lowering for wide-binder forall properties

When a property is `forall <i> : <T>, body` and `<T>` is wider than
U8/I8 (e.g. `Fin[1024]`), `chumsky_adapter` now populates a new
`ParsedProperty::per_slot` field carrying the body rendered with `<i>`
as a free Rust variable. `proptest_gen` emits `fn {prop}_at(s, i)`
alongside the standard `{prop}` predicate, and preservation tests for
handlers taking `<i>` as a param call `{prop}_at(&s, i)` — checking
at the modified slot, which is sufficient for inductive preservation
since handlers only mutate `state.<arr>[i]` (frame condition handles
the rest).

Closes the percolator `account_solvent` warning that previously had
no clean fix; the property is now genuinely verified by proptest at
every handler step.

### `qedgen check --code` exit-code fix

`check_code_drift` was treating user-owned handler files as NoHash
drift because they don't carry a `spec-hash:` marker by design.
v2.11.2 splits the expected-files list into codegen-owned (subject
to spec-hash drift) and user-owned (existence-only check). Healthy
specs no longer exit 1 spuriously on the `--code` mode.

### Bundled examples: 15 handler bodies filled

Every bundled example now has zero `todo!()`, compiles cleanly, and
passes `qedgen check` with zero warnings:

- multisig (6 handlers): event emits with signer pubkey + vote-count
  payloads.
- lending (5 handlers): event emits + Quasar SPL token transfers,
  including pool-PDA-signed transfers via `invoke_signed` with
  `[b"pool", authority, &[bump]]` seeds.
- escrow (3 handlers): event emits + token transfers, including
  escrow-PDA-signed releases on exchange and cancel.
- percolator (1 handler): `close_account`'s `V -= accounts[i].capital`
  via `PodU128.into() → u128.checked_sub`.

Multisig handler files also picked up `let _ = bumps;` to match
current codegen output (eliminates 8 unused-variable warnings).

### Spec edit: lending `pool_solvency`

Rewrote from `forall s : Pool.Active, s.total_deposits >=
s.total_borrows` to `state.total_deposits >= state.total_borrows`.
The redundant outer state-quantifier blocked proptest exhaust without
changing semantics — properties are evaluated against the current
state implicitly.

## What didn't change

- DSL surface, parser, and `.qedspec` file format are unchanged.
- Lean codegen behavior is unchanged (per-slot lowering only affects
  the proptest harness; Lean theorems still take the full forall).
- Anchor target codegen is unchanged.
- No CLI flag additions or removals.

## Final lint state on bundled examples

| Example | spec-only exit | warnings | handler_unfilled_todo |
|---------|---------------|----------|-----------------------|
| escrow | 0 | 0 | 0 |
| lending | 0 | 0 | 0 |
| multisig | 0 | 0 | 0 |
| percolator | 0 | 0 | 0 |

## Pre-release gates

- `cargo fmt --check` — clean
- `cargo clippy -- -D warnings` — clean
- `cargo test -p qedgen-solana-skills --bins` — 452/452 pass
- `cargo run -p qedgen-solana-skills -- check --regen-drift` — 5 examples in sync
- `bash scripts/check-readme-drift.sh` — clean
- `bash scripts/check-version-consistency.sh` — `2.11.2` consistent
- `cargo check` per bundled example — clean

## Carried forward

- Lean coverage status flowing into `UnifiedReport::issue_count`
  causes `qedgen check --code` to exit 1 even on a clean spec when
  the bundled `Spec.lean` has stub `:= trivial` theorems with names
  that don't match `generate_properties` output (`<handler>.cpi_correct`
  expected vs `<handler>_transfer_correct` rendered). v3.0 cleanup item.
- `bin/qedgen check --drift <path> --update-hashes` writes hashes
  the proc macro then rejects, because `crates/qedgen/src/drift.rs`'s
  `content_hash` uses `to_string()` while the macro uses
  `canonical_token_string`. Workaround: read `Actual:` from
  `cargo check --message-format=short` output. v3.0 cleanup item.
