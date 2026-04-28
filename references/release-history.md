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

Use PRDs and release notes in `docs/prds/` for detailed historical context.
