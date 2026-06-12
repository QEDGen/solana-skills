# QEDGen v2.35.0 — Stmt-native effect lowering across every backend

**Status:** shipped (on `release/v2.35.0`). **Theme:** close the #66 MIR
frontier — the typed `Stmt` body is now the single effect-lowering source for
all four codegens (Lean, Rust scaffold, Kani, proptest), conditional
`effect { match … }` bodies are Stmt-native end-to-end (Phase 5), and Branch
handlers get real per-arm Kani conformance coverage. Plus the #88 sBPF regen
fix, Quasar `System.transfer` mechanization, and a dead-code prune.

## What shipped (in dependency order)

### 1. Quasar `System.transfer` CPI + exhaustive `Stmt` matches (#66 / #83 tail, PR #93)

- `try_emit_cpi` routes (Quasar, System) to `emit_system_cpi_quasar`, so
  `call System.transfer(...)` is mechanized on all three Rust targets
  (Anchor / Quasar / Pinocchio).
- All 13 remaining `_ =>` arms over the closed `Stmt` enum were made
  exhaustive — the "new statement kind is a compile error at every consumer"
  discipline is now real at every match site.

### 2. Kani/proptest effect lowering ported onto `Stmt` (#66 frontier, PR #95)

Kani and proptest previously took the `Mir` on their signatures but lowered
effect bodies from `ParsedHandler.effects` (proptest even re-parsed the spec
from disk). A new `Stmt` variant was invisible to both. Now:

- `rust_codegen_util::stmt_effect_triple` projects each effect-shaped `Stmt`
  onto the (field, op_kind, value) triple the string templates consume —
  exhaustive match, no `_` arm. A new `Stmt` variant is a compile error for
  the Kani + proptest backends too.
- `kani_mir` transition loops, conformance-harness iteration, and the
  overflow filter read the MIR body; `proptest_gen_mir::generate` consumes
  the passed `(mir, parsed)` (the `spec_path` re-parse is gone).
- Round-trip tests pin the adaptor invariant (triples == `op.effects`, order
  and content) over an all-seven-op-kinds spec and every bundled example.
- Fixed the flipped `+=!` / `+=?` doc comments on `SatAdd` / `WrapAdd`
  (`+=!` = saturating, `+=?` = wrapping — matching the parser).

### 3. Phase-5 Branch lowering — conditional effects are Stmt-native (#42 / #66, PR #96)

`effect { match … }` previously lowered as the flat *union* of every arm's
effects; the Lean transition applied ALL arms unconditionally — a semantic
divergence between the Lean model and the Kani/proptest model for issue-42
specs. Now:

- `lower_body` builds a real `Stmt::Branch` (declaration-order arms, wildcard
  as `default`); the union and the stub Abort marker are gone.
- The Rust emitter renders the `match` from `Stmt::Branch`
  (output byte-identical to the previous `effect_branches` shape).
- The flat-state Lean transition renders a true Lean `match`: exactly one arm
  applies, and each arm's overflow/underflow bound conjuncts gate only that
  arm, so the aborts-if theorem stays valid. The ADT path still flattens arms
  to their union (`stmts_with_branch_union`) — explicit status-quo; per-arm
  ADT rendering is a follow-up.

### 4. Per-arm Kani conformance harnesses + `Mir.hooks` (#66 follow-ups, PR #97)

- Branch handlers previously got no conformance coverage (the flat harnesses
  self-skip under match semantics). Each (arm, effect) site now emits its own
  harness pinned with `kani::assume(<scrutinee> == <pattern>)`; the wildcard
  arm pins via negated assumes; the sibling-frame check is scoped to the
  arm's own effects. The flat path emits through the same factored helper
  byte-identically.
- `hook after_store(...)` asserts lower onto `Mir.hooks` and the transition
  emitter reads them from the IR — the last transitional `ParsedSpec` read in
  the effect-emission path is gone.

### 5. sBPF `Spec.lean` regen path (#88, PR #94)

`qedgen codegen` on a `pragma sbpf` spec had no working route to regenerate
`Spec.lean` (old-syntax specs errored on the handlers gate; modern ones
silently emitted an Anchor-shaped Rust scaffold; the generated header named a
verb deleted in v2.32). The codegen verb now decides `is_assembly_target()`
up front: for assembly targets only `--lean` and `--ci` emit; every
Rust-shaped backend is skipped with a stderr note. The canonical regen
command (now stamped in the header):

```bash
qedgen codegen --lean --spec <spec>.qedspec --lean-output formal_verification/Spec.lean
```

New `tests/sbpf_codegen_cli.rs` gates both spec shapes against the built
binary.

### 6. Prune dead Lean tests, unbuilt modules, orphaned fixture (PR #98)

The 9 never-built root-level `lean_solana/test_*.lean` files, the
`QEDGen/Solana.lean` umbrella (+ `Verify.lean`, reachable only through it),
and the self-referential `kani-smoke` fixture are deleted; README's phantom
test-suite claims and stale file tree fixed. `Arithmetic.lean` is kept
(embedded via `include_str!` for `--mathlib` scaffolds).

### 7. `CLAUDE.md` casing fix (release branch)

Git only ever tracked lowercase `claude.md`; the "byte-identical uppercase
mirror + CI gate" described in the file never existed — macOS
case-insensitivity made one file answer to both names locally, while Linux
checkouts had no `CLAUDE.md` at all. The file is now tracked as uppercase
`CLAUDE.md` (the canonical discovery name) and the mirror note is removed.

## Compatibility notes

- No spec-syntax or CLI-surface breaks. Specs using `effect { match … }`
  get semantically corrected Lean transitions (single-arm application) and
  new per-arm Kani harnesses; union-view overflow tests that were artifacts
  of `+=!` normalization correctly no longer emit.
- Generated `Cargo.toml` pins move to `tag = "v2.35.0"`.

## Gates

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test` (996 unit + all snapshot suites), `check-readme-drift.sh`
(19 commands), `qedgen check --regen-drift` (8 examples), `qedgen check
--frozen` per bundled example, the `old(...)` pre/post harness regen check,
and the `cargo audit` / `cargo deny` supply-chain gates all pass. No new
RustSec advisories (the five ignored IDs unchanged). Zero unintended `sorry`
in examples. The strict lake gate is covered by the green `lake-build.yml`
run on `main` at `4d812c8` (this release's exact Lean-relevant tree — local
`.lake/` dirs were intentionally pruned post-v2.34 for disk space).

Checklist quirk found en route: `qedgen check --frozen --spec <dir>` fails on
`cross-program-vault` because the sibling-glob picks up
`imports/cross-program-vault-admin/admin.qedspec` (`spec AdminConfig`) as a
fragment; the file-form invocation passes. Pre-existing (v2.34.0 binary on
the v2.34.0 tree fails identically) — tracked as a CLI bug, not a blocker.
