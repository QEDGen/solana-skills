# Release v2.24.1 — v2.24 stability follow-up

Patch release. Closes four issues that surfaced in v2.24.0 the day
after tagging:

1. **Anchor scaffold smoke tests broken on the bundled examples.**
   v2.24.0 wired multi-variant ADT codegen on any spec with 2+ State
   variants, but the bundled examples (escrow, multisig, lending,
   the perp-dex example) hadn't migrated their effect LHS to `Variant.field`
   syntax. The wrapper-struct + inner-enum emission referenced
   variant-payload fields the legacy lib.rs / guards.rs paths still
   addressed flatly, so `cargo check` failed on every regen.
2. **Quasar codegen bleed.** Four emission sites (`set_inner`,
   lifecycle pre-check, auth-guard destructure, inner-enum import)
   were emitting v2.24 wrapper+enum syntax on Quasar specs too,
   even though Quasar's zero-copy `#[account]` is incompatible with
   enum payloads. Result: Quasar scaffolds didn't compile.
3. **`--target pinocchio` rejected verification-backend codegen.**
   The Pinocchio dispatcher bailed before reaching `--kani` /
   `--proptest` / `--lean` because the Rust scaffold path isn't
   ready yet — but the verification backends are target-agnostic
   and should ship.
4. **CI miss on `package.json` version bump.** v2.24.0 only bumped
   `crates/qedgen/Cargo.toml`; `install.sh` reads from there but
   `package.json` is what npm publishes. CI flagged the drift
   post-merge.

No `.qedspec` syntax changes. No new lints. No new CLI commands.

## What's in

### S1 — `WrongState` declaration gates the v2.24 multi-variant ADT codegen path (#53)

`is_multi_variant_adt_state` (codegen.rs:893) and
`is_multi_variant_adt_state` (lean_gen.rs:91) both now require
`WrongState` to be present in the spec's `type Error` declaration.
Same for `is_multi_variant_adt_with_field_in_variant` (check.rs:742),
the predicate that drives R25's `has_one` suppression and the
variant-auth-guard destructure.

Operationally, `WrongState` is the **opt-in signal for v2.24
wrapper-struct + inner-enum codegen**:

  - Declare it in `type Error` and your effect LHSes use
    `Variant.field` syntax → state.rs emits a wrapper struct around
    an inner enum, lib.rs / guards.rs use the new shape, Lean emits
    `inductive State` with pattern-match transitions.
  - Leave it out → codegen stays on the v2.23 flat-fields struct
    + parallel `Status` enum shape, and Lean stays on the
    `structure State` shape.

Single-variant ADTs, single-record states, and multi-account specs
are unaffected (they were never on the wrapper+enum path).

Why the gate exists: the bundled examples ship a mix of pre-v2.24
specs that haven't yet flipped their bare-field effect LHS to
`Variant.field`. Forcing the wrapper+enum path on them produced
references to `wrapper.inner.<variant>` fields from lib.rs PDA seed
expressions and `guards.rs` requires bodies that the legacy emitters
still addressed flatly. The gate lets unmigrated specs fall back to
the working pre-v2.24 codegen until their authors flip the syntax at
their own pace.

Bundled-example status:

  - escrow, multisig, lending, the perp-dex example: **legacy flat path**
    (no `WrongState` declared) — scaffolds compile and proptest
    runs as before.
  - Any new spec written against v2.24 SKILL/DSL guidance opts
    into wrapper+enum by adding `| WrongState` to `type Error`.

### S2 — Quasar target gates on wrapper+enum emission sites (#50)

Four emission sites were re-gated to bail on non-Anchor targets:

  - `lifecycle_check_line` (codegen.rs:1414) — already gated on
    `is_multi_variant_adt_state`; tightened with
    `matches!(target, Target::Anchor)` so Quasar specs keep the
    legacy `status` byte pre-check.
  - `emit_variant_auth_guard` (codegen.rs:2636) — now takes a
    `target: Target` parameter and bails on non-Anchor. Quasar
    keeps `has_one = X` on the flat-struct field.
  - `emit_cross_variant_promotion` and `emit_state_match`
    (codegen.rs:2370, :2515) — gated on the `has_wrong_state`
    predicate above plus Anchor target. Quasar specs emit the
    legacy per-effect translation against the flat struct.
  - `render_handler_scaffold` (codegen.rs:2860) — inner-enum
    import (`use crate::state::<Name>AccountInner;`) only emits
    on Anchor target.

`has_one` suppression in `account_attr` (check.rs:920) carries the
same Anchor-only target gate so Quasar specs keep the suppression
off and the flat-struct field lookup keeps resolving.

### S3 — `--target pinocchio` allows verification backends (#52)

Pre-v2.24.1, `qedgen codegen --target pinocchio` rejected every
invocation because the Rust scaffold path isn't implemented yet.
That blocked the target-agnostic verification backends (Kani,
proptest, Lean) which work the same regardless of which runtime
the spec eventually compiles against.

v2.24.1 dispatches:

  - `--target pinocchio` + any of `--kani`, `--proptest`, `--lean`,
    `--test`, `--integration`, `--ci`, `--crucible`, `--all` →
    backends run as normal, skipping the Rust scaffold step.
  - `--target pinocchio` alone (no backend flags, no `--all`) →
    still errors with a one-line bail explaining what to pass.

`references/cli.md` now documents the quirk in the codegen flag
table so agents reading the reference pick it up automatically.

### S4 — `package.json` version bumped + CI rustfmt fix (#49, #51)

`package.json` v2.24.0 was added separately so npm publishes track
crate versions; v2.24.1 keeps them in lock-step. A multi-line
`matches!` macro from S2 was collapsed to a single line to satisfy
the project's `cargo fmt --check` CI gate.

## Migration

No action required. Existing v2.24.0 users:

  - Already-migrated specs (declared `| WrongState` in `type Error`
    and flipped effect LHS to `Variant.field`) keep emitting the
    wrapper+enum / inductive Lean shape unchanged.
  - Unmigrated specs that were previously failing to compile on
    Anchor scaffolds will now compile against the legacy flat-fields
    shape. Migration to wrapper+enum is opt-in via WrongState
    declaration.

Quasar specs are unaffected — wrapper+enum was never the right
shape for zero-copy `#[account]`.

## Tested against

  - `cargo test --release --bin qedgen` — 804/804 pass
  - `cargo test --release --test codegen_smoke -- --ignored` —
    4/4 pass (escrow scaffold + escrow proptest + multisig
    scaffold + perp-dex scaffold)
  - `cargo fmt --check`, `cargo clippy --release -- -D warnings`,
    `bash scripts/check-readme-drift.sh` — all clean
  - `qedgen check --frozen --spec examples/rust/{escrow,multisig,lending}/` (plus the perp-dex example, since removed)
    — exit 0 across the bundled set
