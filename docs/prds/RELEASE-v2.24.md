# Release v2.24.0 — Multi-variant ADT codegen + Day 2 feedback closeout

v2.24 carries two paired themes. **The marquee work (S5)** rewires
multi-variant ADT codegen on the Anchor target from the legacy
flat-struct + `status: u8` byte discriminator into a real wrapper-
struct + inner-enum shape, plus a structural rewrite of the Lean
codegen to emit `inductive State` with pattern-matching transitions
and per-variant preservation theorem obligations. The motivating
feedback from a real spec author on v2.22: "the codegen to lean is
suboptimal / the ADT issue makes the states mean not much / the
theorems pursuant to the spec we wrote are mostly vacuous." S5
addresses that complaint structurally — the variant now carries
real meaning, the theorems get real per-variant case obligations,
the `sorry`s that remain are visible obligations for agents to
discharge rather than tautologies that pass silently.

**Day 2 feedback closeout (20 gist items)** covers everything the
same v2.22 user surfaced in a structured issue list: lint accuracy
bugs (nested field reads, missing_effect, shape_only_cpi),
DSL ergonomics (preserved_by-except, current_epoch, schema blocks,
interface comma support), modeling blockers (call in match arms,
call return values, Map keyed by enum), and parser limitations
(Map index field-access, whole-record Map assignment). All 20
items resolved.

## What's in

### S5 — Multi-variant ADT codegen (Anchor wrapper + inductive Lean)

**Wrapper-struct + inner-enum emission.** Anchor 0.32.1's `#[account]`
macro requires `ItemStruct`; enums are rejected. v2.24 emits a
wrapper struct that carries the discriminator + `InitSpace` derive,
plus an inner enum that carries the actual variants:

```rust
#[account]
#[derive(InitSpace)]
pub struct EscrowAccount {
    pub inner: EscrowAccountInner,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Debug, PartialEq)]
pub enum EscrowAccountInner {
    Uninitialized,
    Open { initializer: Pubkey, amount: u64, … },
    Closed,
}
```

Gated by `is_multi_variant_adt_state(spec)`: single-record account
types, single-variant ADTs, multi-account specs, and non-Anchor
targets all stay on the legacy flat-struct path.

**Handler-body lowering.** Same-variant in-place mutation lowers
to a match destructure; cross-variant promotion lowers to a
variant pre-check + `set_inner`-style assignment. Both shapes
include the `WrongState` fallthrough arm. Cross-variant from
payload-pre to unit-post (the `cancel : Open -> Closed` case)
emits a bare-constructor flip; init-style unit-pre skips the
pre-check since `#[account(init, …)]` zeroes the account.

**`Variant.field` LHS syntax.** Spec authors now write
`Active.balance += amount` to disambiguate which variant's field
they mean. Pre-fix, the syntax parsed but the lint flagged
`Active` as an undeclared field. Three lints (P7
`undeclared_state_field_in_effect`, Rule 13 `write_without_read`,
Rule 4 `unused_field`) are now variant-prefix-aware; codegen
strips the prefix to the bare field at the destructure site.

**Auth via variant payload.** R25's `auth X` → `has_one = X`
lowering is suppressed when `X` lives in a variant payload (Anchor's
`has_one` macro can't reach `wrapper.inner.<variant>.X`).
A replacement destructure-and-compare auth guard fires in
`guards.rs` so the auth check still happens at runtime when the
spec declares `Unauthorized`.

**Lean ADT.** `lean_gen::render_single_account_adt` emits a real
`inductive State` with payload-bearing variants and per-variant
field accessors. Transitions pattern-match the pre-variant
explicitly. Properties become predicates that case-split on the
variant. Preservation theorems and aborts/overflow/liveness
obligations all sit on `sorry` for now — visible obligations
agents discharge per [[feedback_agent_fills_directly_no_fill_verb]].
Binary properties (bodies with `old(...)`) emit as `(s s' : State)
: Prop` instead of the previous `(s : State) : Prop` tautology.

**Harness alignment.** proptest / Kani / integration_test keep
their flat-State model (the harness layer reasons about spec
semantics, not on-chain representation). `Variant.field` LHS
strips to bare field at the emission boundary so generated test
function names stay legal Rust idents.

**Fingerprint awareness.** `compute_fingerprint` now hashes the
per-variant payload structure into `src/state.rs`'s section hash
so the on-disk emission gets regenerated when variants restructure.

### Day 2 feedback closeout (20 gist items)

| # | Item | Coverage |
|---|---|---|
| #1 | `schema name { requires … }` block | parse + `include` clause expansion |
| #2 | Variant state assignment lint | already fixed (S2b) |
| #3 | `preserved_by all except [...]` | AST + expansion |
| #4 | Map index `lsts[state.x]` field-access | parser tweak |
| #5 | Whole-record Map slot assign | parser-accepted; docs |
| #6 | `<acct>.pubkey` accessor docs | DSL ref sweep |
| #7 | property positional syntax docs | DSL ref sweep |
| #8 | Multi-variant ADT semantics | full S5 work above |
| #9 | `call` inside `match` arms | AST + per-arm synth handler |
| #10 | Per-effect error variant | already fixed (S1) |
| #11 | `let X = call …` return binding | full Anchor lowering w/ `get_return_data` |
| #12 | `missing_effect` recognizes call/transfers/modifies | lint refinement |
| #13 | Tier-2 implicit interface synthesis | adapter |
| #14 | Interface `accounts { }` commas | parser tweak |
| #15 | `shape_only_cpi` dropped for Tier-0 | lint refinement |
| #16 | Nested field reads tracked | lint accuracy (write_without_read + unused_field) |
| #17 | `unguarded_arithmetic` cumulative | already fixed (S2c) |
| #18 | `U64_MAX` etc. builtins | already fixed (S2d) |
| #19 | `current_epoch()` builtin | Rust / Lean / Kani / proptest lowering |
| #20 | `Map[EnumType] T` | sum_types routing + `[T; N_VARIANTS]` codegen |

#11 also fixes the pre-existing `pubkey!` macro path issue (Anchor
0.32 doesn't reexport it under `solana_program::pubkey!`); generated
CPIs now use `<Pubkey as FromStr>::from_str(…)` which works
across Anchor versions.

#16 fixes lints that hadn't tracked nested-path writes since the
beginning — `op.guard_str` was the only read-side source, but
modern specs leave it `None` (they use the typed `requires` clauses
in `op.requires`). That alone removed ~30 false positives on
perp-dex-scale specs.

## What's deferred

- **Lean proof bodies for ADT preservation theorems**: the
  statement shape is complete (real per-variant obligations), the
  bodies are `sorry`. Per
  [[feedback_agent_fills_directly_no_fill_verb]] this is the
  intended escalation order: codegen owns shape, agents own
  bodies.
- **Cross-variant promotion when post-variant has fields not derivable
  from spec data** (e.g. escrow's `Open.taker` at init time):
  bails to per-effect `todo!()` with a clear comment. v2.24.x
  candidate for richer codegen, or stays as a spec-design forcing
  function ("you have to say where this field comes from").
- **#20 indexing syntax** `proposals[.Owner]` — bound type-checks
  and storage layout emit, but the spec author currently has to
  index via U8 cast of the variant ordinal. v2.25 candidate.
- **#11 backend bindings for Lean / Kani / proptest**: the Anchor
  side closes the loop end-to-end; Lean treats call-bound names as
  free variables for now. v2.25 candidate.
- **Bundled-example migration**: only `escrow-split` migrated to
  the new syntax this release. The other bundled multi-variant
  ADT examples (multisig, lending, the perp-dex example) are
  all Quasar target — they take the legacy flat-struct fallback
  path and aren't affected by v2.24's Anchor-side changes.

## Verification gates

- `cargo fmt --check`: ✅
- `cargo clippy -- -D warnings`: ✅
- `cargo test`: 804 passing, 0 failed, 1 ignored (pre-existing)
- `bash scripts/check-readme-drift.sh`: ✅ 19/19 CLI commands documented
- `bash scripts/check-lake-build.sh`: ✅ 10/10 bundled examples build clean
- `qedgen check --frozen` on every spec with a `qed.lock`: ✅
- End-to-end smoke fixtures:
  - `/tmp/v224_sample/` — 3-variant MiniEscrow (init / mutate / cancel + unary + binary props): Anchor cargo check clean, Kani harness well-formed, Lean lake build clean.
  - `/tmp/v224_p11_test/` — `let X = call …` with `-> U64` return type: Anchor cargo check clean.
  - `/tmp/v224_p20_test/` — `Map[AddressField] ProposalSlot`: Anchor cargo check clean.

## Migration

Multi-variant ADT specs on the Anchor target gain new options.
The minimum migration to opt into the new emission:

1. Declare `WrongState | InvalidLifecycle | MathOverflow | Unauthorized` in `type Error` (or a subset depending on which features the spec uses).
2. Flip bare-field effect LHS to `Variant.field` syntax:
   ```diff
   - effect { balance += amount }
   + effect { Active.balance += amount }
   ```
3. Regen with `qedgen codegen --target anchor`.

Pre-v2.24 specs that don't migrate keep working — the legacy
flat-struct path stays as the default for specs that don't declare
`WrongState` (it's the gating signal).

`escrow-split` is the bundled walkthrough — see the v2.24 S5j
commit `4037378` for the diff.
