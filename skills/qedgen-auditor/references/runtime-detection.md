# Runtime and Mode Detection

Use the deterministic `scripts/preflight.sh` output as the source of truth.

## Target selection

Resolve one program root before runtime detection. A program manifest is a
`Cargo.toml` with a `[package]` section that depends on a Solana runtime crate
(`anchor-lang`, `pinocchio`, `quasar-lang`, or `solana-program`); a workspace
root that only mentions those crates under `[workspace.dependencies]` is not a
program. `preflight.sh` uses `--root`'s own manifest when it is a program
manifest; otherwise it auto-selects a unique nested program manifest and
reports its directory as `program_root=`. Several candidates are ambiguous:
the script exits with the candidate list, and you ask the user for a
selection instead of guessing. Runtime and source scans are scoped to the
selected `program_root`, so unrelated sibling programs never affect
detection.

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
means spec-less mode. A `qed.toml` spec-dependency manifest next to the spec
(or at the target root) is reported as `qed_manifest=`; it signals imported
interfaces that the audit should treat as part of the trust surface.

## Capability

`audit_capability=full` requires a ready QEDGen *and* either a detected
runtime or a resolved spec. An unknown runtime with no spec downgrades to
`read-only` even when QEDGen is available — report the downgrade rather than
proceeding as if the target were understood.

Assembly-only source-pattern analysis is unsupported. A spec-aware probe may
still run if it does not claim to inspect assembly implementation semantics.
