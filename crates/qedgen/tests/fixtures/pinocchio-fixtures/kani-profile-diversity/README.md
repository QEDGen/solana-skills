# Kani Profile Diversity Proof Lab

This fixture is a generic Pinocchio proof lab for `--kani-impl`.

It mirrors source and ABI facts needed by larger router-style programs:

- source-inferred account order and dispatcher tags;
- SPL-token-shaped account bytes, token amount deltas, and mint/owner projections;
- PDA-owned accounts and non-`program_id` PDA derivations;
- scalar widths, repeated records, account layouts, and fixed magic bytes;
- incomplete-profile diagnostics.

The env-gated proof-lab gate in `codegen_smoke.rs` copies this whole directory,
generates `src/kani_impl.rs`, and runs selected generated proofs with real
`cargo kani`. The fast non-ignored test remains an emitter regression check. The
ignored Kani proof gate proves generated harnesses that call the real dispatcher,
assert success, assert concrete post-state facts, and require each selected
success path to satisfy `kani::cover!`.

To add another profile-backed case, extend `verification/profile.qedspec` with
the handler contract, add the matching dispatcher arm and handler body in
`src/lib.rs`, and place any required ABI facts in `schema/profile.schema`. If the
new generated proof should be part of the real proof gate, add its name to
`smoke_pinocchio_kani_profile_diversity_with_cargo_kani`.
