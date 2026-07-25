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

# JSON for CI
$QEDGEN readiness --idl target/idl/my_program.json --json

# Print the rule catalog and exit
$QEDGEN readiness --list-rules
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--idl` | Path | required | Anchor IDL JSON (typically `target/idl/<program>.json`) |
| `--quasar` | bool | auto | Treat `--idl` as a Quasar-emitted IDL rather than an Anchor IDL. Auto-detected when a `Quasar.toml` (and no shadowing `Anchor.toml`) lives in the current working directory; pass explicitly to force Quasar mode from elsewhere. |
| `--list-rules` | bool | false | Print the catalog of P-rules applied and exit |
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
| `--quasar` | bool | auto | Treat both IDLs as Quasar-emitted rather than Anchor. Auto-detected from `Quasar.toml`; the flag forces Quasar mode when running from elsewhere. Mixed-framework diffs (Anchor old vs Quasar new) are out of scope. |
| `--list-rules` | bool | false | Print the catalog of R-rules applied and exit |
| `--json` | bool | false | Machine-readable output |

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
then discharge it against the compiled `.so` via a built `qedlift`. Reports
whether the handler's effect is proven against the bytes. No meaning crosses the
boundary — `discharge` reads only qedlift's exit status and whether it emitted a
sorry-free proof.

```bash
$QEDGEN discharge --spec vault.qedspec --handler increment \
  --so vault.so --idl vault.codama.json --qedlift /path/to/qedlift \
  --out-dir formal_verification/discharge
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | required | Path to the `.qedspec` |
| `--handler` | String | required | Handler to discharge (single-field `+= <int literal>` effect) |
| `--account` | String | first account type / program name | Account name — use the IDL account name so qedlift resolves offsets |
| `--so` | Path | required | Compiled program to discharge against |
| `--idl` | Path | required | Codama IDL (`.json`) supplying the account shape (offsets) |
| `--qedlift` | Path | required | Built qedsvm `qedlift` binary (built with `--features qedrecover`) |
| `--module` | String | `<Account><Handler>` | Lean module name for the emitted proof |
| `--out-dir` | Path | temp dir (artifacts discarded) | Persist `<Module>TracedLifted.lean` + `<Module>Refinement.lean` into this directory |
| `--transition` | flag | off | Whole-transition mode (qedsvm v0.9.0, #40): lift **every** path from discovered `<stem>_<path>.pcs` traces beside the `.so`; emits per-path `*_transition_path` / `*_transition_fault` corollaries + the one bundle theorem (`<StemPascal>Transition.lean`) covering success and abort paths. Requires `--out-dir` and ≥ 2 traces |

Whole-transition example:

```bash
$QEDGEN discharge --spec counter.qedspec --handler increment \
  --so counter.so --qedlift /path/to/qedlift \
  --transition --out-dir formal_verification/discharge
```

