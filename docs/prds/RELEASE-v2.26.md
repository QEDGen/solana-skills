# Release v2.26.0 — close the verification loop on CPIs and the user's real impl

Minor release. v2.25 made the spec-author flow real for the LP-shape
pattern: declare the contract (`modifies` + `ensures`), name the
reference math (`ref_impl`), let codegen show the agent where to fill
the Rust impl. The verification *check* half of that loop landed in
spec-self-consistency form only — Kani verified the spec's effect
block against the spec's own ensures, not that the user's Rust impl
satisfies it; CPI boundaries stayed opaque past the v2.8 G3
`by sorry`. v2.26 closes both gaps.

Five threads compose:

1. **Multi-variant ADT modifies-fill** — the v2.25 `modifies` →
   `todo!()` agent-fill pattern now works on ADT-state specs
   (escrow, multisig, lending all participate).
2. **`ref_impl` composition** — ref_impls can call other ref_impls,
   take `Map[N] T` parameters, and be called from `requires` bodies.
   Recursive ref_impls are rejected at parse time with a clear error.
3. **First-class interfaces (Lean side)** — Tier-1/2 CPI calls
   (interfaces that declare `ensures` + an `upstream { binary_hash }`
   pin) now apply `Token.transfer.ensures_axiom_N` in the caller's
   theorem instead of carrying `by sorry`. A bundled SPL Token +
   System Program stdlib resolves via `import X from "spl"` /
   `import Y from "system"` with no `qed.toml` entry needed.
4. **First-class interfaces (Kani side)** — both the v2.25
   ensures-preservation harness and the new impl-targeted harness
   emit `kani::assume(<callee_ensures, substituted>)` after every
   CPI call site whose callee declares ensures. Tier-0 callees emit
   nothing — same fallback path the new `cpi_no_callee_ensures` lint
   surfaces.
5. **Impl-targeted Kani harness** (`--kani-impl`, opt-in) — calls
   the user's *real* Anchor handler against a symbolic `Accounts`
   context built with `kani::any()` for account-data fields and
   spec-declared PDA seeds for derived addresses. Auto-triggers
   when any handler has `modifies ⊋ effect.lhs` (the LP-shape
   signal) or any ref_impl carries potentially-overflowing
   arithmetic over bounded-numeric params.

Plus a real `--check-upstream` gate with differentiated severity (CRIT
in `verify`, P2 in `check --frozen`, escalation via `--strict`,
suppression via `--upstream-stale-ok`) and a record-form
state-account synthesis fix that closes a sharp edge for specs using
`type State = { ... }` without an explicit `accounts { ... }` clause.

## What's in

### Slice 2 — multi-variant ADT modifies-fill

`emit_variant_state_handler_body` learns the v2.25 Phase A pattern.
The variant destructure binds both effect-touched and `modifies`-only
fields; per-field `*X = todo!(...)` agent-fill sites with quoted
`ensures` comments emit inside the match arm:

```rust
match &mut self.<acct>.inner {
    <Inner>::<post> { pool_balance, lp_supply, .. } => {
        *pool_balance = pool_balance.checked_add(amount)
            .ok_or(Error::MathOverflow)?;
        // QED agent-fill site: `lp_supply` is in `modifies` but
        //   not in `effect`. Implement against the spec's ensures…
        *lp_supply = todo!("compute lp_supply to satisfy ensures above");
    }
    _ => return Err(Error::WrongState.into()),
}
```

The `unconstrained_modifies` lint (P0, v2.25) fires correctly on ADT
specs without code changes — it's field-name-based and the codegen
emitter was the only thing that needed teaching.

### Slice 3 — `ref_impl` composition (3a / 3b / 3c)

Three sub-features:

- **3a** — ref_impls can call other ref_impls. Recursive ref_impls
  (direct or mutual) are rejected at parse time with a DFS-cycle
  detector and a clear error pointing at structural decomposition.
- **3b** — `Map[N] T` parameters lower to `[T; N]` in Rust and `Map N T`
  (≡ `Fin N → T`) in Lean. The Lean body rewrites `m[i]` to `(m i)`
  before emission so the indexing expression composes with the
  function-typed Map.
- **3c** — ref_impls are callable from `requires` bodies. The
  generated `programs/src/ref_impls.rs` module hosts each declaration
  as a `pub fn`; `lib.rs` declares `pub mod ref_impls;`, and
  `guards.rs` brings it into scope via `use crate::ref_impls::*;` so
  the call resolves without any per-context plumbing.

### Slice 4a — first-class interfaces (Lean)

`render_cpi_theorems` rewrites the per-call-site theorem body for
Tier-1/2 callees (interfaces with `ensures` clauses + an
`upstream { binary_hash = ... }` pin):

```lean
/-- Token.transfer.ensures @ `deposit` call #0 (stance 1:
    discharged via Tier-1 binary-hash axiom; v3.0 will replace
    the axiom with an imported callee proof). -/
theorem deposit_Token_transfer_call_0_post_0 (s : State) (amount : Nat) :
    amount > 0 :=
  Token.transfer.ensures_axiom_0 amount
```

A sibling `<Interface>.lean` module is emitted per imported interface,
carrying `def binary_hash : String := "sha256:..."` and one
`axiom ensures_axiom_<idx>` per declared `ensures` clause. The
generated lakefile is rewritten idempotently to include the new
module in its `roots`.

Tier-0 callees (no `ensures` declared) keep the `by sorry` shape and
fire a new P1 lint `cpi_no_callee_ensures` pointing the user at the
upstream contract gap.

**Bundled SPL / system / Metaplex stdlib.**
`crates/qedgen/data/interfaces/spl_token.qedspec`,
`crates/qedgen/data/interfaces/system.qedspec`, and
`crates/qedgen/data/interfaces/metaplex.qedspec` ship as Tier-1
fixtures with `binary_hash` pins. The resolver short-circuits the
`spl`, `system`, and `metaplex` keys before manifest lookup; specs
that use only builtins need no `qed.toml` entry. The Metaplex fixture
covers the canonical Token Metadata CPI surface — NFT-mint metadata
creation (`create_metadata_account_v3`), metadata updates
(`update_metadata_account_v2`), creator verification
(`sign_metadata`), and collection membership (`verify_collection`,
`set_and_verify_collection`). User-authored interfaces continue to
work unchanged.

### Slice 4b — first-class interfaces (Kani, both harness shapes)

A shared substitution helper at `crates/qedgen/src/cpi_substitute.rs`
(extracted from Track F's private helper in `lean_gen.rs`) maps each
callee param to the caller's call-site expression. For every
`call Foo.bar(args)` site whose callee declares `ensures`, both the
v2.25 ensures-preservation harness AND the new impl-targeted harness
emit `kani::assume(<substituted_ensures>)` at the splice point AFTER
the handler call and BEFORE the caller's `assert!`:

```rust
let result = accounts.handler(amount);
if result.is_ok() {
    let post_lp_supply = accounts.state.lp_supply;
    let post_pool_balance = accounts.state.pool_balance;
    // CPI ensures-as-fact (Token.transfer):
    kani::assume(amount > 0);
    assert!(
        post_lp_supply == pre_lp_supply
                       + lp_out(pre_lp_supply, pre_pool_balance, amount),
        "ensures clause 0 on deposit (impl) violated"
    );
}
```

For `let X = call Foo.bar(...)` bindings, the bound name `X`
participates in the substituted ensures via the callee's declared
return-value binder (see "`result` keyword" below).

Tier-0 callees emit nothing — same fallback as the Lean side. The
existing `cpi_no_callee_ensures` lint surfaces the gap.

**Multi-CPI ordering** is a known limitation: when a handler makes
≥2 CPI calls whose substituted ensures reference the same caller-
state field, both `kani::assume`s fire at the same splice point
against one shared `(pre_X, post_X)` snapshot pair and can
over-constrain. A new P2 lint `multi_cpi_same_field` detects the
shape and the generated harness emits a `// WARNING: multi-CPI
ordering` breadcrumb. Per-call snapshot frames (the structural fix)
are v3.0-class — they require breaking the impl-targeted handler
call into segments.

### Slice 4c — `--check-upstream` as a real gate

Pin mismatch routing:

- `qedgen verify --check-upstream` → CRIT-severity `Finding` in the
  standard verification output, non-zero exit
- `qedgen check --frozen` → P2 warning by default, zero exit
- `qedgen check --frozen --strict` → escalates frozen to CRIT
- `qedgen verify --check-upstream --upstream-stale-ok` → demotes to
  Info, zero exit (for offline development)

Auto-on when `qed.lock` declares any pinned hash; no-op otherwise.
Network/CLI errors (missing `solana` CLI, unreachable RPC) stay P2
under any gate — a missing toolchain does NOT false-positive CRIT.

### Slice 1 — impl-targeted Kani harness (`--kani-impl`, opt-in)

New module `crates/qedgen/src/kani_impl.rs` emits
`programs/tests/kani_impl.rs` with one harness per handler that
carries `ensures` clauses. The harness builds a symbolic `Accounts`
context via a `mod symbolic_accounts { build_<Handler>() }` block,
calls the user's *real* Anchor handler, snapshots pre/post
account-field values, and asserts ensures clauses against the
flat `pre_<field>` / `post_<field>` locals.

PDA-derived account addresses bind via the spec's declared
`pda <name> [seeds]` (re-uses the same derivation helpers
`integration_test.rs` uses). Other account fields are `kani::any()`.

Two trigger conditions:

1. User passes `--kani-impl` explicitly to `qedgen codegen`
2. Auto-trigger when **either**:
   - Any handler has `modifies ⊋ effect.lhs` (the v2.25 LP-shape
     signal — the impl is expected to fill the gap)
   - Any `ref_impl` carries potentially-overflowing arithmetic
     (`*`, `<<`, `+`, `-`) over bounded-numeric params (the new
     `ref_impl_unbounded_arith` lint shape)

When auto-triggered, the file header carries a comment naming the
triggering handlers / ref_impls.

Anchor target only in v2.26. Pinocchio + native are deferred to v2.27.

### `ref_impl_unbounded_arith` P2 lint

Lean lowers `U64`/`I64`/etc. params to unbounded `Nat`/`Int` so
algebraic proofs go through `omega`/`ring` cleanly. The Rust side
keeps the bounded width; the same expression can wrap (release) or
panic (debug). Kani BMC catches this — but only if the user runs
Kani. The new lint fires on ref_impl bodies with `*`, `<<`, `+`, or
`-` over bounded-numeric params/return and points at the new
`--kani-impl` auto-trigger so the bounded-arith verification surface
runs even on flag-less invocations.

### State-account synthesis for record-form specs

A pre-existing sharp edge: specs using `state { ... }` sugar or
`type State = { ... }` without an explicit `accounts { ... }` clause
generated guards.rs with raw `s.X` references that didn't compile
(no Anchor account field carried the state). `chumsky_adapter::adapt()`
now synthesizes a default `state` handler-account when the conditions
match (record-form state, no explicit accounts, handler touches state
via effects or `s.X` references in clause bodies). ADT-state specs
unchanged.

### `result` keyword on interface handler return types

Optional `-> <ident> : <Type>` syntax names the binder used in the
callee's ensures, so the substitution helper maps it correctly when
the caller writes `let X = call Foo.bar(args)`:

```
interface Pool {
  handler absorb (amount : U64) -> burned : U64 {
    requires amount > 0
    ensures  burned <= amount
  }
}

handler liquidate (loss : U64) {
  let actual = call Pool.absorb(amount = loss)
  ensures actual <= loss
}
```

Bare `-> Type` (no binder) and no-return forms both still parse;
the substitution helper defaults to the literal `result` when no
binder is declared. Full backwards-compat.

### `render_indexed_state` now emits caller CPI theorems

Pre-fix: the renderer dispatcher routed `is_indexed_spec` (records
present OR Map-typed fields) into `render_indexed_state`, which was
the only one of the four renderers that never called
`render_cpi_theorems`. Record-form `type State = { ... }` specs and
any spec with Map-typed fields silently skipped caller-theorem
emission while ADT-state specs (escrow-split) worked. Now all four
renderers emit caller theorems where appropriate.

Sibling fix: the records loop in `render_indexed_state` was
double-emitting `structure State where ...` (once from the records
loop, once from the dedicated state-struct emission). Filtered
`"State"` out of the records loop.

## Migration

No action required. Existing specs continue to parse and codegen
exactly as before. The new lints (`ref_impl_unbounded_arith`,
`multi_cpi_same_field`, `cpi_no_callee_ensures`) are P1/P2 and don't
fail builds.

Specs that consume the bundled stdlib (`import X from "spl"` /
`"system"`) now resolve those keys via the embedded fixtures with no
`qed.toml` entry. Existing path-based imports continue to work.

Specs that declared CPI calls into Tier-1/2 interfaces previously
carried `:= by sorry` in the caller's Lean theorem. Those theorems
now apply the bundled axiom (`exact <Iface>.<handler>.ensures_axiom_N ...`)
— no spec change required, but the discharge is now explicit. `lake
build` on existing specs picks up the new module imports
automatically via the idempotent lakefile rewrite.

Specs that use `state { ... }` sugar or `type State = { ... }` record
form without an explicit `accounts { ... }` clause now get a
synthesized `state` handler-account — the generated guards.rs and
lib.rs Accounts structs are populated correctly.

The bundled stdlib's `binary_hash` values are `sha256:000...`
placeholders. The Lean axiom chain is structurally complete but
cryptographically un-anchored until real pins land. Plan to set the
real pins before any production use of `qedgen verify --check-upstream`.

## Tested against

- `cargo test --release --bin qedgen` — 870/870 pass (60+ new tests
  across `cpi_substitute`, `import_resolver`, `kani`, `kani_impl`,
  `lean_gen`, `check`, `chumsky_parser`, `upstream_check`)
- `cargo test --release --test codegen_smoke -- --ignored` — 4/4
  scaffold + proptest smoke tests pass
- `cargo test --release --test upstream_check_e2e -- --ignored` —
  4/4 CLI-dispatch upstream-mismatch tests pass
- `cargo fmt --check`, `cargo clippy --release -- -D warnings` —
  clean
- `bash scripts/check-readme-drift.sh` — 19/19 CLI commands
  documented
- `bash scripts/check-lake-build.sh` — 10/10 bundled examples
  lake-build clean (escrow, escrow-split, lending, multisig,
  percolator on Anchor; counter, dropset, slippage, transfer, tree
  on sBPF)
- `qedgen check --regen-drift` — clean on all 6 bundled rust
  examples
- Hand-verified LP-deposit demo end-to-end: bundled SPL stdlib
  resolves, ref_impl lowers to both Lean `def` and Rust fn,
  ensures-preservation harness binds `(pre, post)` correctly,
  impl-targeted harness auto-triggers, caller theorem in
  `Spec.lean` applies `Token.transfer.ensures_axiom_0`
  (no `sorry`), `lake build` clean

## What's next

v2.27 candidates:

- **Runtime expansion** — Pinocchio + native + Quasar variants of
  the impl-targeted Kani harness. Each needs its own account-builder
  shape (Pinocchio's raw-pointer layout, native's bespoke
  scaffolding, Quasar's zero-copy / Pod-aware fields).
- **Real `binary_hash` pins** for bundled SPL Token + System Program +
  Metaplex Token Metadata stdlib (current `sha256:0000…` placeholders
  need canonical values against
  `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`,
  `11111111111111111111111111111111`, and
  `metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s`)
- **Richer Token / System / Metaplex ensures** — current bundled
  clauses are placeholder tautologies (or empty for Metaplex); real
  balance-preserving clauses (`post.from + amount == pre.from`,
  `post.to == pre.to + amount`), authority-preserving clauses for
  Metaplex metadata updates, and total-supply invariants for
  `mint_to`/`burn` all pending audit
- **Multi-CPI ordering** — per-call snapshot frames so handlers with
  ≥2 CPIs touching the same caller-state field don't over-constrain
  at the splice point. Likely v3.0-class given the structural
  redesign needed
- **Callee param-name collisions with caller locals** — defensive
  rename at substitution time to make the binder shadowing explicit
- **Tier-0 self-documenting breadcrumb** — generated harness emits a
  comment when skipping a Tier-0 callee's assume slot, mirroring the
  existing P1 lint surface
