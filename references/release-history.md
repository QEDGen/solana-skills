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

Use PRDs and release notes in `docs/prds/` for detailed historical context.
