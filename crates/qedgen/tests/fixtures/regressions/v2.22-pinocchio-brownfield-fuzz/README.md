# v2.22 — Pinocchio brownfield Crucible fuzz

Fixture for the PRD-v2.22 Slice 3 regression: `qedgen probe --fuzz <budget> --root`
on a Pinocchio crate.

## What's here

```
buggy_pinocchio/
├── Cargo.toml      # `pinocchio = "0.6"` dep — triggers Runtime::Pinocchio detection
├── idl.json        # Maintainer-authored Codama IDL (3 instructions, 1-byte disc)
└── src/lib.rs      # Deliberately buggy handlers
```

Three handlers in `src/lib.rs`:
- `process_run`   — divide-by-zero panic.
- `process_maybe` — `Option::unwrap` on `None`.
- `process_drain` — sweeps `source` lamports into `target` with no authority check.

## What qedgen does with it

`crates/qedgen/tests/crucible_brownfield_smoke.rs` exercises the path:

```bash
qedgen probe --fuzz 0 --root crates/qedgen/tests/fixtures/regressions/v2.22-pinocchio-brownfield-fuzz/buggy_pinocchio/
```

- Runtime detection (`probe::detect_runtime_public`) sees `pinocchio = "0.6"` in
  `Cargo.toml` and picks `Runtime::Pinocchio`.
- `crucible_brownfield::synthesize_spec` discovers `idl.json` at the project
  root (highest-precedence Codama path) and passes it through verbatim.
- Handler list is synthesized from the IDL's `instructions[].name` (snake-cased,
  no `process_` prefix). The harness emitter PascalCases each handler to
  produce `instruction::Run`, `instruction::Maybe`, `instruction::Drain`
  literals matching the macro's output.
- `crucible_gen::generate` emits the harness at
  `<root>/.qed/fuzz/buggy_pinocchio/` with action stubs
  `action_run`, `action_maybe`, `action_drain`.
- The IDL is copied to `<harness>/idls/buggy_pinocchio.json` so
  `declare_fuzz_program!` finds it.
- `--fuzz 0` short-circuits before `cargo build` / `crucible run`, so the
  test doesn't need the Crucible binary on PATH.

## How to build / run for real (manual)

`cdylib` programs need the Solana toolchain. To build outside the qedgen
workspace:

```bash
cp -r crates/qedgen/tests/fixtures/regressions/v2.22-pinocchio-brownfield-fuzz/buggy_pinocchio /tmp/
cd /tmp/buggy_pinocchio
cargo build-sbf --arch v3
```

Then `cd .qed/fuzz/buggy_pinocchio && cargo build --features invariant_test`
to compile the harness. Live fuzz: `crucible run buggy_pinocchio invariant_test`.

## Why a Codama IDL (not a synthesised one)

v2.22 Slice 3 explicitly gates Pinocchio brownfield fuzz on a maintainer-authored
IDL. Codama (or any tool producing Anchor 0.30-shaped IDL) is the trusted source
for account/arg metadata. Scanner-based inference from handler bodies was
considered and rejected — `borrow_mut_*` patterns miss CPI-mutated accounts,
`from_le_bytes` patterns miss zero-copy unpacking, and account-name conventions
vary. See `crates/qedgen/src/crucible_brownfield.rs` `synthesize_pinocchio`.
