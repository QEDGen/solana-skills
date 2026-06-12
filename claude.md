# CLAUDE.md

Guidance for Claude Code when working in this repository. This file is loaded into **every** session — keep it lean. Deep material lives in `references/` and `docs/design/`; this file orients and points.

> The lowercase `claude.md` is a byte-identical mirror — edit both together (CI gate; see [docs/RELEASING.md](docs/RELEASING.md)).

## What this is

QEDGen is a Claude Code skill for spec-driven verification of Solana programs. The `.qedspec` is the single source of truth: `qedgen check` validates it (lint + proptest + Lean), `qedgen codegen` generates downstream artifacts (Rust scaffold, Kani/proptest harnesses, Lean proofs, CI), and `#[qed(verified)]` stamps verified code. Leanstral and Aristotle fill hard proof sub-goals when escalated.

**Core loop:** intent → write `.qedspec` → `qedgen check` → iterate → `qedgen codegen --all` → fill `todo!()` → `qedgen verify`.

The UX is **agent-first**: the user interacts with the SKILL (`SKILL.md`) and agents; the `qedgen` CLI is glue between agents and artifacts, not a user-facing tool. Full CLI reference: [`references/cli.md`](references/cli.md).

## How to think about this codebase

**Escalation ladders — mechanize first, escalate only after you've tried.**

- *Proofs:* (1) mechanical → codegen template (`lean_gen_mir.rs`); (2) tractable → write the Lean directly in-session (most real proof bodies: case analysis, Mathlib lemma selection, per-handler structural proofs); (3) hard → Leanstral (`qedgen fill-sorry`); (4) last resort → Aristotle (`qedgen aristotle submit`).
- *Code/tests:* (1) mechanical → codegen template (`codegen_mir` + `codegen_shared::mechanize_effect`); (2) tractable → fill `todo!()` in-session (events, transfers, CPI wiring, complex effect RHS); (3) last resort → refine the spec (under-specification is the real bug).

**Design principles:**
- A DSL feature that *structurally eliminates* a proof obligation beats a new proof template or a shelled-out sorry.
- Codegen mechanizes only deterministic translation; business logic (transfers, CPI authority, events, PDA creation) stays agent-filled `todo!()`.
- When a template can't close a case, emit `sorry` with a comment — never bury it in tactics that might spuriously close.
- Don't pre-shell to Leanstral/Aristotle for what a local LLM can do. Escalation is for *after* you've tried, not when you expect to need to.
- The typed MIR (`mir.rs`) exists for bug-class elimination, not LoC: matches over the closed `Stmt` enum are exhaustive by discipline (no `_` arms — see the enum doc in `mir.rs`), so a new statement kind is a compile error at every `Stmt` consumer (Lean codegen, Rust scaffold, and the Kani/proptest effect lowering via `rust_codegen_util::stmt_effect_triple`), not silent drift. Conditional `effect { match … }` bodies lower to `Stmt::Branch` (Phase 5): Rust renders a `match`, the flat-state Lean transition applies exactly one arm (per-arm bound guards); the ADT Lean path still flattens arms to their union (`stmts_with_branch_union`). Measure intrinsics by bugs eliminated, not lines saved.

## Build and test

```bash
cargo build --release && cp target/release/qedgen bin/qedgen   # always copy to bin/
cargo test                                                      # Rust unit + snapshot tests
cd lean_solana && lake build                                    # Lean support library
```

Snapshot suites (`tests/{mir,kani,codegen,proptest}_snapshot.rs`) gate every fixture against checked-in references. Regenerate with `UPDATE_SNAPSHOTS=1 cargo test --test <suite>` — but `rm bin/qedgen` and rebuild first (snapshots run the built binary and won't auto-rebuild a stale one). Full command + flag reference: [`references/cli.md`](references/cli.md).

## Crate map

**`crates/qedgen-macros/`** — `#[qed]` proc macro: compile-time drift detection (`lib.rs` entry, `verified.rs` content-hash + `compile_error!`).

**`crates/qedgen/src/`** — CLI, parsers, codegens:
- `main.rs` — CLI entry points (init, setup, check, codegen, verify, reconcile, generate, fill-sorry, aristotle, spec, asm2lean, consolidate, probe, adapt, interface, readiness, check-upgrade, …)
- `chumsky_parser.rs` / `chumsky_adapter.rs` — `.qedspec` → typed AST
- `mir.rs` — typed Solana-native IR; `lower(parsed) -> Mir` is the canonical entry, consumed by all four codegens
- `lean_gen_mir.rs` — Lean 4 codegen (flat `structure State` default; `mir.adt_state` for inductive; `mir.is_assembly` → sBPF `render_sbpf`)
- `kani_mir.rs` / `proptest_gen_mir.rs` — Kani BMC + proptest harness codegens
- `codegen_mir.rs` — Rust codegen for all three targets (Anchor / Quasar / Pinocchio)
- `codegen_shared.rs` — shared Rust-codegen helpers: `FrameworkSurface`, `generate_guards`, Pinocchio scaffold, per-target SPL/System CPI dispatch (`try_emit_cpi`)
- `lean_sidecars.rs` — pinned-interface `import` lines + sibling `<Iface>.lean` axiom modules
- `check.rs` — lint, coverage matrix, drift detection
- `pinocchio_probe.rs` / `miri_verify.rs` — Pinocchio audit-site enumerator + Miri verify backend
- `asm2lean.rs` — sBPF `.s` → Lean program module
- supporting: `api.rs` (Mistral), `aristotle.rs`, `drift.rs`, `idl.rs` / `idl2spec.rs`, `fingerprint.rs`, `validate.rs`, `deps.rs`, `project.rs`, `consolidate.rs`, `unit_test.rs`, `integration_test.rs`

**`lean_solana/`** — Solana axiom library (`QEDGen.Solana.{Account,Cpi,State,Valid}`); sBPF semantics + binary-proof engines come from the `qedsvm` package (`require qedsvm`, `SVM.SBPF.*`, pinned tag).

Codegen/MIR architecture rationale (cross-cutting transforms, CPI composition, divergence) lives in [`docs/design/`](docs/design/).

## Key concepts

First-class verification features — one-line orientation; full mechanics in [`references/qedspec-dsl.md`](references/qedspec-dsl.md):

- **CPI ensures-as-axiom** — `call Iface.handler(...)` emits a per-call-site theorem. Tier-1/2 callees (declare `ensures` + `upstream { binary_hash }` pin) discharge via `exact Iface.handler.ensures_axiom_<i>`; Tier-0 (no ensures) keep `by sorry` + fire the `cpi_no_callee_ensures` lint. (`lean_gen_mir::render_cpi_theorems`, `cpi_substitute`)
- **First-class interfaces** — `interface` participates across Lean / Kani / verify. Bundled SPL + System + Metaplex stdlib in `crates/qedgen/data/interfaces/`; `verify --check-upstream` promotes pin mismatches to CRIT.
- **State-aware contracts** — callee `ensures` over abstract `state.X`; callers map via per-call-site `state_binders {}`; verified-callee composition imports `.qed/proofs/<Iface>.lean`.
- **`pragma state_repr = adt`** — explicit opt-in to inductive multi-variant `State` (default is flat `structure State` + `status`). Single source: `ParsedSpec::state_repr_is_adt()` → `Mir::adt_state`. `cross-program-vault` is the sole bundled ADT example.
- **Impl-targeted Kani (`--kani-impl`)** — `kani_impl.rs` exercises the real Anchor handler against a symbolic `Accounts` context; auto-triggers on `modifies ⊋ effect.lhs` or unbounded `ref_impl` arithmetic. Anchor only.

## Verification scope

- **Verify:** authorization (signers/constraints), conservation (token totals), state machines (lifecycle/one-shot), arithmetic safety (overflow/underflow), CPI correctness (program/accounts/discriminator).
- **Trust (axioms):** SPL Token, Solana runtime (PDA derivation, ownership), CPI mechanics, Anchor framework.

See `examples/rust/escrow/formal_verification/VERIFICATION_SCOPE.md`.

## References

- [`references/cli.md`](references/cli.md) — every CLI command + flag
- [`references/qedspec-dsl.md`](references/qedspec-dsl.md) — full `.qedspec` DSL
- [`references/proof-patterns.md`](references/proof-patterns.md) — Lean tactic rules + common errors/fixes
- [`references/sbpf.md`](references/sbpf.md) — sBPF workflow, `wp_exec`/`wp_step`, memory disjointness, simp-performance rules
- [`references/support-library.md`](references/support-library.md) — `QEDGen.Solana` API
- [`docs/design/`](docs/design/) — codegen / MIR architecture
- [`docs/RELEASING.md`](docs/RELEASING.md) — **pre-release checklist (run before any tag)**
- `SKILL.md` — the user-facing proof/verification workflow
- `.claude/rules/lean-proofs.md` — Lean gotchas, auto-loaded when editing `.lean` files

## Environment

- `MISTRAL_API_KEY` — `fill-sorry` / `generate` (Lean sorry-filling only)
- `ARISTOTLE_API_KEY` — `aristotle` commands (hard sub-goals; https://aristotle.harmonic.fun)
- `QEDGEN_VALIDATION_WORKSPACE` — override validation workspace (default: platform cache dir)

Spec writing, validation, and codegen need no API keys or Lean toolchain. First Lean build is expensive (15–45 min for Mathlib) — run `qedgen setup` first. If `lake build` reports "could not resolve 'HEAD' to a commit", remove `.lake/packages/mathlib` and run `lake update`.
