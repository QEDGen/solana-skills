# Release History Notes

This file keeps version-feature history out of `SKILL.md`.

## Current Contract

QEDGen's stable workflow is:

```text
check -> codegen -> agent fill -> verify
```

`qedgen check` validates and reports on `.qedspec`.
`qedgen codegen` generates verification artifacts and an agent-fill Rust
scaffold.
`qedgen verify` runs generated backends and framework builds where
applicable.

## v2.x Highlights

- v2.5 introduced richer spec composition and proof-generation patterns.
- v2.6 tightened generated Kani/proptest behavior and bundled example checks.
- v2.7 improved parser, arithmetic, and generated harness correctness.
- v2.8 added `import`, `qed.toml`, `qed.lock`, `--frozen`, and upstream checks.
- v2.9 added brownfield Anchor adaptation and `#[qed]` drift stamps.
- v2.10 removed stale `SPEC.md` generation paths and strengthened probe and codegen behavior.
- v2.11 cleanup work focuses on simplifying codegen contracts, target-specific surfaces, generated scaffold smoke tests, and example drift gates.
- v2.11.2 closes the harness loop on bundled examples: ships the `handler_unfilled_todo` lint, fixes Quasar `Program<Token>` codegen for token transfers, refines spec-completeness lints to eliminate boundary-only false positives, and adds per-slot proptest lowering for wide-binder forall properties.
- v2.11.3 fixes the Lean side end-to-end: four `lean_gen` codegen bugs (auth-var as State field, account-binding `.pubkey` in effect RHS, raw `Nat` indices into `Map[N] T`, cover-witness fallback poisoning Pubkey fields), the `init.rs` lakefile silently excluding every user `Proofs.lean` from `lake build`, and the `drift.rs` ↔ proc-macro hash divergence that broke `qedgen check --update-hashes`. Adds `scripts/check-lake-build.sh` as the pre-release gate that catches this class. **All 10 bundled examples now `lake build` clean.**
- v2.11.4 makes `qedgen verify` and `qedgen reconcile` walk `.qed/config.json` like `check` and `codegen` already do (`--spec` becomes optional on both), removes stale milestone-marker strings (`--kani` "lands in v2.4-M2", `qedgen adapt` "v2.10+" non-Anchor message), adds the previously-undocumented `qedgen verify --check-upstream` / `--offline` / `--rpc-url` flags to the README with a Solana CLI dependency callout, ships a `lake-build.yml` GitHub Actions workflow for the Lean-side gate, and starts tracking `RELEASE-*.md` notes in-repo.
- v2.22 closes the inverse-recall gaps surfaced by the subscriptions-vs-Cantina bench: three new arithmetic-symbol probe rules (silent-success / graceful-error-as-DoS / unchecked-arith-with-fund-flow), a paired-validator asymmetry detector, a lifecycle external-state catalog (`external_authority_not_revoked_on_close`), a `gated_by` triad detector that drops Pinocchio noise-floor from 114 → 12 ungated mediums, and Pinocchio brownfield Crucible fuzz (the v2.21-deferred runtime gate is lifted; requires a maintainer-authored Codama / Anchor 0.30 IDL on disk). 8 PRD-named bench misses → 8 categorical hits. Folds 4 community Kani codegen fixes from PR #46.
- v2.22.1 doc-only hotfix: drops `schema X { ... }` blocks, the `include schema_name` handler clause, and `on/when/then ident` sugar from `references/qedspec-dsl.md`. The parser silently no-oped `include` and never accepted `schema` / `on` / `when` / `then`; zero bundled examples used any of them.
- v2.23 closes the silent-vacuous-proof bug class in proptest and Kani codegen: every preservation `property` whose body referenced `old(...)` lowered to a structural tautology (`s.x cmp s.x`) and reported green without actually checking the binary obligation. The fix is structural — classify properties at parse time, bifurcate the property-fn signature into `fn p(pre: &State, post: &State) -> bool`, and capture pre-state in the per-handler preservation harness. Plus two defense-in-depth lints (`vacuous_property_lowering`, `old_in_single_state_context`). Also ships the brownfield first-contact onboarding flow: `SKILL.md` brownfield-detect branch routes existing-program first-contact through `/qedgen-auditor`, an auditor → spec scaffold handoff in `skills/qedgen-auditor/SKILL.md`, the 580-line finding-to-spec mapping table at `skills/qedgen-auditor/references/finding_to_spec.md`, and a bundled `examples/rust/brownfield-onboarding/` walkthrough.
- v2.25 introduces three layers that compose into the LP-shape verification pattern: `ref_impl name (...) : T = <expr>` for named pure reference math callable from `ensures`, `modifies [...]` agent-fill that emits `todo!()` sites for fields declared in `modifies` but absent from `effect`, and ensures-preservation Kani harnesses that verify the spec-translated transition satisfies its own `ensures` against `(pre, post)`. New `unconstrained_modifies` P0 lint for the completely-unbounded `modifies` shape.
- v2.26 closes the verification loop on CPIs and the user's real impl. **Slice 2:** variant ADT modifies-fill — `emit_variant_state_handler_body` learns the v2.25 Phase A pattern, ADT specs now participate. **Slice 3:** ref_impl composition — ref_impls call other ref_impls, take `Map[N] T` parameters, are callable from `requires`; recursive ref_impls rejected at parse time. **Slice 4a:** first-class interfaces (Lean) — `render_cpi_theorems` applies `<Iface>.<handler>.ensures_axiom_<idx>` for Tier-1/2 callees (closes the v2.8 G3 `by sorry` carve-out); bundled SPL Token + System Program + Metaplex Token Metadata stdlib via `import X from "spl"` / `"system"` / `"metaplex"` with no `qed.toml` entry. **Slice 4b:** first-class interfaces (Kani, both shapes) — shared `cpi_substitute` helper splices `kani::assume(<callee_ensures, substituted>)` after every CPI call site in both the v2.25 spec-model harness and the new impl-targeted harness. **Slice 4c:** `--check-upstream` as a differentiated-severity gate (CRIT in `verify`, P2 in `check --frozen`, `--strict` escalates, `--upstream-stale-ok` suppresses for offline dev). **Slice 1:** impl-targeted Kani harness `--kani-impl` opt-in calling the user's real Anchor handler against a symbolic `Accounts` context with PDA-derived addresses bound via spec-declared seeds. New lints: `ref_impl_unbounded_arith` (P2, surfaces the Lean Nat / Rust u64 semantic gap), `multi_cpi_same_field` (P2, detects over-constraint from ≥2 CPIs referencing the same caller-state field), `cpi_no_callee_ensures` (P1, Tier-0 callee gap). New DSL surface: `-> result : ReturnType` on interface handler signatures. New module: `programs/src/ref_impls.rs` Rust-side ref_impl emission so guards/handlers/properties call them by name. Pre-existing bind_state leak for record-form-state specs without explicit accounts closed via default-state-account synthesis in `chumsky_adapter::adapt()`.

Use per-release notes in `docs/prds/RELEASE-vN.md` for detailed
historical context (this file is a sparse highlights index, not a
full changelog).
