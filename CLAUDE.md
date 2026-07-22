# CLAUDE.md

Guidance for Claude Code when working in this repository. This file is loaded into **every** session — keep it lean. Deep material lives in `references/` and `docs/design/`; this file orients and points.

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
- Codegen mechanizes only deterministic translation. Supported direct CPIs with transaction signers, Anchor/Quasar lifecycle account creation, and Anchor builder-shape CPIs with assemblable PDA signer seeds are complete; transfer sugar, events, unsupported calls, Pinocchio PDA creation, and every other PDA-signed CPI stay agent-filled `todo!()`. The per-target contract lives in `docs/framework-support.md`.
- When a template can't close a case, emit `sorry` with a comment — never bury it in tactics that might spuriously close.
- Don't pre-shell to Leanstral/Aristotle for what a local LLM can do. Escalation is for *after* you've tried, not when you expect to need to.
- The typed MIR (`mir.rs`) exists for bug-class elimination, not LoC: matches over the closed `Stmt` enum are exhaustive by discipline (no `_` arms — see the enum doc in `mir.rs`), so a new statement kind is a compile error at every `Stmt` consumer (Lean codegen, Rust scaffold, and the Kani/proptest effect lowering via `rust_codegen_util::stmt_effect_triple`), not silent drift. Conditional `effect { match … }` bodies lower to `Stmt::Branch` (Phase 5): Rust renders a `match`; both the flat and ADT Lean transitions apply exactly one arm with per-arm bound guards (ADT via `emit_adt_branch`). Caveat: the ADT abort/overflow emitters (`adt_effect_map` / `adt_bound_conds`) treat `Stmt::Branch` as a no-op, so ADT abort/overflow obligations ignore branch-arm effects (honest today only because those bodies are `by sorry`). Measure intrinsics by bugs eliminated, not lines saved.
- The closed-enum discipline applies to small enums too: never tally or gate on an enum with open-coded `.filter(|x| x.variant == …)` chains — count through an exhaustive `match` (e.g. `SeverityCounts::of`) so a new variant is a compile error at every accounting site, not a silently dropped case (#260/#270: `Severity::Error` vanished from the check summary and exit code for ten releases this way).

## Build and test

```bash
cargo build --release && cp target/release/qedgen bin/qedgen   # always copy to bin/
cargo test                                                      # Rust unit + snapshot tests
cd lean_solana && lake build                                    # Lean support library
```

Snapshot suites (`tests/{mir,kani,codegen,proptest}_snapshot.rs`) gate every fixture against checked-in references; the shared harness lives in `tests/common/mod.rs` and rebuilds `qedgen` before driving it (no stale-binary footgun). Regenerate with `UPDATE_SNAPSHOTS=1 cargo test --test <suite>`. Snapshots prove output stability, not correctness: the executable artifact gate (`tests/generated_artifact_gate.rs`, `-- --ignored`, own CI job) regenerates every bundled Anchor example, compiles all generated Rust artifacts, runs the generated unit tests and proptests, and type-checks the Kani harness via `crates/kani-compile-stub` (#294). Full command + flag reference: [`references/cli.md`](references/cli.md).

## Dogfooding → toolchain backlog (dev-mode loop)

When you use QEDGen on a real target from this repo (verifying an audit program, a codegen bring-up, a spec at scale), close the loop as the **last step**: run the **`toolchain-scout`** agent (`.claude/agents/toolchain-scout.md`) on the session. It mines the run for friction — codegen bugs, missing modes/DSL constructs, DX papercuts, reusable techniques — and files evidence-backed, deduplicated entries to [`docs/toolchain-backlog.md`](docs/toolchain-backlog.md) plus one sanitized GitHub issue each. The scout **proposes**; the main loop fixes codegen bugs in source (never works around them) with a regression test. This lives here **only** — it is dev/maintainer tooling and must NOT go in `SKILL.md`, which ships to end users verifying their own programs.

## Crate map

**`crates/qedgen-hash-core/`** — canonical spec/body hashing (`sha256_hex16`, `canonical_token_string`, `extract_handler_block`, `normalize_spec_block`, `spec_context_digest`, `scan_balanced_block`) shared by the CLI and the proc macro — agreement by construction, no hand-kept mirrors. `tests/stamp_crosscheck.rs` hard-asserts checked-in `#[qed(verified, hash = …)]` stamps.

**`crates/qedgen-macros/`** — `#[qed]` proc macro: compile-time drift detection (`lib.rs` entry, `verified.rs` content-hash + `compile_error!`); hashing delegated to `qedgen-hash-core`.

**`crates/qedgen/src/`** — CLI, parsers, codegens. Directory modules by pipeline stage (post-v2.35 reorg; root re-exports in `main.rs` keep `crate::<module>` paths stable):
- `main.rs` / `cli.rs` / `run.rs` / `run_helpers.rs` — CLI surface (split out of `main.rs` in v2.36): `main.rs` (binary entry + the `crate::<module>` re-export hub), `cli.rs` (clap arg defs for every subcommand: init, setup, check, codegen, verify, reconcile, generate, fill-sorry, aristotle, spec, asm2lean, consolidate, probe, adapt, interface, readiness, check-upgrade, …), `run.rs` (`command_name_of` + the `dispatch` match), `run_helpers.rs` (dispatch-support glue). `stamp` (v2.44) emits `#[qed(verified)]` attributes gated on recorded implementation-verified evidence; `adapt` and `spec --idl` are soft-deprecated (scaffold → probe elicitation, attribute → stamp)
- `spec/` — `.qedspec` front-end: `chumsky_parser` / `chumsky_adapter` (→ typed AST), `ast`, `validate`, `quantifier`, `spec_hash`, `import_resolver`, `idl` / `idl2spec`
- `mir/` (`mod.rs` = the IR) — typed Solana-native IR; `lower(parsed) -> Mir` is the canonical entry, consumed by all four codegens; `cpi_substitute`
- `codegen/` — all backends: `lean_gen_mir` (Lean 4; flat `structure State` default, `mir.adt_state` for inductive, `mir.is_assembly` → sBPF `render_sbpf`), `kani_mir` / `kani_impl` / `proptest_gen_mir` (Kani BMC + proptest), `codegen_mir` (Rust for Anchor / Quasar / Pinocchio), `codegen_shared` (`FrameworkSurface`, `generate_guards`, Pinocchio scaffold, per-target SPL/System CPI dispatch `try_emit_cpi`), `rust_codegen_util`, `lean_sidecars` (pinned-interface imports + `<Iface>.lean` axiom modules), `asm2lean` (sBPF `.s` → Lean), `crucible_gen`, `interface_gen`, `unit_test`, `integration_test`, `banner`, `fingerprint`
- `check/` (`mod.rs`) — lint, coverage matrix, drift detection
- `obligations/` (`mod.rs` = types + recorder + manifest IO, `inventory.rs` = expected-inventory + reconcile) — the backend-obligation manifest (#332): every requested obligation ends `emitted` / `unsupported(reason)` / `failed` per backend, recorded at the emission sites inside the three model codegens (never by scanning output) and reconciled against the spec-derived inventory so a silent skip surfaces as `failed`. Persisted to `.qed/obligations.json` by codegen; recomputed in memory by `check --coverage` (backend-coverage section) and `verify --strict` (gate)
- `probe/` (`mod.rs` = the enumerator) — audit data layer: `pinocchio_probe`, `shank_probe`, `crucible_probe` / `crucible_brownfield`, `arithmetic_symbol_probe`, `lifecycle_probe`, `paired_validator_probe`, `probe_repro`, spec elicitation (`hypothesize` = evidence-anchored invariant hypotheses, `elicit` = structured answers + clause lowering), scaffold-to-spec interview (`cluster`, `handler_intent`, `prompts`, `ratify`)
- `adapt/` — brownfield ingest: `anchor_adapt` / `anchor_check` / `anchor_extractor` / `anchor_project` / `anchor_resolver`, `native_extractor`, `pinocchio_extractor` / `pinocchio_profile` / `pinocchio_to_spec`
- `verify/` (`mod.rs` = the orchestrator) — `miri_verify`, `sbpf_verify`, `verify_{counterexample,kani_parse,proptest_parse,probe_repros}`, `drift`, `regen_drift`, `upstream_check`, `ratchet`, `evidence` (persisted `.qed/verify-evidence.json` — the `stamp` gate's input)
- `dispatch/` — external LLM dispatch: `api` (Mistral), `aristotle`
- `project/` (`mod.rs` = scaffolding) — `init`, `deps`, `qed_lock`, `qed_manifest`, `consolidate`, `reconcile`, `fill`, `proofs_bootstrap`, `feedback`

**`lean_solana/`** — Solana axiom library (`QEDGen.Solana.{Account,Cpi,State,Valid}`); sBPF semantics + binary-proof engines come from the `qedsvm` package (`require qedsvm`, `SVM.SBPF.*`, pinned tag).

Codegen/MIR architecture rationale (cross-cutting transforms, CPI composition, divergence) lives in [`docs/design/`](docs/design/).

## Key concepts

First-class verification features — one-line orientation; full mechanics in [`references/qedspec-dsl.md`](references/qedspec-dsl.md):

- **CPI ensures-as-axiom** — `call Iface.handler(...)` emits a per-call-site theorem. Tier-1/2 callees (declare `ensures` + `upstream { binary_hash }` pin) discharge via `exact Iface.handler.ensures_axiom_<i>`; Tier-0 (no ensures) keep `by sorry` + fire the `cpi_no_callee_ensures` lint. (`lean_gen_mir::render_cpi_theorems`, `cpi_substitute`)
- **First-class interfaces** — `interface` participates across Lean / Kani / verify. Bundled SPL + System + Metaplex stdlib in `crates/qedgen/data/interfaces/`; `verify --check-upstream` promotes pin mismatches to CRIT.
- **State-aware contracts** — callee `ensures` over abstract `state.X`; callers map via per-call-site `state_binders {}`; verified-callee composition imports `.qed/proofs/<Iface>.lean`.
- **`pragma state_repr = adt`** — explicit opt-in to inductive multi-variant `State` (default is flat `structure State` + `status`). Single source: `ParsedSpec::state_repr_is_adt()` → `Mir::adt_state`. `cross-program-vault` is the sole bundled ADT example.
- **Impl-targeted Kani (`--kani-impl`)** — `kani_impl.rs` exercises the real Anchor handler against a symbolic `Accounts` context; auto-triggers on `modifies ⊋ effect.lhs` or unbounded `ref_impl` arithmetic. Anchor only. Brownfield shapes: `--kani-impl-brownfield` (state-struct harness, #162) and `--kani-impl-context` (real `try_accounts` + instruction fn over symbolic `AccountInfo`s — instruction-level authorization gates, #169; `pragma context_struct`).

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
- [`docs/framework-support.md`](docs/framework-support.md) — per-framework capability matrix (Anchor / Quasar / Pinocchio / native / sBPF), gate-verified
- [`docs/RELEASING.md`](docs/RELEASING.md) — **pre-release checklist (run before any tag)**
- `SKILL.md` — the user-facing proof/verification workflow
- `.claude/rules/lean-proofs.md` — Lean gotchas, auto-loaded when editing `.lean` files

## Environment

- `MISTRAL_API_KEY` — `fill-sorry` / `generate` (Lean sorry-filling only)
- `ARISTOTLE_API_KEY` — `aristotle` commands (hard sub-goals; https://aristotle.harmonic.fun)
- `QEDGEN_VALIDATION_WORKSPACE` — override validation workspace (default: platform cache dir)

Spec writing, validation, and codegen need no API keys or Lean toolchain. First Lean build is expensive (15–45 min for Mathlib) — run `qedgen setup` first. If `lake build` reports "could not resolve 'HEAD' to a commit", remove `.lake/packages/mathlib` and run `lake update`.

Generated Rust is rustfmt-formatted at the single write seam (`codegen_shared::write_generated_file` → `format_rust_source`; new emitters must route through it, gated by `tests/generated_rustfmt_gate.rs`). rustfmt is a soft dependency: absent → unformatted output + one warning. rustfmt is NOT token-neutral (trailing commas, `!((…))` paren removal), so `#[qed(verified, hash=…)]` body hashes are computed AFTER formatting (`scaffold.rs`); tests match generated Rust whitespace-insensitively (`compact` helpers). Static Rust templates live in `crates/qedgen/templates/*.rs` (`include_str!`), kept fmt-clean by the same gate.
