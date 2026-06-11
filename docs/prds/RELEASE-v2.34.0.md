# QEDGen v2.34.0 — Finish the qedsvm extraction; adopt Lean stable

**Status:** shipped (on `release/v2.34.0`). **Theme:** complete the
qedsvm-from-qedgen split — `lean_solana` now *depends on* qedsvm's frozen
`SVM` surface instead of carrying a vendored fork of it — and move the whole
Lean toolchain onto `v4.30.0` stable. Plus a multi-framework Pinocchio
impl-proof layer and an auditor model/cleanup pass.

## What shipped (in dependency order)

### 1. Un-vendor the sBPF engine — `require qedsvm` (#86 / PR #89)

qedsvm was extracted from qedgen, but the extraction was partial: qedgen kept
a vendored copy of qedsvm's sBPF core under `lean_solana/QEDGen/Solana/SBPF/`
(the `ISA.lean` headers were byte-identical), pinned "until qedsvm tags
stable." This release deletes that subtree and adds a lake dependency:

- `lean_solana/lakefile.lean` does `require qedsvm from git @ "v0.4.0"`; the
  vendored `SBPF/` tree (8 files) is gone. A per-declaration drift audit
  confirmed qedsvm is a strict superset — the only vendored-only declarations
  (four r10 frame lemmas) had no consumers and were dropped.
- One `Pubkey` across the spec/binary boundary:
  `QEDGen.Solana.Account.Pubkey := SVM.Pubkey` (the two were byte-identical
  twin structs; `ext'` / `ne_iff` now come from qedsvm).
- The `qedguards` / `qedbridge` DSLs stay qedgen-owned codegen; the generated
  obligations now import qedsvm's tactics (`wp_exec`) and thread qedsvm's
  `RegionTable` — every guard obligation binds `rt` and carries one
  `rt.containsRange <addr> <width> = true` coverage hypothesis per derived
  read (qedsvm's `Patterns` idiom; with a symbolic base address the region
  check only closes by hypothesis rewrite). `lean_gen_mir` and `asm2lean`
  emit the new shape; `asm2lean` also regained a flat `progAt` for
  ≤64-instruction programs.

The boundary is now clean: qedgen owns the `.qedspec` DSL + abstract-domain
vocabulary; qedsvm owns the sBPF semantics + both binary-proof engines
(SL/lift and WP/fuel); one dependency arrow, qedgen → qedsvm.

### 2. qedsvm v0.4.0 / Lean v4.30.0 stable (PR #90)

qedsvm v0.4.0 shipped the WP-track fixes the un-vendoring surfaced and bumped
itself to Lean stable; qedgen follows:

- Toolchain `v4.30.0-rc2` → `v4.30.0` stable across `lean_solana`,
  `lean_solana_mathlib`, every bundled example, the embedded `project.rs`
  templates, and the pinned Mathlib tag.
- `Width.bytes` now reduces inside `wp_exec`/`wp_step` upstream, so the
  per-call-site workaround is removed from the migrated sBPF example proofs —
  they close with no extras. The strict lake gate confirms this end-to-end
  through the dependency, validating the upstream fix.
- qedsvm v0.4.0 also exposes `region_covers` / `wp_exec_from` tactics and
  conditional r10 frame lemmas (available for future sBPF proof work).

### 3. Generic Pinocchio impl-proof profiles (PR #80)

A parser-backed proof-profile layer for `--kani-impl` consumes ABI-derived
dispatcher tags, account order/roles, instruction packing, PDA derivations,
token-account bindings, and account-layout facts — without selecting code by
program name. Includes two generated-harness correctness fixes:

- Guard-rejection proofs for handlers with more than eight guard terms no
  longer split into `assume ¬t; assert ¬t` tautologies; each arm now assumes
  the prefix terms and the i-th violated, then asserts the handler actually
  rejects (the split arms partition the violation domain exactly).
- "Unchanged-field" ensures classification is anchored, so a
  `post.X == pre.X + delta` conservation ensures no longer matches as a
  substring and emits a spurious equality assertion.
- Each guard-rejection harness emits a satisfiability cover, so an arm whose
  term is implied by its prefix fails loudly instead of passing vacuously.

### 4. Auditor: default to Opus 4.8; scrub named protocols/programs (PR #91)

- The auditor's recommended-model default is now **Claude Opus 4.8** with
  extended thinking (GPT-5.5-high remains the alt-harness path).
- The auditor's loaded surface (`SKILL.md` + `references/`) and the security
  primer no longer name any specific protocol, program, audit target, or
  audit firm. Every corpus reference now describes the *vulnerability class*
  + rough year/loss rather than the incident. Crypto-primitive names
  (Winternitz/WOTS/Lamport/Pedersen/Merkle) and integration-framework names
  (Anchor/Quasar/Pinocchio) are retained — they are schemes and integration
  targets, not finding sources.

### 5. The sBPF order-book example is temporarily out

The bundled sBPF order-book example's proof corpus was proven against the
pre-qedsvm vendored semantics and does not replay under qedsvm's
region-table + call-stack `State`. Rather than ship it with retired-proof
stubs, the example is removed from `examples/sbpf/` for this release and will
return once re-proven under the qedsvm semantics (now tractable via v0.4.0's
`wp_exec_from` + conditional r10 lemmas). Its codegen fixture is retained
under `crates/qedgen/tests/fixtures/`, so snapshot coverage of sBPF codegen
is unaffected. The other four sBPF examples (counter, tree, transfer,
slippage) prove cleanly under the new engine.

## Compatibility notes

- First Lean build after upgrading re-fetches Mathlib for `v4.30.0` (the
  expensive step); the `lake-build` CI cache key rolls on the
  `lean-toolchain` + `lakefile` change, so expect one cold miss.
- A lake `require` couples the toolchains: a future Lean bump in `lean_solana`
  now waits on a compatible qedsvm tag. Both pin `v4.30.0` today.
- First Lean validation in a fresh `qedgen`-generated project now fetches
  qedsvm from git (pinned tag, pure Lean, no Mathlib) — the embedded support
  package imports `SVM.Pubkey`.

## Gates

`cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`,
`check-readme-drift.sh`, `check-lake-build.sh --strict` (10 examples),
`qedgen check --frozen`, and the `cargo audit` / `cargo deny` supply-chain
gates all pass. No new RustSec advisories (the five ignored IDs are
unchanged). Zero unintended `sorry` in examples.
