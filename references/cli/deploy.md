# CLI: deploy safety

`readiness`, `check-upgrade`, `descriptor`, `discharge`.

Part of the [CLI Reference](../cli.md).

## Mainnet readiness

QEDGen embeds the ratchet rule engine for upgrade-safety lints over
Anchor IDLs — separate from the spec/proof gates above. `readiness`
runs the **P-rule preflight** (one IDL); `check-upgrade` runs the
**R-rule diff** (old vs new IDL). Both exit `0` for additive/safe,
`1` for breaking, `2` for unsafe. Both are linked in as a library —
no standalone `ratchet` CLI on PATH after `install.sh` /
`npx skills add`; use these wrappers instead.

### `readiness`
Lint one Anchor IDL for mainnet-readiness before first deploy. Catches
upgrade risks before the program ever ships: missing `version: u8`
prefix, no `_reserved` trailing padding, unpinned discriminators, name
collisions, writable accounts with no signer.

```bash
# Standard preflight
$QEDGEN readiness --idl target/idl/my_program.json

# Also check that the built program is sBPF v3
$QEDGEN readiness --idl target/idl/my_program.json --so target/deploy/my_program.so

# A program without an IDL (Pinocchio, native, sBPF assembly)
$QEDGEN readiness --so target/deploy/my_program.so

# JSON for CI
$QEDGEN readiness --idl target/idl/my_program.json --json

# Print the rule catalog and exit
$QEDGEN readiness --list-rules
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--idl` | Path | required (unless `--so` or `--list-rules`) | Anchor IDL JSON (typically `target/idl/<program>.json`) |
| `--so` | Path | - | Built program. Reports `QED002` when it is older than sBPF v3 (see below) |
| `--root` | Path | - | Project root for source-vs-IDL reconciliation. Needs `--idl` |
| `--unsafe` | String | - | Acknowledge an unsafe finding (repeatable), for example `--unsafe allow-pre-v3-sbpf` |
| `--quasar` | bool | auto | Treat `--idl` as a Quasar-emitted IDL rather than an Anchor IDL. Auto-detected when a `Quasar.toml` (and no shadowing `Anchor.toml`) lives in the current working directory; pass explicitly to force Quasar mode from elsewhere. |
| `--list-rules` | bool | false | Print the catalog of P-rules and QED rules applied and exit |
| `--json` | bool | false | Machine-readable output |

### `check-upgrade`
Diff an old vs new Anchor IDL and flag every upgrade-unsafe change.
Catches the failure modes `solana program upgrade` won't — field
reorders, discriminator changes, orphaned accounts, PDA seed drift,
signer/writable tightening.

```bash
# Standard upgrade diff
$QEDGEN check-upgrade --old old.json --new new.json

# Acknowledge a specific finding so it reports as Additive
$QEDGEN check-upgrade --old old.json --new new.json \
  --unsafe R007=ProgramId

# Declare a migration / realloc was added in source
$QEDGEN check-upgrade --old old.json --new new.json \
  --migrated-account TreasuryV2 --realloc-account UserConfig

# Also check that the new build is sBPF v3
$QEDGEN check-upgrade --old old.json --new new.json \
  --new-so target/deploy/my_program.so

# Print the rule catalog and exit
$QEDGEN check-upgrade --list-rules
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--old` | Path | required (unless `--list-rules`) | Baseline IDL (the one on-chain today) |
| `--new` | Path | required (unless `--list-rules`) | Candidate IDL (the one the upgrade would ship) |
| `--unsafe` | String | - | Acknowledge a specific finding so it reports as Additive (repeatable). Pass `--list-rules` to see the full flag catalog. |
| `--migrated-account` | String | - | Declare an account as having a migration in source; demotes R003/R004 findings for that account to Additive (repeatable) |
| `--realloc-account` | String | - | Declare an account as having `realloc = ...` in source; demotes R005 for that account to Additive (repeatable) |
| `--new-so` | Path | - | Built program the upgrade would ship. Reports `QED002` when it is older than sBPF v3 (see below) |
| `--quasar` | bool | auto | Treat both IDLs as Quasar-emitted rather than Anchor. Auto-detected from `Quasar.toml`; the flag forces Quasar mode when running from elsewhere. Mixed-framework diffs (Anchor old vs Quasar new) are out of scope. |
| `--list-rules` | bool | false | Print the catalog of R-rules and QED rules applied and exit |
| `--json` | bool | false | Machine-readable output |

### `QED002`: program older than sBPF v3

SIMD-0500 (planned for Agave 4.4) rejects deploys, upgrades, and
finalizations of programs older than sBPF v3. Programs already deployed keep
running, but they cannot be upgraded unless the new build is v3.
`readiness --so` and `check-upgrade --new-so` read the ELF header of the built
program. When `e_flags` is below 3, they report `QED002`
(`sbpf-version-below-v3`) as unsafe (exit `2`). A newer version passes. A
missing file, a non-ELF file, a truncated file, or an ELF for another machine
is an error (exit `3`), never a pass. The reader checks the ELF header and
that the tables and segments it points to fit in the file.

Rebuild with `cargo build-sbf --arch v3` (cargo-build-sbf 4.2.0+,
platform-tools v1.56+) or `sbpf build -a v3`. For a V0 program that is
deployed and will never be upgraded, acknowledge the `readiness` finding with
`--unsafe allow-pre-v3-sbpf`. It then reports as additive. `check-upgrade
--new-so` offers no acknowledgement: its candidate is the upgrade, and the
cluster rejects it.

Rebuilding a program as v3 changes its binary. Callers that pin it with
`upstream { binary_hash }` see pin drift, which `verify --check-upstream`
reports. Update the pin after the v3 upgrade is deployed.

## Discharge (experimental — the qedgen ↔ qedsvm seam)

Hands a name-level refinement obligation to qedsvm's `qedlift`, which proves it
against the decoded program bytes (field offsets resolved from the IDL on the
qedsvm side). Today's scope is a single-field constant-increment handler
(`field += <int literal>`); the bundled CPI-callee `ensures` and the sBPF bridge
are otherwise axiomatized against a `binary_hash` pin. See
[`docs/design/qedsvm-discharge.md`](../../docs/design/qedsvm-discharge.md).

### `descriptor`
Emit the name-level refinement descriptor (JSON, to stdout) — the producer half
of the seam. Carries only semantics (which named field a handler mutates, by how
much); offsets are resolved IDL-side. Schema: qedsvm `docs/REFINEMENT_DESCRIPTOR.md`.

```bash
$QEDGEN descriptor --spec vault.qedspec --handler increment
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | required | Path to the `.qedspec` |
| `--handler` | String | required | Handler to inspect (single-field `+= <int literal>` effect) |
| `--account` | String | first account type / program name | Account name for the descriptor — use the IDL account name so qedsvm resolves offsets |

### `discharge`
The one-command driver over the seam: build the descriptor from the `.qedspec`,
discharge it against the compiled `.so` via a built `qedlift`, then type-check the
emitted modules with Lean. `discharge` reads qedlift's structured
`refinement outcome` line (older qedlift builds fall back to the exit status and
the emitted files). It never reads files left in `--out-dir` by an earlier run.

| Verdict | Meaning | Exit |
|---|---|---|
| `verified` | qedlift emitted the refinement and Lean accepted it | 0 |
| `emitted` | qedlift emitted the refinement, but no Lean check ran (no Lake project) | 0 |
| `rejected` | The obligation conflicts with the bytes or layout (typed `reason`) | 1 |
| `unsupported` | qedlift cannot bind the obligation (typed `reason`) | 1 |
| `model_only` | qedlift lifted the program but did not act on the descriptor | 1 |
| `failed` | qedlift failed, or Lean rejected the modules or found `sorry` | 1 |

CI that needs a proof should gate on `"verdict": "verified"` in the `--json` report.
A qedlift that cannot be launched, or a failed copy into `--out-dir`, is also a
`failed` report (`qedlift_not_runnable`, `persist_failed`), so `--json` always
prints one.

```bash
$QEDGEN discharge --spec vault.qedspec --handler increment \
  --so vault.so --idl vault.codama.json --qedlift /path/to/qedlift \
  --out-dir formal_verification
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | required | Path to the `.qedspec` |
| `--handler` | String | required | Handler to discharge (single-field `+= <int literal>` effect) |
| `--account` | String | first account type / program name | Account name — use the IDL account name so qedlift resolves offsets |
| `--so` | Path | required | Compiled program to discharge against |
| `--idl` | Path | required | Codama IDL (`.json`) supplying the account shape (offsets) |
| `--qedlift` | Path | required | Built qedsvm `qedlift` binary (`cargo build -p qedlift --bin qedlift` in qedsvm's `qedsvm-rs/`) |
| `--module` | String | `<Account><Handler>` | Lean module name for the emitted proof |
| `--out-dir` | Path | temp dir (artifacts discarded) | Persist `<Module>TracedLifted.lean` + `<Module>Refinement.lean` into `<out-dir>/Generated/`, so they import as `Generated.<Module>Refinement` from a Lake source root at `<out-dir>`. Written only when the verdict is `verified` or `emitted`. With `--transition`, every path module and the bundle go there |
| `--transition` | flag | off | Whole-transition mode (qedsvm #40): lift every `<stem>_<label>.pcs` trace beside the `.so` into one module per path plus the bundle theorem (`<StemPascal>Transition.lean`). Reports one row per path; see below. Needs ≥ 2 traces |
| `--lean-project` | Path | nearest Lake project at or above `--out-dir` | Lake project used to type-check the emitted modules. It must `require qedsvm` and be built. With neither this nor a Lake project above `--out-dir`, no Lean check runs and the verdict is at most `emitted` |
| `--json` | bool | false | Machine-readable report. Same verdict as the human report |

Whole-transition example:

```bash
# traces beside the binary: guarded_counter_success.pcs, guarded_counter_zero_amount.pcs
$QEDGEN discharge --spec guarded.qedspec --handler credit \
  --so guarded_counter.so --idl guarded.codama.json --qedlift /path/to/qedlift \
  --transition --out-dir formal_verification
```

#### Whole-transition verdicts

`--transition` reports one row per path and one overall verdict:

- **Expected paths come from the spec.** The handler expects `success`, plus
  one rejection path per `requires ... else E`, labeled with the snake_case of
  `E` (`else ZeroAmount` expects `<stem>_zero_amount.pcs`). Implicit
  checked-arithmetic overflow paths are not expected. A trace the spec does not
  name is still lifted and listed as "not in spec".
- **Path kinds come from qedlift.** A path is a `return` (with its exit code) or
  a `fault` (a VM `abort` or `access_violation`). A spec rejection is usually a
  return with a non-zero code and no write to the tracked field. The success
  path must return 0. A spec rejection path must return a non-zero code, leave
  the tracked field alone, and not end in a VM fault. A path with a missing exit
  code or tracked-write flag is unconfirmed, and a path whose module qedlift did
  not write fails. qedlift reports kinds in a `transition outcome` line (requested in
  QEDGen/qedsvm#70). Without that line the kinds are unknown and the verdict is
  at most `incomplete` (`no_path_outcomes`).
- **Verdict.** `verified` needs every expected path traced, every path kind
  confirmed, and Lean accepting every emitted module. An expected path with no
  trace is `incomplete` (`expected_path_missing`), never a success. A refused
  path, a contradicting kind, a `sorry`, or a failed Lean check is `failed`.
- **Coverage.** Trace coverage is not whole-CFG coverage. The report separates
  `all_discovered_paths_verified` from `all_expected_paths_verified`, and records
  the `program_sha256` of the binary the modules were lifted from.
- **Stale files.** qedlift writes into a fresh temp dir. Modules are published
  into `<out-dir>/Generated/` only when the verdict passes, as one set: they are
  staged first, so a failed copy publishes nothing. A manifest
  (`<Bundle>.qedgen-modules`) records the set, so a path dropped on a rerun is
  removed. Proofs qedgen did not record are never touched.
