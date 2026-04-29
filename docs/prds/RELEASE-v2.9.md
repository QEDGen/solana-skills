# QEDGen v2.9 — release notes draft

**Tag:** `v2.9.0` (pending)
**Branch:** `v2.9-anchor-first-class`
**Theme:** Anchor first-class — meet users where their code already lives.

v2.8 closed the spec-composition gap. v2.9 closes the **brownfield**
gap end-to-end: `qedgen` now reads existing Anchor programs,
scaffolds a `.qedspec` from real source, paints `#[qed]` attribute
seals on each handler, and breaks the build on any drift between
spec and code. Handler discovery survives the four forwarder shapes
production Anchor programs use — Inline, free-fn, type-associated,
accounts-method — plus a `--handler` override for custom dispatcher
patterns. The "yeah, but how do I run this on my actual program"
question that's been the #1 adoption blocker since v2.5 has a
one-line answer now.

## What's in

### G2 — brownfield `#[qed]` adapter

The headline. Five layers:

- **G2a `qedgen adapt --program <c>` (scaffold mode).** Parses
  `<c>/src/lib.rs`, finds the `#[program]` mod, walks each
  instruction to its actual handler body, and renders a parseable
  `.qedspec` skeleton. Carries forward handler names, typed
  arguments, the `Context<X>` accounts struct (as a comment), and a
  path breadcrumb to where the body lives.
- **G2a' `qedgen adapt --program <c> --spec <s>` (attribute mode).**
  Given a filled-in spec, emits one `#[qed(verified, spec = ...,
  handler = ..., hash = ..., spec_hash = ...)]` line per handler with
  the matching source path. Paste each above its handler `pub fn`;
  body or spec edits trip `compile_error!` until you re-run this
  command. Method-shape handlers seal end-to-end via the impl-arm
  fallback (see G2d).
- **G2b — handler-discovery via `#[program]`-mod body following.**
  `anchor_resolver` classifies each tail expression and walks the
  crate's `src/` to lock onto the actual handler body. Four shapes
  covered:
    * `Inline` — multi-stmt body in the program-mod fn.
    * `FreeFn` — `module::function(args)` (also accepts the two-stmt
      `<call>?; Ok(())` and the single-stmt `<call>?` shapes; see
      G7).
    * `TypeAssoc` — `Type::method(ctx, args)` with a PascalCase
      prefix.
    * `AccountsMethod` — `ctx.accounts.method(args)`.
  Custom dispatcher / closure / non-path-call patterns surface as
  `Unrecognized` with a clear note pointing at `--handler`.
- **G2c — error enum read from `#[error_code]`.** The scaffold path
  walks the crate's `src/` for a `#[error_code] pub enum X { ... }`
  declaration and seeds the spec's `type Error | …` block with real
  variant names. Falls back to `| InvalidArgument` placeholder when
  no enum is found. Attribute path matched by last segment, so
  `#[anchor_lang::error_code]` works too.
- **G2d — drift loop end-to-end, all three legs.**
    * **Body hash** for both free fns and impl methods. The macro
      tries `syn::ItemFn` first and falls back to `syn::ImplItemFn`
      via the new `FnLike` shim, so accounts-method
      (`ctx.accounts.process(...)`) and type-associated
      (`Type::method(ctx, args)`) handlers carry `#[qed]` directly on
      the impl method.
    * **Spec hash** via the existing balanced-brace scan in
      `spec_bind`. Editing any byte inside a `handler { ... }` block
      fires drift.
    * **Accounts struct hash** — optional triplet `accounts =
      "Type", accounts_file = "src/...", accounts_hash = "..."`.
      When present, the macro reads the file, finds `pub struct Type`,
      hashes its tokens, and compares. Adding/removing fields,
      changing types, or editing inner `#[account(...)]` constraints
      fires drift. The adapter auto-includes the triplet when it can
      find `Context<X>` in source — and in v2.9 the lookup honors
      qualified paths (see G7).

  All three legs verified by `examples/qed-drift-fixture/`, a
  workspace member that fails to compile if any of:
  `qedgen::spec_hash::body_hash_for_fn` ↔ `qedgen-macros::FnLike::content_hash`
  (free-fn arm),
  `qedgen::spec_hash::body_hash_for_impl_fn` ↔ `qedgen-macros::FnLike::content_hash`
  (impl arm),
  `qedgen::spec_hash::spec_hash_for_handler` ↔ `qedgen-macros::spec_bind::spec_hash_for_handler`,
  `qedgen::spec_hash::accounts_struct_hash` ↔ `qedgen-macros::spec_bind::accounts_struct_hash_in`
  diverge. The acceptance regression — *tweak a body / spec / struct,
  drift fires; revert, drift clears* — runs on every workspace
  `cargo test`.

### G3 — generic Anchor CPI codegen

- `try_emit_anchor_cpi` dispatches between SPL Token (special case,
  kept) and the new generic Anchor path. Generic path emits a real
  Anchor sighash (sha256("global:<handler>")[..8]), Borsh-serialized
  argument payload, and an `AccountMeta` vector built from the
  imported interface. The user's CPI handler ships with no `todo!()`.
- Sighash conversion goes through a from-scratch `to_snake_case` so
  the bytes match Anchor's at-rest hash exactly. Verified by
  self-recompute in the test (no hardcoded golden values).

### G4 — `qedgen check --anchor-project` cross-check

- New flag on `qedgen check`: validates the spec's handler set
  matches the program's `#[program]` mod instruction set.
- Two finding shapes — *spec handler not in program* (stale spec /
  rename) and *program instruction not in spec* (uncovered handler).
  Pure read; no codegen. Exits 1 on any disagreement.
- Pairs with `--frozen` for the full CI freeze gate.

### G5 — shallow transitive imports

- `import_resolver` rewritten as a worklist DFS instead of a
  single-level loop. Imported specs that themselves declare imports
  now resolve recursively.
- Cycle detection (clear chain message) and conflict detection (two
  deps resolving to the same `dep_key` from different sources fail
  loudly).

### G6 — docs + worked examples

- New `references/qedspec-anchor.md` covering the full G2/G3/G4
  surface end-to-end.
- `SKILL.md` brownfield section rewritten: `qedgen adapt --program`
  is now the primary path for Anchor programs; `qedgen spec --idl`
  demoted to "no-source fallback". New "the `#[qed]` drift loop"
  subsection covers attribute-paste + `--anchor-project` CI.
- `README.md` brownfield section updated to lead with `qedgen adapt`
  and document `--anchor-project` as a CI gate.
- Three new worked-example directories:
  - `examples/anchor-brownfield-demo/` — free-fn forwarders; has
    `before.qedspec` + `after.qedspec`. Snapshot-pinned.
  - `examples/anchor-marinade-style-demo/` — accounts-method
    forwarders. Snapshot-pinned. Scaffold-only.
  - `examples/anchor-squads-style-demo/` — type-associated
    forwarders. Snapshot-pinned. Scaffold-only.
- `examples/qed-drift-fixture/` — workspace member that exercises the
  full `#[qed]` drift loop end-to-end on every `cargo test`.

### G2e — drift override + effect coverage + nested accounts + whitespace tolerance

The four items originally slated for v2.10 land here, closing the
brownfield surface to "no known gaps":

- **Custom dispatcher override.** `qedgen adapt --handler <name>=<rust_path>`
  points the resolver at the actual implementation when a handler's
  forwarder can't be followed automatically. Repeatable; works for
  both scaffold mode and attribute mode. Custom dispatcher patterns
  (runtime lookup tables, function-pointer indirection, closure-call
  shapes) are the canonical use case.
- **Effect coverage lint.** `qedgen check --anchor-project` now also
  asserts every spec effect's field is mutated in the resolved Rust
  body. Heuristic (LHS-leaf match), not a semantic-equivalence
  proof, but catches "added a spec effect but forgot to wire it in
  code" cheaply. Findings flow through `--json`.
- **Nested accounts struct discovery.** `accounts_struct_hash`
  (qedgen + macros, both sides) descends into `pub mod foo { ... }`
  blocks instead of stopping at top-level items. `pub struct Buy`
  declared inside `pub mod accounts { ... }` resolves the same as a
  top-level declaration; hash bytes are identical (the mod wrapper
  isn't part of the hashed token stream).
- **Whitespace-tolerant spec hash.** Spec-hash computation runs the
  extracted `handler { ... }` block through a normalizer before
  hashing. Cosmetic edits (extra spaces, line comments, blank-line
  shuffling) no longer fire drift; semantic edits (identifier
  changes, operator swaps, added clauses) still do. String literal
  contents pass through verbatim. Both sides re-pin via the same
  algorithm; the drift fixture's pinned `spec_hash` values were
  regenerated for v2.9.

### G7 — review-driven hardening

Late branch sweep landed three reviewer findings as committed
fixes + regression fixtures:

- **Multi-stmt forwarder classification.** The classifier now
  accepts the two-statement `<call>?; Ok(())` shape and the
  single-statement `<call>?` try-tail as pure forwarder plumbing.
  Pre-fix, both were misclassified as `Inline`, sealing the
  wrapper bytes in `lib.rs` instead of the real handler in
  `instructions/<name>.rs`. Multi-stmt bodies with user logic
  (`require!`, `let`, `msg!`) stay `Inline` so user bytes flow
  into the body hash.
- **Qualified-path accounts-struct resolution.**
  `Context<crate::b::Shared>` now resolves against `src/b/...`
  even when an alphabetically-earlier `crate::a::Shared` exists
  in another module. Pre-fix, `extract_accounts_type` dropped
  the path qualifier and the file walk returned the first match
  by ident name, silently sealing the wrong type.
- **`--handler` override scope.** The override now wins for any
  classifier outcome (Inline / FreeFn / Method / Unrecognized),
  not just Unrecognized — giving the user a clean escape from
  any misclassification.

Regression fixtures land at
`examples/regressions/anchor-forwarder-multistmt/` and
`examples/regressions/anchor-accounts-collision/`.

### Framework target selection

`qedgen init` and `qedgen codegen` gain a `--target
<anchor|quasar|pinocchio>` flag, replacing the historical `--quasar`
toggle. The flag selects which Rust framework the generated program
crate uses:

- `--target anchor` — Anchor-compatible Rust:
  `anchor_lang::prelude::*`, `Context<X>`, `Result<()>`, `'info`
  lifetimes on `#[derive(Accounts)]` structs, auto-derived
  instruction discriminators. **`#[derive(Accounts)]` structs land
  in `lib.rs` at crate root** (Anchor's `#[program]` macro requires
  this); handler impl blocks live in `instructions/<name>.rs`.
- `--target quasar` — Quasar (Blueshift) Rust:
  `#![no_std]`, `use quasar_lang::prelude::*;`, `Ctx<X>`,
  `Result<(), ProgramError>`, bare types (no lifetime params),
  explicit `#[instruction(discriminator = N)]` on each handler,
  `#[account(discriminator = N, set_inner)]` on each state struct,
  `Program<System>` for system program. Matches the conventions in
  `~/code/blueshift/quasar/examples/` (escrow, multisig).
- `--target pinocchio` — Pinocchio (no_std) Rust. CLI surface
  reserved; codegen branch ships in v2.10+. Selecting today errors
  cleanly with a v2.10+ pointer.

Omit `--target` to skip program scaffolding entirely (just the
`formal_verification/` skeleton lands).

**First build is clean.** Generated handlers carry a fully-pinned
`#[qed(verified, spec = ..., handler = ..., hash = ..., spec_hash = ...)]`
attribute. The codegen pre-computes the body hash by parsing the
rendered scaffold via `syn`, walking the `TokenStream` with a
hand-rolled canonical-string emitter (single-space separator, fixed
group brackets, no dependency on `proc_macro2`'s spacing-sensitive
`to_string`), and splicing the value into the `hash` field. The same
canonicalizer ships in `qedgen-macros::FnLike::content_hash` so
codegen-time and compile-time agree byte-for-byte. The drift fixture
at `examples/qed-drift-fixture/` was rebaselined to the new hashes
and pins the agreement on every workspace `cargo build`.

### Breaking changes

- **`qedgen init --quasar` (historical) is gone.** Replaced by
  `--target anchor` or `--target quasar`. v2.9 is the first tagged
  release with this surface in shipped form, so no migration path is
  needed for tagged users; anyone tracking `main` should swap
  `--quasar` → `--target {anchor,quasar}` based on which framework
  they want.

## What's not in

Cut from v2.9 scope:

- **Framework auto-detection.** A cascaded `Anchor.toml` →
  `Cargo.toml` `[dependencies]` → `Xargo.toml` walk shipped briefly
  on the branch and was removed before tag — its only public surface
  was a `--framework <name>` CLI flag that was never wired up.
  Anchor is the only supported framework today; raw / native
  Solana program support lands in v2.10+ via the `pragma sbpf`
  path that already exists for assembly programs.

Carried forward to v3.0+:

- **Effect coverage precision.** The heuristic checks that *some*
  mutation targets each effect's field; it doesn't verify RHS
  expressions or operator agreement. A handler with
  `state.balance = 0;` "covers" a spec effect `balance += amount`.
  Tightening to operator-aware or RHS-aware comparison is a v3.0
  judgment call (depends on whether the precision pays).
- **Method-shape custom-dispatcher overrides.** `--handler`
  resolves to a free fn today. Pointing at an `impl Type::method`
  via the same flag isn't supported.
- **Bare-path accounts-struct disambiguation.** When a handler
  writes `Context<Shared>` (no qualifier) and two `pub struct
  Shared`s exist in different modules, the historical
  first-match-wins ordering still applies. Use-tree resolution
  through the program-mod fn's source file is a v3.0 item.

## Numbers

- 18 commits on `v2.9-anchor-first-class` (M1, M2, M3, M4.1–M4.4,
  M5, G2a-attr, G2c, G2b-fixtures, G6-docs, G2d-impl-method +
  accounts-hash, G2e roadmap-fold, G7 reviewer-findings + drift
  sweep).
- 418 tests pass (baseline v2.8 release: 354). +64 net.
- 24 tests in `qedgen-macros` (was 19; +`FnLike` parsing, +impl
  fallback, +impl-arm hash, +whitespace-tolerant spec hash).
- Workspace `cargo build` exercises the drift fixture across all
  three sealing legs (free-fn body, impl-method body, accounts
  struct).
- All 10 `examples/*/formal_verification/` lake-build clean against
  the existing toolchain pin.
- Zero new dependency surface. New modules use `syn` (already in-tree
  for proc macros), `tempfile` for fixtures, `anyhow`. Nothing else.
- `cargo fmt --check` clean. `cargo clippy --all-targets -- -D warnings`
  clean.

## End-to-end demo

```bash
# 1. Scaffold a starter spec from existing Anchor source.
$ qedgen adapt --program examples/anchor-brownfield-demo
  # → .qedspec to stdout

# 2. Edit TODOs to fill in the spec. (See after.qedspec for the
#    end state on this fixture.)

# 3. Paint #[qed] attributes on each handler.
$ qedgen adapt --program examples/anchor-brownfield-demo \
    --spec examples/anchor-brownfield-demo/after.qedspec
  # === handler: initialize ===
  # source: src/instructions/initialize.rs
  # #[qed(verified, spec = "after.qedspec", handler = "initialize", hash = "...", spec_hash = "...")]

# 4. Paste attributes above each handler. cargo build.
#    Edit a body. cargo build fails with the drift diff. Revert. Cleared.

# 5. Gate CI on spec staying in sync with the program.
$ qedgen check --spec examples/anchor-brownfield-demo/after.qedspec \
    --anchor-project examples/anchor-brownfield-demo
  # Anchor cross-check (...) — spec and program agree.
```

## Pre-tag gates

- [x] `cargo fmt --check` clean
- [x] `cargo clippy --all-targets -- -D warnings` clean
- [x] `cargo test` — 418/418 pass
- [x] `bash scripts/check-readme-drift.sh` clean
- [x] `lake build` in every `examples/*/formal_verification/` (10/10 clean)
- [x] Zero unintended `sorry` (v2.8 G3 stance-1 sites are the only
      documented sorries; none added in v2.9)
- [x] **Drift-loop regression:** `examples/qed-drift-fixture/` is a
      workspace member; `cargo build` exercises the full chain on
      every commit
- [x] **Cross-check regression:** `qedgen check --anchor-project` smoke-
      tested against `examples/anchor-brownfield-demo/`
- [x] **Existing examples unchanged:** `qedgen check --frozen --spec
      examples/rust/escrow-split/` exits 0
- [x] `crates/qedgen/Cargo.toml` bumped to `2.9.0`
- [x] **Doc/code drift sweep** (CLAUDE.md pre-release item #9): every
      shipped command + flag has a section in `references/cli.md`; no
      references to deleted symbols / files / flags; `RELEASE-v2.9.md`
      "What's in" matches shipped commits; naming policy clean across
      SKILL.md, references/, RELEASE notes, and `clap` help text;
      module `//!` docstrings reflect post-fix behavior on touched files.
- [ ] Tag + `gh release create v2.9.0`
