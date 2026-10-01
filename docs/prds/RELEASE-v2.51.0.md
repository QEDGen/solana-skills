# QEDGen v2.51.0: sBPF v3 by default and verified byte-level discharge

**Scope:** 10 merged PRs since v2.50.0 (#431, #432, #433, #434, #435, #436,
#437, #438, #439, and #441), closing #404, #405, #406, #422, #423, #426,
#427, #429, #430, and #440. #424, #425, and #428 were closed after checks
showed that no code change was needed.

**Theme:** SIMD-0500, planned for Agave 4.4, rejects deploys and upgrades of
programs older than sBPF v3. This release makes v3 the default target
everywhere qedgen chooses a version, flags older builds before the cluster
does, and models v3 in the Lean proofs. It also makes `qedgen discharge`
report a real verdict: Lean now checks every emitted module.

## 1. sBPF v3 is the default target (#431, #434)

- New spec pragma `pragma sbpf_version = v3 | v0`, and a matching
  `asm2lean --sbpf-version v3|v0` flag.
- Under v3, `asm2lean` places `.rodata` at address 0. The `RODATA_*` addresses
  match what `sbpf build -a v3` writes into the `lddw` immediates.
- The generated Lean module records its version in a header line. The version
  order is: CLI flag, then spec pragma, then the recorded header, then v3.
- A module generated before this release has no recorded version. It now
  regenerates as v3, and qedgen prints a note that its `RODATA_*` addresses
  change.
- V0 still works but is deprecated. Choosing it prints a warning, and the new
  `check` lint `sbpf_version_v0_deprecated` reports it. V0 is removed in
  qedgen v3.0.
- New lints: `sbpf_version_invalid` (error) and `sbpf_version_without_sbpf`
  (warning).
- `probe --execute-repros` and `probe --fuzz` read the built `.so` and warn once
  per file when it is older than v3. `cargo build-sbf` still defaults to V0, so
  a plain build gives a program that cannot be deployed after SIMD-0500.
- User-facing build commands now say `cargo build-sbf --arch v3` and
  `anchor build -- --arch v3`.
- The four bundled sBPF examples declare `pragma sbpf_version = v3` and are
  regenerated.

## 2. Readiness flags programs older than v3 (#432)

`readiness --so <program.so>` and `check-upgrade --new-so <program.so>` read the
ELF header of the built program. When the sBPF version is below 3, they report
`QED002` (`sbpf-version-below-v3`) as unsafe (exit 2).

- A version newer than v3 passes.
- A missing file, a non-ELF file, or an ELF for another machine is an error
  (exit 3), never a pass.
- `--unsafe allow-pre-v3-sbpf` acknowledges the finding for a V0 program that
  is deployed and will never be upgraded.
- `readiness --idl` is optional when `--so` is given, so Pinocchio, native, and
  sBPF assembly programs can use the check.

Rebuilding a program as v3 changes its bytes. Any `upstream { binary_hash }`
pin on it drifts, and `verify --check-upstream` reports that.

## 3. Lean proofs model sBPF v3 (#438, #439)

- The qedsvm pin moves from v0.10.1 to v0.13.0, the first qedsvm release with v3
  execution semantics. The Lean toolchain stays at v4.30.0.
- `asm2lean` lifts the eleven JMP32 instructions (`jeq32` to `jset32`). They
  are an error under V0, which has no 32-bit jumps.
- `verify --asm` no longer prints a note that the proofs use V0 semantics.
- `references/sbpf.md` has a new JMP32 section. It also says to bind every
  loaded value that a branch compares to a variable. A raw `readU64` in a
  branch condition can make the kernel check of a `wp_exec` proof run without
  finishing.
- `check-lake-build.sh` and the lake-build workflow now also build the Lean
  projects under `crates/qedgen/tests/fixtures/`, including a new v3 JMP32
  fixture with proofs of all three exit paths.

## 4. `discharge` reports a checked verdict (#435, #436, #437)

Before this release, `qedgen discharge` reported success when qedlift exited 0
and the emitted file had no `sorry`. Lean never ran. Now it reads qedlift's
structured outcome, type-checks every emitted module with Lean, and reports one
verdict:

| Verdict | Meaning | Exit |
|---|---|---|
| `verified` | qedlift emitted the refinement and Lean accepted it | 0 |
| `emitted` | qedlift emitted the refinement, but no Lean project was found | 0 |
| `rejected` | The obligation conflicts with the bytes or layout | 1 |
| `unsupported` | qedlift cannot bind the obligation | 1 |
| `model_only` | qedlift lifted the program but did not act on the descriptor | 1 |
| `incomplete` | A spec-expected path has no trace, or path kinds are unknown | 1 |
| `failed` | qedlift failed, or Lean rejected the modules or found `sorry` | 1 |

CI that needs a proof should gate on `"verdict": "verified"` in the `--json`
report.

- **New flags:** `--lean-project` (default: the nearest Lake project at or above
  `--out-dir`) and `--json`.
- **Parameter deltas (#436).** `total += amount` now emits descriptor schema v3
  with an explicit `input_layout`. Use `--account-data-lengths` and either
  `--idl` or `--account-index`. qedgen never infers the layout. Constant deltas
  still emit schema v1, byte for byte as before.
- **Whole transitions (#437).** `--transition` reports one row per path. The spec
  expects a `success` path plus one rejection path per `requires ... else E`. An
  expected path with no trace gives `incomplete`, never a success. The report
  records the binary's `program_sha256` and keeps
  `all_discovered_paths_verified` separate from `all_expected_paths_verified`.
- **Stale files.** qedlift always writes into a fresh temporary directory.
  Modules are copied into `--out-dir` only when the verdict passes, so an old
  file cannot make a failed run pass.
- A v3 build of qedsvm's vault program is a new fixture. The opt-in
  `discharge_e2e` gate expects `verified` for `total += 1` and `rejected` for
  `total += 2` against it.

## 5. Auditor: sBPF v3 bug classes (#433)

Three new catalog entries. Each is backed by a fixture program built as V0 and
as v3 from the same source and run in Mollusk:

- `sbpf_v3_low_address_read`: under v3, a read through a null pointer does not
  fault. It reads `.rodata`.
- `sbpf_v3_stack_overrun_no_fault`: a stack overrun corrupts the callee's
  locals instead of faulting. This also applies to V0 once SIMD-0460 is active.
- `sbpf_v3_unresolved_syscall`: on platform-tools v1.56 and later, a
  hand-declared syscall is a link error. The entry keeps a disassembly check
  only for a `.so` of unknown origin.

`known-non-findings.md` adds two entries so that these classes are not reported
against programs where they cannot apply.

## 6. Build and test gates

- The runtime gates (`runtime_journey` on Mollusk, `parallax_repro_gate` on
  LiteSVM, and the live Crucible domain-boundary gate) build with
  `--arch v3` and assert that every `.so` is v3. They require cargo-build-sbf
  4.2.0 or later and platform-tools v1.56 or later. CI pins the Solana CLI to
  v4.3.0.
- Checks confirmed that the current Anchor (`anchor-lang` 0.32.1), Quasar, and
  Pinocchio (0.8.4) pins build and run as v3 with every syscall resolved, so
  generated programs need no dependency change (#424, #425, #428).
- A flaky discharge lock test is fixed (#441). A child process spawned by
  another test thread could hold the `flock` lock briefly after release.

## Upgrading

No `.qedspec` migration is required. To make the target explicit, add
`pragma sbpf_version = v3` to sBPF specs.

- Regenerating an sBPF module that records no version moves it to v3, and its
  `RODATA_*` addresses change. Re-run `lake build` on its proofs. To stay on V0
  for now, pass `--sbpf-version v0` or declare `pragma sbpf_version = v0`. Both
  print a deprecation warning.
- Build programs with `cargo build-sbf --arch v3` (or `anchor build -- --arch v3`)
  and add `readiness --so` to the deploy check.
- `discharge` needs a qedlift built from qedsvm v0.13.0 or later for parameter
  deltas and transition path kinds.
- `lean_solana` and the bundled examples now pin qedsvm v0.13.0. A Lean project
  with its own qedsvm pin should move it to v0.13.0 for v3 semantics and JMP32.
