# Runtime and Mode Detection

Use the deterministic `scripts/preflight.sh` output as the source of truth.

## Target selection

Resolve one program root before runtime detection. In a monorepo, do not scan
unrelated sibling programs for runtime signals. When several program manifests
are plausible, ask for a selection instead of guessing.

## Runtime signals

- Anchor: selected manifest depends on `anchor-lang`.
- Pinocchio: selected manifest depends on `pinocchio`.
- Native Rust: selected manifest depends on `solana-program` without Anchor.
- QEDGen codegen: selected source contains `#[qed(verified)]` or uses the
  relevant codegen dependency.
- Assembly-only sBPF: selected root has `.s` sources and no Rust handler source.

A Rust target containing helper assembly remains a Rust target.

## Spec resolution

Use an explicit `--spec` path when supplied. Otherwise, use a unique
`*.qedspec` under the selected root, excluding build and VCS directories. More
than one candidate is ambiguous and requires explicit selection. No candidate
means spec-less mode.

Assembly-only source-pattern analysis is unsupported. A spec-aware probe may
still run if it does not claim to inspect assembly implementation semantics.
