# QEDGen v2.29 — DSL Close-Out + Unified Imports

**Date:** 2026-05-24
**Planning input:** internal v2.29 PRD (intentionally not shipped).

## Headline

**The long-standing v2.26-era DSL gaps are closed.** v2.29 absorbs
ten of the thirteen unresolved items from the v2.26.0 sweep into
the shipped DSL — negative literals, abstract binders, payload-pre →
payload-post variant promotion, lint refinement, the `#[qed(verified)]`
drift UX, and **unified imports**: a single `import` keyword that now
covers both interface-only CPI stubs (v2.8) AND full-spec data-shape
imports for cross-program account reads.

Quasar / Pinocchio data-shape imports, Lean abstract-binder /
imported-field quantification, and a few smaller Kani / proptest
surface gaps stay deferred to v2.29.1+.

## What's in

| Item | Status |
|---|---|
| Payload-pre → payload-post variant promotion (per-field preamble) | shipped (Slice C) |
| Negative integer literals in expressions | shipped (Slice A) |
| Negative integer literals in `const` bodies | shipped (Slice A) |
| Cross-program data-shape imports (resolver) | shipped (Slice F) |
| Imported-account local mirror (codegen) | shipped (Slice H) |
| Cross-namespace type refs (`acct : type Foreign.State`) | shipped (Slice G) |
| `abstract <name> : <Type>` handler clause | shipped (Slice A — Lean defer to v2.29.1) |
| `missing_cpi_for_token_context` lint suppression on init handlers | shipped (Slice D) |
| `<account>.pubkey` accessor discoverability | shipped (Slice E) |
| `#[qed(verified)]` post-codegen drift surfacing | shipped (Slice E) |

Three items stay deferred:

| Item | Reason |
|---|---|
| Multi-variant ADT seeds (proptest) | one-variant cap holds in v2.29; full coverage queued behind the broader proptest record-derives pass |
| Cross-program data-shape imports for Quasar / Pinocchio | Slice F/G/H emit Anchor-only lowering in v2.29 |
| Misc Lean / Kani surface gaps | each rolls into a v2.29.1 or v2.30 line item |

## Slice-by-slice changes

### Slice A — DSL gap-close

- **Negative integer literals** desugar to `Sub(0, N)` at the
  expression atom layer so `state.x.exp == -4` and `pool_balance := {
  exp := -6 }` parse without the inline `0 - 6` workaround.
- **`const_decl`** widened to accept `(maybe '-') integer`;
  `TopItem::Const` + `InstructionItem::Const` now carry `i128`, the
  `const_literals` HashMap widened to `i128` so negative consts
  round-trip correctly, and `infer_const_type` falls through to the
  smallest signed Rust type for negatives. Const-expression evaluation
  (`const N6 = 0 - 6`) still rejected — that's v2.30+.
- **`abstract <name> : <Type>` handler clause** — existentially-
  quantified value the handler can refer to in `requires` / `effect` /
  `ensures` without committing to how it was computed (price-oracle
  output, curve-solver result, precomputed Merkle proof element).
  Wired through the Rust scaffold (`todo!()` with a requires-summary
  prompt), every Kani harness (`kani::any()` + the existing assume
  chain), and the proptest rejection harness (boundary strategy
  parameter). **Lean wiring is v2.29.1+** — the transition function
  treats the binder as a let-bound undefined value until the
  existential-wrapping codegen lands.

### Slice C — payload-pre → payload-post variant promotion

- Lift the bail in `emit_cross_variant_promotion`; emit a `let (f1,
  f2) = match &self.<acct>.inner { Inner::Pre { f1, f2, .. } =>
  (f1.clone(), f2.clone()), _ => Err(WrongState) };` preamble for pre
  fields that the post initializer reads. `resolve_cross_variant_rhs`
  accepts bare pre-field names as in-scope locals.
- `state := .Variant { f := e, ... }` whole-state assignment desugars
  at the adapter into per-field variant-prefixed effects so the
  existing codegen path consumes it without a parallel pathway;
  `state := .UnitVariant` drops to zero effects (the
  `handler.post_status` field drives the wrapper assignment).
- `check_effect_targets` accepts bare `state` LHS on multi-variant
  ADT specs as a safety net for shapes the desugaring doesn't catch.

### Slice D — `missing_cpi_for_token_context` lint refinement

- Suppress on lifecycle-init handlers (Uninitialized / Empty → X)
  that carry a writable token-typed account — Anchor's
  `#[account(init, associated_token::…)]` handles the SPL CPI
  implicitly via the init macro, so no `transfers` block is needed.
- Regression tests cover both the suppression and the still-firing
  non-init case.

### Slice E — `#[qed(verified)]` drift UX + `.pubkey` discoverability

- New `drift::check_stamped_drift` walks the codegen output directory
  after generation, recomputes `spec_hash` / `accounts_hash` / body
  hash per stamped function, and emits a `cargo:warning=` line per
  stale stamp plus the exact `qedgen check --drift … --update-hashes`
  re-stamp command. Closes the loop on the proc-macro
  `compile_error!` users were hitting on the next `cargo build`.
- Pull a "Cross-program authority" callout up to the top of the
  handler-clauses table in `references/qedspec-dsl.md` with the
  snapshot pattern (`admin := admin.pubkey` then `requires
  state.admin == signer.pubkey`); add a `## Cross-program patterns`
  section to `SKILL.md` cross-linking the dsl-reference anchor.

### Slice B — record derives + abstract-binder defer + inner-enum accessors

- Record types referenced inside multi-variant ADT account fields
  emit the full Anchor derive set (`AnchorSerialize`,
  `AnchorDeserialize`, `Clone`, `Copy`, `Default`, `PartialEq`,
  `Eq`) so they round-trip through Borsh inside the wrapper struct.
- **Inner-enum accessors** — multi-variant ADT account types lower
  to `pub struct <Name> { inner: <Name>Inner }` + `enum <Name>Inner`
  + per-field accessor methods on `<Name>Inner` (`pub fn admin(&self)
  -> &Pubkey`). Field reads in `requires` / `ensures` route through
  the accessor instead of bare field-name dereferencing — the
  generated `(*ctx.acct.inner.admin())` shape works for both
  locally-declared and imported multi-variant state.
- **Abstract-binder guard defer** — the Slice A `abstract <name>`
  clause's `requires` constraints lower as `kani::assume(...)` /
  `prop_assume!(...)` deferred until *after* the binder is bound,
  not before (the binder is in-scope only inside the body, not the
  signature).

### Slices F + H — unified imports resolver + local mirror

- **Slice F (resolver).** `ParsedSpec` grows `imported_namespaces:
  BTreeMap<String, ImportedNamespace>` (keyed by the consumer-side
  local name — matches the v2.8 interface-merge convention). The
  `resolve_and_merge_imports` loop captures every imported source's
  `account_types` + `records` whenever non-empty; bundled CPI stubs
  (SPL / System / Metaplex) leave the map empty because their
  fixtures declare no `type` blocks — backwards-compatible by
  construction. A new "data-only import" path synthesizes a minimal
  empty `ParsedInterface` (program_id only) so imports whose source
  declares only `type` blocks (no `interface`, no handlers) still
  flow through the merge loop and populate `imported_namespaces`.
- **`qed.lock` gains `imported_account_type_names`** — a
  comma-joined, sorted list of every imported account type per
  namespace. The `--frozen` diff report includes the field so a
  renamed or removed imported type surfaces with a clear
  before/after line; the default `structurally_equal` derive
  compares it as part of the standard `Eq` impl, so adding a type
  to a foreign spec bails `--frozen` exactly the way changing
  `spec_hash` does.
- **Slice H (local mirror).** `generate_imported_mirror` emits one
  file per imported namespace under `src/imported/<ns>.rs` plus a
  `src/imported/mod.rs` re-exporter. `generate_lib` adds `pub mod
  imported;` to `lib.rs` whenever `imported_namespaces` is non-empty.
  Two account-type shapes mirrored: single-variant → plain
  `#[account] pub struct <Name> { … }` (with the lifecycle `status:
  u8` + `enum <Name>Status` when the type declares lifecycle states);
  multi-variant ADT → wrapper struct + inner enum + the Slice B
  accessor pattern. Plain record types referenced by the imported
  account types are emitted in the same file so the mirror is
  self-contained.

### Slice G — cross-namespace type refs end-to-end

- **Parser.** `AccountAttr::Type` now accepts a dotted form via an
  optional second `.<ident>` after the head: `type Foreign.State`
  parses into `AccountAttr::Type("Foreign.State")`. Bare types
  (`type token`, `type State`) keep the pre-v2.29 single-ident shape;
  the dotted-vs-bare dispatch lives in the chumsky_adapter.
- **Resolver + validation.** `ParsedHandlerAccount` grows
  `imported_namespace: Option<String>`. A new
  `validate_imported_account_refs` pass (after import resolution)
  walks every handler-account binding tagged with
  `imported_namespace`, verifies the namespace is populated in
  `imported_namespaces`, and verifies the type name exists in that
  namespace's `account_types`. Error messages list the
  known-namespaces / known-types set so the user can fix the typo
  without grepping the imported source.
- **Anchor codegen lowering.** `FrameworkSurface` gains
  `imported_account_type(ns, source_type, mutable)` which renders
  `Account<'info, crate::imported::<ns>::<source_type>>`.
  `render_account_field_type` checks for `imported_namespace.is_some()
  && account_type.is_some()` BEFORE falling through to the
  `unchecked_account_type` default, so any imported binding gets the
  typed `Account<'info, T>` wrapper instead of `AccountInfo<'info>`.
- **Field reads compile.** The `bind_state` pass routes
  `<acct>.<field>` references for imported-account-tagged accounts
  through either `(*ctx.<name>.inner.<field>())` for multi-variant
  ADT mirrors or `ctx.<name>.<field>` for flat-struct mirrors (Anchor
  `Account<'info, T>` auto-derefs the body).

### Slice I — DSL docs + migration writeup

- `references/qedspec-dsl.md` gains a new `## Cross-program patterns`
  section covering both the v2.8 callee-interface import path
  (cross-linked to `qedspec-imports.md`) AND the v2.29 full-spec
  data-shape import path: `import Foreign from "dep_key"`, account
  binding via `acct : type Foreign.State`, field reads via the local
  mirror, the Slice F `qed.lock` `imported_account_type_names`
  field, the Slice H mirror at `src/imported/<ns>.rs`, and the
  v2.29 limitations (Anchor-only lowering; Lean ∀-quantification of
  imported fields deferred to v2.29.1).
- `SKILL.md`'s existing `## Cross-program patterns` section now
  cross-links the new DSL-reference anchor for the full-spec import
  story.
- This release notes file (`docs/prds/RELEASE-v2.29.md`) replaces
  the in-flight commit-message running log.

## Known gaps deferred to v2.29.1+

- **Lean abstract-binder wiring** (Slice A tail). The
  existential-wrapping codegen for `abstract <name> : <Type>` is
  punted to v2.29.1; until then Lean theorems that mention the
  binder surface as an undefined identifier. Rust scaffold / Kani /
  proptest paths are complete.
- **Lean ∀-quantification of imported account fields** (Slice G
  tail). Same scope as the abstract-binder wiring above — the
  consumer's handler theorem statement does not quantify over the
  imported account's fields in v2.29. Spec.lean for a property that
  references `foreign_acct.admin` from Lean will fail to compile
  until v2.29.1 closes the wiring.
- **Multi-variant ADT seeds in proptest.** One-variant cap holds in
  v2.29 — proptest's `arb_<Name>` strategy picks the first
  variant; the broader record-derives pass that lifts the cap is
  queued behind the Slice B accessor work.
- **Pinocchio + Quasar imported-types backends.** Slice F/G/H emit
  Anchor-only lowering; Quasar falls back to the pre-v2.29 path
  (`UncheckedAccount`) and Pinocchio reserves an
  `ImportedOwnerCheck` audit site + bytemuck deserialization stub
  for the v2.24-line Pinocchio runtime work.
- **Kani / proptest symbolic init for imported fields.** The current
  paths don't emit `kani::any()` for imported account fields
  automatically; only reachable when a property explicitly quantifies
  over an imported field. Documented gap for v2.29.1+.

## Migration notes

- **No spec syntax breaks.** Every v2.28 spec continues to parse and
  codegen identically in v2.29; new constructs (`abstract <name>`,
  `import Foreign from "..."` against a `type`-bearing source, `acct
  : type Foreign.State`) are pure additions.
- **Phantom-state workaround → import-state pattern.** Pre-v2.29,
  specs that needed cross-program account field reads had to fake
  the foreign account as a local state field (`state.foreign_admin :
  Pubkey`) and hand-thread the value at every handler. The v2.29
  migration is: import the foreign program's spec by `dep_key`, drop
  the phantom field, bind the account via `acct : type
  Foreign.State`, and read fields directly (`requires
  foreign_acct.admin == signer.pubkey`). The local mirror at
  `src/imported/<ns>.rs` is regenerated on every codegen — do not
  hand-edit. See the cross-program walkthrough in
  `references/qedspec-dsl.md#importing-another-programs-spec` for
  the step-by-step.
- **`qed.lock` schema bump.** The `imported_account_type_names`
  field is additive — existing locks parse cleanly with the field
  defaulting to empty. Re-running `qedgen check` (without
  `--frozen`) refreshes the lock; CI users running with `--frozen`
  will see a one-time stale-lock diff per import and need to
  re-lock once.
- **`SKILL.md` cross-link.** The existing `## Cross-program
  patterns` section in `SKILL.md` now cross-references the new
  full-spec import flow in addition to the v2.8 callee-interface
  imports. No removed links; new anchor added.

## Pre-release checklist

This release notes file lands ahead of the actual version bump.
When cutting the tag, run the standard checklist from `CLAUDE.md`:

1. Version bump (`crates/qedgen/Cargo.toml` + `package.json` to
   `2.29.0`; `bash scripts/check-version-consistency.sh`).
2. `cargo fmt --check`.
3. `cargo clippy -- -D warnings`.
4. `cargo test`.
5. `bash scripts/check-readme-drift.sh`.
6. `bash scripts/check-lake-build.sh --strict`.
7. Zero unsanctioned `sorry` in bundled `examples/**/*.lean`.
8. `qedgen check --frozen` clean for every `examples/rust/*/qed.toml`.
9. Doc/code drift sweep (this file, `references/qedspec-dsl.md`,
   `SKILL.md`, `CLAUDE.md` ↔ `claude.md` byte-identical).
