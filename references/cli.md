# CLI Reference

All commands are run via the wrapper: `$QEDGEN <command> [flags]`

## Require-git guard

`qedgen codegen`, `qedgen check`, and `qedgen reconcile` all require the
current directory to be inside a git repository (they walk upward looking for
`.git`). If no repo is found, the command prints

```
qedgen requires a git repo — run `git init` first
```

and exits 1. QEDGen relies on git for safe regeneration (three-way merge of
generated artifacts), proof preservation, and drift reconciliation; running
outside a repo would silently discard user edits to `src/instructions/*.rs`
and `Proofs.lean`.

## Project setup

### `init`
Scaffold a new formal verification project. Creates `.qed/` project state
directory and pins the spec path in `.qed/config.json` so subsequent
commands don't need `--spec`.

```bash
$QEDGEN init --name escrow   --spec escrow.qedspec
$QEDGEN init --name tree     --spec tree.qedspec --asm src/tree.s
$QEDGEN init --name engine   --spec engine.qedspec --mathlib
$QEDGEN init --name counter  --spec counter.qedspec --target anchor
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--name` | String | required | Project name (alphanumeric + underscores) |
| `--spec` | Path | - | Spec path (file or directory) — written into `.qed/config.json` so `check`/`codegen` can resolve it automatically |
| `--asm` | Path | - | sBPF assembly source (runs asm2lean automatically) |
| `--mathlib` | bool | false | Include Mathlib dependency |
| `--target` | enum | - | Also generate the program crate + Kani harnesses for the named framework target. Values: `anchor` (Anchor-compatible Rust), `quasar` (Blueshift Quasar — `#![no_std]`, explicit discriminators, `Ctx<X>`), `pinocchio` (Pinocchio `#![no_std]` — `entrypoint!` + byte-discriminant dispatch, zeropod zero-copy state, `&AccountInfo` account structs with `.handler()` methods). Requires `--spec`. Omit to skip program scaffolding entirely. |
| `--output-dir` | Path | `./formal_verification` | Output directory |

The written `.qed/config.json`:

```json
{
  "name": "escrow",
  "spec": "escrow.qedspec",
  "interfaces_dir": ".qed/interfaces"
}
```

### `setup`
Set up the global validation workspace at `~/.qedgen/workspace/`.

```bash
$QEDGEN setup
$QEDGEN setup --mathlib
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--workspace` | Path | `~/.qedgen/workspace/` | Override workspace path |
| `--mathlib` | bool | false | Fetch Mathlib cache (~8GB) |

### `asm2lean`
Transpile sBPF assembly to Lean 4 program module.

```bash
$QEDGEN asm2lean --input src/program.s --output formal_verification/Prog.lean
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--input` | Path | required | sBPF assembly source file |
| `--output` | Path | required | Output Lean 4 file |
| `--namespace` | String | derived from filename | Lean namespace |

## Spec and validation

### `interface`
Generate a Tier-0 interface `.qedspec` from an Anchor IDL. Shape only —
program ID, discriminator, accounts, argument types. No `requires`/
`ensures`/`effect` (those require semantic understanding the IDL does not
carry). The `upstream` block is left as a TODO stub for the author to fill
in after running QEDGen harnesses against the deployed program.

See `docs/design/spec-composition.md` §2 for the CPI tier model.

```bash
# Print to stdout
$QEDGEN interface --idl target/idl/jupiter.json

# Write to an explicit path
$QEDGEN interface --idl target/idl/jupiter.json --out interfaces/jupiter.qedspec

# Vendor into .qed/interfaces/<program>.qedspec (canonical library location,
# resolved via the nearest .qed/config.json)
$QEDGEN interface --idl target/idl/jupiter.json --vendor
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--idl` | Path | required | Anchor IDL JSON file |
| `--out` | Path | - | Output path (default: stdout). Conflicts with `--vendor`. |
| `--vendor` | bool | false | Drop into `.qed/interfaces/<program>.qedspec`. Requires a discoverable `.qed/` ancestor. |

### `spec`
**DEPRECATED (slated for v3.0 removal).** The IDL is now an evidence
*source* for `qedgen probe` — the hypothesizer consumes IDL signer flags
and `has_one` relations directly and offers confirmable clauses instead
of a TODO shell. Remains functional in v2.x with a runtime warning.

Scaffold a `.qedspec` from an Anchor IDL JSON. (For Tier-0 interface
scaffolding from an IDL — program ID + handler signatures only — prefer
`interface`, which is more focused.) v2.10 dropped the SPEC.md
generators that previously lived behind `--from-spec` and the default
`--format md` path; `.qedspec` is QEDGen's front-door artifact and
parallel Markdown duplicates were drifting in practice.

```bash
$QEDGEN spec --idl target/idl/program.json
$QEDGEN spec --idl target/idl/program.json --output-dir ./formal_verification
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--idl` | Path | required | Anchor IDL JSON file |
| `--output-dir` | Path | `./formal_verification` | Output directory; `<idl-stem>.qedspec` is written inside |

### `adapt`
**DEPRECATED (slated for v3.0 removal).** `adapt` bundled two unrelated
jobs and both now have honest homes: scaffold mode is subsumed by
`qedgen probe --emit-spec-candidates --audit-dir` (elicitation-first —
confirmable, evidence-anchored hypotheses instead of TODO stubs, and the
same skeleton written as a byproduct), and attribute mode is
[`stamp`](#stamp) (same emission plus the recorded-verification gate).
Both modes remain functional in v2.x with a runtime warning.

Brownfield adapter for existing Anchor programs. Two modes:

- **Scaffold mode** (`--program <c>` only): parses `<c>/src/lib.rs`, finds
  the `#[program]` mod, walks each instruction to its handler body via
  forwarder classification, and emits a parseable `.qedspec` skeleton with
  TODO markers for state machine / requires / effect bodies.
- **Attribute mode** (`--program <c> --spec <s>`): given a filled-in spec,
  emits one `#[qed(verified, spec = ..., handler = ..., hash = ...,
  spec_hash = ...[, accounts = ..., accounts_file = ..., accounts_hash = ...])]`
  line per handler. Paste each above its handler `pub fn`; future body or
  spec edits trip `compile_error!` until you re-run `adapt --spec`.

Forwarder shapes the classifier handles end-to-end: Inline, free-fn
(`module::fn(args)` plus the two-stmt `<call>?; Ok(())` and `?`-tail
shapes), type-associated (`Type::method(ctx, args)` PascalCase prefix),
accounts-method (`ctx.accounts.method(args)`). Custom dispatcher patterns
fall through to `Unrecognized` — use `--handler` to point them at the real
implementation.

```bash
# Scaffold a starter spec from existing Anchor source
$QEDGEN adapt --program ./programs/my_program

# Write to disk instead of stdout
$QEDGEN adapt --program ./programs/my_program --out my_program.qedspec

# Emit #[qed] attributes for an existing spec
$QEDGEN adapt --program ./programs/my_program --spec my_program.qedspec

# Custom dispatcher handlers — point each at its actual implementation
$QEDGEN adapt --program ./programs/my_program \
  --handler dispatch=instructions::dispatch::handler \
  --handler ix2=instructions::ix2::run
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--program` | Path | required | Program crate (directory holding `Cargo.toml`, with `src/lib.rs` inside) |
| `--spec` | Path | - | Existing `.qedspec`. Switches to attribute-emit mode |
| `--out` | Path | stdout | Output path. In scaffold mode writes a `.qedspec`; in attribute mode writes a `// === handler … ===` report |
| `--handler` | `NAME=PATH` | - | Manually point an unrecognized handler at its actual implementation. Format: `<handler>=<rust_path>` where path is `module::sub::function` or just `function`. Repeatable. Wins over the classifier's choice for any outcome (Inline / FreeFn / Method / Unrecognized) |

### `stamp`
v2.44 — stamp `#[qed(verified, …)]` drift attributes for an
already-verified spec: the post-verification half of the old `adapt`,
under a name that says what it does. Emits one attribute per handler
(body hash + spec-block hash, plus the Accounts-struct seal when the
`Context<X>` struct is found) to paste above each `pub fn`; the
`qedgen-macros` proc macro recomputes both at compile time and fires
`compile_error!` on drift. Anchor-only (it round-trips through the
Anchor project parser to locate each handler body).

`stamp` runs **after** verification and proves nothing itself. Its one
new behavior over `adapt --spec` is the gate: it requires recorded
implementation-verified evidence — `.qed/verify-evidence.json`, written
by every `qedgen verify` run — with (a) a `spec_hash` matching the spec
being stamped byte-for-byte, (b) a `program_hash` matching the current
program source tree, and (c) at least one passing **implementation-bound**
backend (miri or a `kani_impl*.rs` harness). Checking/model-tested results
and bug-oriented `--probe-repros` are not eligible; an edited spec or
program invalidates the evidence until re-verified. So
the division of labor is: a source-bound backend establishes the
implementation claim, `stamp` freezes it, the compiler guards the
freeze.

```bash
# 1. verify with an implementation-bound backend (records evidence)
$QEDGEN verify --spec my_program.qedspec --program programs/my_program --kani --kani-path programs/my_program/src/kani_impl.rs

# 2. stamp — refuses unless the recorded evidence matches and is impl-bound
$QEDGEN stamp --program programs/my_program --spec my_program.qedspec
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--program` | Path | required | Program crate (directory holding `Cargo.toml`, with `src/lib.rs` inside) |
| `--spec` | Path | required | The verified `.qedspec` to stamp against |
| `--out` | Path | stdout | Output path for the `// === handler … ===` attribute report |
| `--handler` | `NAME=PATH` | - | Manually point an unrecognized handler at its implementation (same semantics as `adapt`) |
| `--evidence` | Path | `<spec_dir>/.qed/verify-evidence.json` | Override the verification-evidence path |

### `check`
Validate a spec — lint, coverage, drift, and verification report. Default
(no flags) runs lint + coverage.

Requires a git repo (see [Require-git guard](#require-git-guard)).

`--spec` is optional — when omitted, walks up from the current directory to
the nearest `.qed/config.json` and uses its `spec` field. Explicit `--spec`
overrides.

```bash
# From inside a project initialized with `qedgen init --spec ...`
$QEDGEN check
$QEDGEN check --json

# Explicit spec path
$QEDGEN check --spec my_program.qedspec

# Coverage matrix
$QEDGEN check --coverage

# Verification report
$QEDGEN check --explain
$QEDGEN check --spec my_program.qedspec --explain --output report.md

# Drift detection
$QEDGEN check --spec my_program.qedspec --drift programs/src/
$QEDGEN check --spec my_program.qedspec --drift programs/src/ --deep
$QEDGEN check --spec my_program.qedspec --drift programs/src/ --update-hashes

# Unified code + kani drift
$QEDGEN check --spec my_program.qedspec --code programs/my_program/ --kani programs/tests/kani.rs

# sBPF verification (hash check + lake build)
$QEDGEN check --spec my_program.qedspec --asm src/program.s

# Anchor project cross-check (spec ↔ #[program] mod handler set)
$QEDGEN check --spec my_program.qedspec --anchor-project programs/my_program/

# CI freeze gate: refuse to update qed.lock and refuse network fetches.
# v2.26 Slice 4c — `--frozen` also diffs each pinned binary_hash against
# the on-chain .so. Mismatches surface as P2 warnings (exit 0); pair with
# `--strict` to escalate to CRIT and fail the check.
$QEDGEN check --spec my_program.qedspec --frozen
$QEDGEN check --spec my_program.qedspec --frozen --strict
$QEDGEN check --spec my_program.qedspec --frozen --no-cache

# Bundled example drift gate
$QEDGEN check --regen-drift
$QEDGEN check --regen-drift --examples-root examples/rust
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | optional | Spec file or directory. Defaults to `.qed/config.json spec` |
| `--proofs` | Path | `./formal_verification` | Proofs directory |
| `--coverage` | bool | false | Show operation × property matrix (spec coverage) plus the per-backend obligation rollup (backend coverage, #332): for each of kani / lean / proptest, how many requested obligations are `emitted` vs `unsupported(reason)` vs `failed`, recomputed in memory from the current spec. `--json` adds a `backend_coverage` key next to the existing matrix fields. |
| `--explain` | bool | false | Generate Markdown verification report |
| `--output` | Path | stdout | Output file for --explain |
| `--drift` | Path | - | Rust source path for #[qed(verified)] drift detection |
| `--update-hashes` | bool | false | Auto-stamp hashes in source files |
| `--deep` | bool | false | Transitive drift detection (check callees) |
| `--code` | Path | - | Generated program source dir (code drift detection) |
| `--kani` | Path | - | Kani harness file (Kani drift detection) |
| `--asm` | Path | - | sBPF assembly source (hash check + lake build) |
| `--anchor-project` | Path | - | Anchor program crate (`Cargo.toml` + `src/lib.rs`). Cross-checks the spec's `handler` set against the `#[program]` mod's instruction set, plus an effect-coverage lint per resolved handler body. CI gate. |
| `--frozen` | bool | false | Refuse to update `qed.lock`; error if the on-disk lock is stale or missing. Used in CI to detect un-bumped imports. |
| `--strict` | bool | false | Escalate `--frozen` upstream binary-hash mismatches AND v2.27 Track D1 proof_hash drift from P2 warning to CRIT (gates exit). Use in release-blocking CI; default `--frozen` stays warning-only. Requires `--frozen`. |
| `--no-cache` | bool | false | Force-refresh the github source cache for every imported dep. Wipes `~/.qedgen/cache/github/<org>/<repo>/<kind>/<ref>/` and re-clones. |
| `--regen-drift` | bool | false | Regenerate bundled examples into temporary directories and fail if committed generated support code, harnesses, or `Spec.lean` drift. Also fails when an example has `.qed/` state or generated artifacts but no `qed.toml`. |
| `--examples-root` | Path | `examples/rust` | Example root scanned by `--regen-drift` |
| `--write` | bool | false | With `--regen-drift`, also write the regenerated content into the repo so committed example outputs match current codegen. Useful for rebasing PRs across codegen-touching releases. Never touches user-owned files (handler bodies, Spec.lean proofs) — only the codegen-owned set `--regen-drift` already compares. |
| `--json` | bool | false | Machine-readable output |

Lints fired by `check` include `[shape_only_cpi]` for `call
Interface.handler(...)` sites whose target declares no `ensures` —
making the visible gap between "my Rust compiles" and "my program is
verified" explicit.

### `reconcile`
Emit a unified drift report comparing a `.qedspec` against both its Rust
handlers and its Lean proofs. Report-only — never modifies files.

Requires a git repo (see [Require-git guard](#require-git-guard)).

```bash
# Default paths: --code programs/ --proofs formal_verification/
$QEDGEN reconcile --spec my_program.qedspec

# Custom paths
$QEDGEN reconcile --spec my_program.qedspec --code programs/escrow/ --proofs verification/

# Machine-readable (for CI / agent consumption)
$QEDGEN reconcile --spec my_program.qedspec --json
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | required | Spec file (.qedspec) |
| `--code` | Path | `programs/` | Root directory scanned for `#[qed(verified, ...)]` attributes (recursive) |
| `--proofs` | Path | `formal_verification/` | Directory containing `Proofs.lean` |
| `--json` | bool | false | Emit JSON instead of the human-readable report |

What it reports:

- **Rust handler drift** — handlers where the computed body hash or the
  recomputed spec-handler hash no longer matches the stamped `#[qed(...)]`
  attribute, or where the attribute references a handler that no longer
  exists in the spec.
- **Lean orphans** — `*_preserved_by_*` theorems in `Proofs.lean` that don't
  correspond to any current (property, handler) pair in the spec.
- **Lean missing** — (property, handler) pairs required by `preserved_by`
  clauses in the spec for which no `*_preserved_by_*` theorem exists in
  `Proofs.lean`.
- **Cross-spec warnings** — Rust files with `#[qed]` attributes pointing at a
  different `.qedspec` than the one passed on the CLI.

Exit codes:

- `0` — no drift; spec, code, and proofs are in sync
- `1` — drift detected (any of the categories above)

Typical use:

- After editing a `.qedspec`: `qedgen reconcile --spec x.qedspec` shows
  exactly which handlers need a hash refresh and which proofs are now
  orphans or missing.
- As a CI gate: `qedgen reconcile --spec x.qedspec --json | tee drift.json`
  plus `test $? -eq 0` ensures drift blocks merges.
- As the first step of the agent-driven reconciliation loop described in
  SKILL.md **Step 4d**.

### `verify`
Run the generated harnesses against the implementation. `check` validates
the spec; `verify` validates the code the spec produced. With no backend
flags, runs every backend whose artifact is present on disk
(`./programs/tests/proptest.rs`, `./programs/tests/kani.rs`,
`./formal_verification/`). Use `--proptest` / `--kani` / `--lean` to
target one backend.

v2.44 — every run also records its evidence to
`<spec_dir>/.qed/verify-evidence.json` (spec hash, optional program-source
hash, per-backend status, and whether an **implementation-bound** backend
passed: miri, or Kani only when the `--kani-path` file is a
`kani_impl*.rs` harness). Plain proptest/Kani/Lean exercise the spec model,
while `--probe-repros` confirms bug findings; neither category counts. This
record is what [`stamp`](#stamp) gates
on; it is written on pass and fail alike (a failed run is still evidence
of what ran) and a failed write never turns a green verify red.

```bash
# Auto-detect: every backend whose artifact exists on disk
$QEDGEN verify --spec my_program.qedspec

# Targeted
$QEDGEN verify --spec my_program.qedspec --proptest
$QEDGEN verify --spec my_program.qedspec --kani
$QEDGEN verify --spec my_program.qedspec --lean

# CI gating
$QEDGEN verify --spec my_program.qedspec --fail-fast --json

# Diff every imported library's pinned upstream_binary_hash against
# the on-chain .so (requires `solana` CLI in PATH). v2.26 Slice 4c —
# mismatched pins surface as CRIT findings and gate exit. Auto-on when
# qed.lock declares any pinned `binary_hash`.
$QEDGEN verify --spec my_program.qedspec --check-upstream
$QEDGEN verify --spec my_program.qedspec --check-upstream --rpc-url https://api.devnet.solana.com
$QEDGEN verify --spec my_program.qedspec --check-upstream --offline
# Offline development — suppress the upstream check; mismatches demote
# to Info and verify exits zero. Do NOT use in CI.
$QEDGEN verify --spec my_program.qedspec --check-upstream --upstream-stale-ok
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | required | Spec file (`.qedspec`) |
| `--program` | Path | none | Program crate to hash into implementation-bound evidence; required before `stamp` can consume that evidence |
| `--proptest` | bool | false | Run proptest harnesses (`cargo test --release`) |
| `--proptest-path` | Path | `./programs/tests/proptest.rs` | Proptest harness file |
| `--kani` | bool | false | Run Kani BMC harnesses (`cargo kani --tests`) |
| `--kani-path` | Path | `./programs/tests/kani.rs` | Kani harness file |
| `--lean` | bool | false | Run Lean proofs (`lake build`) |
| `--lean-dir` | Path | `./formal_verification` | Lean project directory |
| `--miri` | bool | false | Run Pinocchio Miri reproducers under `.qed/probes/pinocchio/*/repro_miri.rs` via `cargo +nightly miri test`. UB / aliasing / overflow diagnostics surface as findings; dual-execution divergence against Mollusk repros surfaces as Critical. |
| `--fail-fast` | bool | false | Stop on the first failing backend |
| `--json` | bool | false | Machine-readable output for CI |
| `--check-upstream` | bool | false | Diff each pinned `upstream_binary_hash` against the on-chain `.so` via `solana program dump`. Skips deps without a pinned hash. Non-zero exit on any mismatch. |
| `--rpc-url` | String | Solana CLI default | Override RPC endpoint passed to `solana program dump --url <rpc>` |
| `--offline` | bool | false | Refuse to reach the network. Any dep that would require an on-chain fetch reports as Error. CI-gate friendly. |
| `--upstream-stale-ok` | bool | false | Suppress the upstream binary-hash check even when the lock declares pinned hashes. Mismatches demote to Info; verify exits zero. Offline-dev only — do not use in CI. Pairs with the auto-on behavior of `--check-upstream`. |
| `--probe-repros` | bool | false | Discover and run probe reproducers under `<project>/target/qedgen-repros/`, including agent-authored audit repros and mechanically generated category repros. Reports `Fired`, `Silent`, or `BuildError` per repro; emits `note: no repros found` only when the directory contains no runnable reproducers. |
| `--crucible` | u64 | none | Run the coverage-guided fuzz engine for the given wall-clock seconds. Thin alias over `probe --fuzz` — folds findings into the BackendReport so they render through the same named-trace human surface as Kani / proptest. |
| `--crucible-harness-dir` | Path | `./fuzz/<prog>/` | Harness directory for `--crucible`. |
| `--crucible-no-smoke` | bool | false | Skip the 30s smoke pre-flight. |
| `--crucible-stateful` | bool | false | Stateful action-chain mode for `--crucible`. |
| `--recursive` | bool | false | v2.27 Track D3 — DFS-walk the transitive proof-package closure (deduped by path) and run `lake build` per layer. Per-layer PASS/FAIL is reported; failed layers print the first ~10 lines of stderr/stdout. Exits non-zero on any layer failure; emits "every imported proof package built clean" when all pass. No-op success when the spec imports nothing with `verified = true` in `qed.lock`. |
| `--require-verified` | bool | false | v2.27 Track D2 — exits non-zero before any backend dispatches if any imported Tier-1+ interface (binary_hash + `ensures`) did NOT ship a `.qed/proofs/<Iface>.lean + lakefile.lean` package alongside. Tier-0 (no ensures) and sentinel-pinned natives (all-zero binary_hash) are exempt. Default-off in v2.27 because the bundled stdlib still ships Stance 1 for `import System from "system"` (no bundled proof package for Pubkey-param handlers). |
| `--strict` | bool | false | #332 — recompute the reconciled backend-obligation manifest (kani / lean / proptest, in memory) and exit 1 on any `unsupported` or `failed` entry. A passing strict verify means no requested obligation was silently dropped by a backend. Known capability gaps (multi-account file-level features #324, ADT Kani parity #326, pubkey Lean clauses #328, multi-account ghosts #331) fail affected specs by design. |

### `probe`
Probe a `.qedspec` for category-coverage gaps (spec-aware mode) or walk a
brownfield project root and emit a per-handler work list (spec-less /
`--bootstrap` / `--program` mode). Output is always a schema-v3 JSON envelope;
`qedgen probe` has no `--json` flag. Spec-aware runs may contain both
`candidates[]` (unconfirmed investigation leads) and `findings[]`
(reproducer-backed results). Spec-less runs also report `runtime`, `handlers`,
and `applicable_categories`.

In schema v3, `engine_runs[]` records per-engine status (`passed | partial
| blocked | failed | skipped`, with `candidates_dropped` and
`skipped_files`); `coverage` reports what was discovered/exercised;
and `outcome` (`passed_with_coverage | no_findings_low_coverage |
blocked_incomplete_harness | engine_failed | dry_run`) lets a consumer
tell a real clean pass from a probe that under-ran (only
`passed_with_coverage` licenses "found nothing"). `findings[]` keeps
its reproducer-only contract. Budget-0 fuzz reports `outcome: dry_run`
with the fuzz engine `blocked`. Migration: `docs/design/probe-schema-v3-migration.md`.
v2.19 adds an optional `clusters[]` array under `--emit-spec-candidates`
(additive; distinct from `candidates[]` — clusters are proto-spec-clauses
for the scaffold-to-spec interview). v2.20 extends the bootstrap envelope
with `dispatcher_kind: "shank_central_match"` for native programs
where `qedgen probe --bootstrap` detects a central-match dispatcher
in `lib.rs` (S2.1 Shank adapter), and each `handlers[]` entry now
carries per-handler `applicable_categories` + `intent_tag`
narrowed by handler-body heuristic (S2.2 — authority-gated /
trader-gated / permissionless).

v2.44 (#235) adds the **IDL-enrichment overlay** to every spec-less
envelope. Source discovery stays ground truth; when an on-disk IDL exists
(canonical paths: `idl.json`, `program/idl.json`, `target/idl/*.json`,
`idl/*.json` — Anchor legacy / 0.30 / Codama IR all accepted) the envelope
reports it as `idl_path` and each matched `handlers[]` entry gains
`idl_accounts` (signer/writable flags) + `idl_args` (name/type,
discriminators elided). On Anchor/Quasar — where declared signer flags are
runtime-enforced — the IDL derives an `intent_tag` for handlers the body
classifier left untagged, narrowing `applicable_categories` (body
classification always wins; Codama/Shank flags on other runtimes enrich
but never narrow). Handler-set disagreement between source and IDL
surfaces as `idl_source_drift` entries in `candidates[]` (both
directions, never silently reconciled). Pinocchio bootstrap fills its
otherwise-empty `handlers[]` from the Codama IDL (`discovered_via:
"idl"`, `source_file` = the IDL path). No IDL on disk → overlay skipped;
when one is mechanically derivable it is reported as `derivable_idl:
"anchor" | "quasar" | "shank" | "codama"` — an unbuilt Anchor/Quasar
checkout is one `anchor build` away (idl-build default-on since Anchor
0.30, and this beats any codama config, which in framework repos consumes
the built IDL), a `shank`/`codama` dep or codama config file is one
`shank idl` / `codama run` away. A hint for the agent, the CLI does not
shell out.

v2.44 (#240) also runs a **dead-guard / unwired-error-variant sweep** on
every spec-less envelope: each `#[error_code]` enum variant that is defined
but has no enforcement call-site (`require!` / `require_*!` / `err!` /
`return Err(.. Variant ..)` / a match arm) anywhere in `src/` surfaces as an
`unwired_error_variant` entry in `candidates[]` (`handler` = the variant,
`spec_silent_on` = its definition `file:line`). A named-but-never-fired
error is a guard that exists in name only — the path it was meant to protect
proceeds unchecked. Deterministic (enumerate the enum, grep each variant),
so it is a candidate, never a reproducer-backed finding; the
`investigation_hint` carries the severity rule (grade at the impact ceiling
of the unguarded path, not a dead-variant floor). No `#[error_code]` enum →
no candidates (clean no-op, not a false positive).

v2.44 adds **spec elicitation** (design:
`docs/design/spec-elicitation-prd.md`) to every spec-less envelope,
default-on (no flag):

- `run_id` — stable per-run identifier (`run-<program>-<unix-secs>`),
  threaded through the audit working set so `ratify` outputs join back to
  the probe run (funnel conversion, time-to-first-check).
- `hypotheses[]` — evidence-anchored, confirmable invariant hypotheses
  about *this* program from the deterministic hypothesizer
  (`probe/hypothesize.rs`). Seven classes: `authorization` (a single
  unambiguous IDL signer plus a stored-authority binding — body
  key-comparison / assert helper, an IDL `has_one` relation naming the
  signer, or an authority-named enforced signer); `lifecycle_init_once`
  (an init-shaped handler plus an init guard in the body or an Anchor
  `#[account(init, …)]` constraint; `init_if_needed` does not count);
  `arithmetic_bound` (a bound check the body already enforces —
  `require!(param <= X, Err)` or the if-return-Err shape — lifted into a
  question; never guessed from a type width or name); `conservation` (a
  paired forward/reverse operation — deposit/withdraw etc. — with no
  supply-changing flow anywhere in the scan; abstains the moment any
  issuance/destruction flow exists); `cpi_integrity` (a pinned SPL-token
  callee plus a resolved `Transfer { from, to, authority }` role
  mapping; abstains when either is unresolved); `unwired_guard` (a #240
  dead-guard candidate flipped into "you named this check but never
  wired it — should it hold?"; `accept` routes to a missing-enforcement
  finding, `reject` records a dead variant); and `state_machine` (an IDL
  status enum carried by a state struct field — exactly one, else the
  representation is ambiguous and it abstains — lifted into the spec's
  `type State`). Precision rule: **no evidence anchor
  → no hypothesis** — a handler name alone never fires. Each record
  carries `id` (`h-<8hex>-<class>-<handler>`, stable across runs),
  `claim`, `evidence[]` (`{kind, detail, source}`), `payoff`, `backend`,
  `assurance` (always `checking` at emission — §3.1 assurance contract),
  `confidence`, and an optional `lowering` recipe `ratify` executes on
  accept.
- `spec_readiness` — `{hypotheses_total, by_class, by_confidence,
  lowerable}` supply counts.
- A ranked human-readable hypothesis summary (claim + evidence + payoff +
  backend + id) prints on **stderr**; stdout JSON stays the agent
  surface.

Deep cross-procedure hypothesis formation (state-machine completeness,
conservation across paths) remains the agent's job; the binary owns only
the deterministic, evidence-anchored classes.

```bash
# Spec-aware
$QEDGEN probe --spec my_program.qedspec

# Spec-less / brownfield (generic alias)
$QEDGEN probe --bootstrap --root programs/my_program

# Spec-less / brownfield (Pinocchio-aware alias — same envelope when
# the detected runtime is pinocchio, plus the site catalogue)
$QEDGEN probe --program programs/my_program

# v2.19 — emit candidate spec clauses for the scaffold-to-spec
# interview; companion `qedgen ratify` reads what's written to
# --audit-dir to produce the final .qedspec.
$QEDGEN probe --program programs/my_program \
              --emit-spec-candidates \
              --audit-dir .qed/audit/2026-05-16

# v2.21 — Crucible brownfield protocol-mode. No .qedspec required;
# emits a harness under <root>/.qed/fuzz/<prog>/ whose
# protocol guard suite checks observable post-state deltas such as
# lamport conservation, ownership/discriminator changes, close/realloc
# integrity, rent loss, and token-balance conservation. Program-internal
# errors (panic, unwrap, require!, overflow) remain transaction errors and
# require a spec assertion or an agent-authored reproducer.
$QEDGEN probe --fuzz 300 --root programs/my_program

# v2.21 — budget-0 dry-run: emit the harness without paying the
# build cost. Useful for previewing the action_* stubs the agent
# is asked to fill.
$QEDGEN probe --fuzz 0 --root programs/my_program

# v2.22 — same shape, Pinocchio. Requires a maintainer-authored
# Codama / Anchor 0.30 IDL on disk; canonical paths the dispatcher
# probes (first match wins):
#   <root>/idl.json
#   <root>/program/idl.json
#   <root>/target/idl/*.json     (Anchor `anchor build` output)
#   <root>/idl/*.json            (Codama default output dir)
# Anchor 0.30 top-level `instructions[]` and Codama IR nested
# `program.instructions[]` are both recognised. Native + sBPF still
# are not supported by brownfield Crucible. Native still has static probe
# coverage; sBPF assembly uses the dedicated Lean/qedsvm proof path.
$QEDGEN probe --fuzz 300 --root programs/my_pinocchio_program

# Domain mode — replay ratified domain sequences, then fuzz. Protocol
# mode stays blind to domain-specific bugs; domain mode links a ratified
# dossier fact to a spec invariant and deterministically replays the
# bound witness before exploratory fuzzing.
$QEDGEN probe --fuzz 300 --crucible-mode domain \
  --spec my_program.qedspec \
  --domain-dossier .qed/audit/latest/domain-dossier.json \
  --domain-sequences .qed/audit/latest/domain-sequences.json \
  --domain-sequence-bindings .qed/audit/latest/domain-sequence-bindings.json
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | optional | Path to `.qedspec` (spec-aware mode) — conflicts with `--bootstrap` and `--program` |
| `--bootstrap` | bool | false | Spec-less mode — walk a project root and emit the auditor work list. Requires `--root`. |
| `--root` | Path | optional | Project root for spec-less mode (the program crate dir). Paired with `--fuzz` (without `--spec`) for brownfield protocol-mode Crucible. The generated harness checks observable post-state guards: wallet/total lamports, ownership and discriminator stability, close/realloc integrity, rent exemption, and token-balance conservation. Program-internal faults such as panic, unwrap, `require!`, and overflow return transaction errors and are outside this spec-less guard suite. Pinocchio requires an on-disk Codama / Anchor 0.30 IDL (canonical paths: `idl.json`, `program/idl.json`, `idl/*.json`, `target/idl/*.json`); native and sBPF brownfield fuzzing are unsupported. |
| `--program` | Path | optional | Program audit mode entry point. Auto-routes via `Cargo.toml` detection to the runtime's dedicated extractor: Pinocchio → site catalogue + SAFETY metadata (v2.19), Anchor/Quasar → anchor extractor (scaffold-to-spec interview), native/qedgen-codegen → native extractor; runtimes without an extractor fall back to the generic bootstrap envelope. Static engine only: conflicts with `--spec`, `--bootstrap`, `--root`, and `--fuzz` (fuzz brownfield targets via `--fuzz <budget> --root <path>`; merge the two JSON outputs to combine engines). |
| `--runtime` | enum | auto | Override runtime detection. Values: `pinocchio`, `anchor`, `quasar`, `native`, `sbpf`. Pinocchio, Anchor/Quasar, and native each have a dedicated extractor under `--program`. `sbpf` identifies the target and returns generic metadata only; it does not make assembly auditable by the source auditor. |
| `--emit-spec-candidates` | bool | false | Lift probe evidence into candidate spec clauses in `clusters[]` for the scaffold-to-spec interview. This field is additive within the schema-v3 envelope and is distinct from `candidates[]`: clusters are proto-spec clauses; candidates are unconfirmed security leads. |
| `--audit-dir` | Path | optional | When paired with `--emit-spec-candidates`, write the resumable audit working set: `hypotheses.json` (run_id + the elicitation hypothesis set — ratify's lowering input), `clusters.json`, `skeleton.qedspec`, `domain-dossier.{json,md}`, `domain-interview.{json,md}`, `run-manifest.json` (carries `run_id` + `spec_readiness`), and the legacy `interview.md`. `qedgen ratify --audit-dir <path>` consumes this directory and adds the ratified handoff/sequence artifacts. Conventionally `.qed/audit/<timestamp>/`. |
| `--fuzz` | u64 | none | Wall-clock seconds. Runs the coverage-guided fuzz engine INSTEAD of the pattern-match predicates for that invocation (run `probe --spec` separately and merge the JSON to combine engines). Requires `--spec <path>` (spec-driven invariants) OR `--root <project-path>` (brownfield protocol-mode); passing both layers spec invariants on top of protocol guards. Findings come back in the same `findings[]` with a `Reproducer::Crucible`. Each minimized crash is replayed and classified from the harness's `[FUZZ_FINDING]` marker rather than a last-action heuristic: the reproducer carries the named `invariant_id` when replay identified one, `category_tag` reflects the evidence (`invariant_violation`, `property_violation`, a protocol guard, `assertion_failure`, or `unclassified_crash`), non-reproducing crashes are dropped, and `coverage.replay_success` reports replay health. Budget `0` emits the selected harness and returns `outcome: dry_run` without building or fuzzing. |
| `--harness-dir` | Path | `./fuzz/<prog>/` | Crucible harness directory. Matches `codegen --crucible` output. An existing harness is reused, never regenerated (agent-filled `todo!()` account literals survive re-runs); delete it to pick up spec or binding changes. When the directory leaf differs from the program name it is treated as a parent and the `<prog>` leaf is appended. |
| `--no-smoke` | bool | false | Skip the 30s smoke pre-flight that stops early on high-rate duplicate findings. |
| `--stateful` | bool | false | Stateful action-chain mode. Higher throughput, longer crash chains. |
| `--crucible-mode` | enum | inferred | Select the Crucible verification layer explicitly (all values require `--fuzz`): `protocol` (mechanical behavioral guards; requires `--root`), `skeleton` (structural `.qedspec` assertions; requires `--spec`), `domain` (ratified domain facts plus protocol guards; requires `--spec` and `--domain-dossier`). Omitted → legacy inference: root-only = protocol, spec-only = skeleton, spec + root = both. |
| `--domain-dossier` | Path | - | Canonical `domain-dossier.json` for `--crucible-mode domain`. Every fact assigned to the Crucible lane must be ratified (`auto` or `user`) before fuzzing starts. Requires `--fuzz`. |
| `--domain-sequences` | Path | - | Deterministic action targets emitted by `qedgen ratify`. Every target must resolve before domain-mode replay starts. Requires `--fuzz` and `--domain-sequence-bindings`. |
| `--domain-sequence-bindings` | Path | - | Explicit user values for every unresolved account, argument, and lifecycle association in `--domain-sequences` — never inferred from names or nearby source. Requires `--fuzz` and `--domain-sequences`. Produces `resolved-domain-sequences.json`, `account-binding-overlay.json`, a byte-exact replay seed corpus, and a durable `domain-replay-report.json`. |
| `--execute-repros` | bool | false | **#228** — build and run generated reproducer harnesses, promoting a candidate to a finding only when its harness actually reproduces. **Off by default**: the default `probe --spec` only *generates* harnesses under `target/qedgen-repros/<category>/<handler>/` and leaves each candidate carrying a `repro_harness` pointer (path + exact `rustc … && ./repro` invocation + `failing_input`) for the agent/CI to run — so the default path performs no builds and no execution (agent-authored-repros default preserved). Currently wired for `ArithmeticOverflowWrapping` (`+=?` / `-=?`): the harness is a deterministic boundary-value program that exits 0 iff the wrap reproduces. On promotion the finding carries a `Reproducer::BoundaryValue`. A `reproducers` engine run reports counts (generated / executed / reproduced / build errors); `blocked` when generated-not-run. Requires `rustc` on PATH (soft dependency). |
| `--json` | bool | false | Accepted for parity with sibling subcommands (#251) — probe output is unconditionally JSON, so the flag is a no-op rather than a clap error. |

### `ratify`
Consume the working set emitted by `qedgen probe --emit-spec-candidates
--audit-dir <path>` and produce the final `.qedspec`. Since v2.44 the
primary answer surface is **structured**: the in-harness interview's
answers land in `<audit_dir>/answers.json` —
`{"run_id": …, "answers": [{"id", "decision", "note"}]}` — addressing
elicitation hypotheses (`h-…`, from `hypotheses.json`) and scaffold
clusters (`c-…`, from `clusters.json`) uniformly. When `answers.json`
resolves, the legacy user-edited `interview.md` is not consulted (and not
required); audit dirs from older probes keep working through the
`interview.md` path unchanged.

Decisions route as follows:

- `accept` → cluster clauses merge as before; **confirmed hypotheses are
  lowered to executable clauses** (authorization → `auth <signer>`
  injected into the handler body; lifecycle → an init-shaped
  `: State.<pre> -> State.<post>` transition resolved against the
  skeleton's own `type State` variants, rewriting placeholder
  self-loops). Each lowering commits only if the spec still parses and
  introduces no new Error-severity lints; otherwise the hypothesis is
  reported **`confirmed, not executable`** and stays in the dossier —
  never inserted as a placeholder comment.
- `narrow` → clusters only; clause emitted per-handler instead of
  program-wide.
- `reject` → appended to `<project_root>/.qed/plan/scoping.md` with the
  rationale (clusters and hypotheses alike).
- `bug` → a finding file: clusters →
  `.qed/findings/scaffold-to-spec-<id>.md`, hypotheses →
  `.qed/findings/elicitation-<id>.md` (the invariant is intended but
  unenforced — elicitation doubling as a bug-catcher).

The check gate is mandatory: the ratified spec **must parse** (hard
error otherwise) and completeness-lint counts are printed beside the
result with its assurance level (`checking`). Ratify also writes
`elicitation-outcome.json` (`run_id`, per-hypothesis outcomes,
`time_to_ratify_seconds`, check counts) into the audit dir — the
conversion half of the Phase-0 funnel instrumentation.

```bash
# Structured answers (in-harness interview; the agent writes answers.json)
$QEDGEN ratify --audit-dir .qed/audit/2026-07-17 \
              --out my_program.qedspec

# Also generate the spec-model proptest harness from the ratified spec
$QEDGEN ratify --audit-dir .qed/audit/2026-07-17 --proptest
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--audit-dir` | Path | required | Directory previously written by `probe --emit-spec-candidates --audit-dir`. Must contain `clusters.json` and `skeleton.qedspec`, plus either `answers.json` (structured) or the legacy `interview.md`. |
| `--out` | Path | derived | Output path for the generated `.qedspec`. Defaults to `<project_root>/<project_name>.qedspec`, derived from the audit-dir grandparent. |
| `--scoping-out` | Path | `<project_root>/.qed/plan/scoping.md` | Override the rejected-answer scoping-notes path (append-on-write). |
| `--findings-dir` | Path | `<project_root>/.qed/findings/` | Override the directory bug-flagged findings are written to. |
| `--answers` | Path | `<audit_dir>/answers.json` if present | Structured answer set. When resolved, `interview.md` is ignored. |
| `--proptest` | bool | false | Also generate the spec-model proptest harness at `<audit_dir>/model-proptest.rs`. Generation is `checking`-level evidence that the spec lowers; **running** the harness (`qedgen verify --proptest` in a scaffolded project) is what earns the `model-tested` label — never conflate the two. |

## Code generation

### `codegen`
Generate committed artifacts from a qedspec. Default (no artifact flags)
generates the program Rust skeleton only (Anchor-compatible; see the generated
`Cargo.toml` for dependency configuration). Passing explicit artifact flags
generates only those selected artifacts; `--all` emits the Rust scaffold plus
every artifact. The `.qed/` prerequisite therefore applies to the default and
`--all`, not to a harness-only invocation such as `--proptest`.

Requires a git repo (see [Require-git guard](#require-git-guard)).

`--spec` is optional — when omitted, resolved via the nearest
`.qed/config.json`'s `spec` field. Explicit `--spec` overrides.

When any model backend runs (`--kani`, `--proptest`, `--lean`, `--all`; not
sBPF specs), codegen also writes the backend-obligation manifest to
`.qed/obligations.json` (#332): every requested obligation, per backend, as
`emitted` (with the harness / theorem / test name), `unsupported` (with a
machine-readable capability reason), or `failed`. One summary line per
backend is printed, plus one line per non-emitted obligation. The manifest
never gates codegen — `verify --strict` is the gate.

```bash
# From inside a project initialized with `qedgen init --spec ...`
$QEDGEN codegen
$QEDGEN codegen --all

# Explicit spec path
$QEDGEN codegen --spec my_program.qedspec --all

# Selective
$QEDGEN codegen --lean
$QEDGEN codegen --kani
$QEDGEN codegen --test
$QEDGEN codegen --proptest
$QEDGEN codegen --integration
$QEDGEN codegen --ci

# Rename recovery (#288) — after a spec-level rename left the user-owned
# files stale (codegen warns). Both need a committed git baseline.
$QEDGEN codegen --merge-accounts   # Anchor: regen #[derive(Accounts)] structs only, fills survive
$QEDGEN codegen --force            # regen user-owned set wholesale; re-apply fills from git
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | optional | Spec file or directory. Defaults to `.qed/config.json spec` |
| `--target` | enum | `anchor` | Framework target for the Rust program crate. Values: `anchor` (Anchor-compatible, default); `quasar` (Blueshift `quasar_lang`); `pinocchio` (Pinocchio `#![no_std]` — `entrypoint!` + byte-discriminant dispatch, zeropod zero-copy state, `&AccountInfo` account structs with `.handler()` methods, checked effects, SPL Token CPIs). All three targets emit the full program scaffold. The verification backends (`--kani` / `--proptest` / `--lean` / `--ci`) are spec-driven and target-agnostic — they run for any target (see the comment at the top of any generated `tests/kani.rs`). Exception: `--integration` is Quasar-only — the in-process SVM scaffold imports `quasar_svm` and the generated `<name>-client` crate, which don't compile for other targets; non-Quasar targets skip it with a note. |
| `--output-dir` | Path | `./programs` | Output directory for Rust skeleton. Relative paths — this and every `--*-output` default below — resolve against the **spec's directory** (the project root), not the invoker's cwd (#279): `codegen --spec <elsewhere>/x.qedspec` from anywhere writes into `<elsewhere>/`. Absolute paths pass through untouched. |
| `--force` | bool | false | **Destructive opt-in (#288):** regenerate the USER-OWNED files too (`src/lib.rs`, `src/instructions/*.rs`) — the rename workflow where regen + re-fill beats hand-merging. Every affected file must have a committed, unmodified git baseline (the recovery path); dirty or untracked files abort before anything is written. Conflicts with `--merge-accounts`. |
| `--merge-accounts` | bool | false | **Surgical rename recovery (#288, Anchor only):** regenerate only the `#[derive(Accounts)]` structs inside the user-owned `lib.rs`, preserving handler fills and everything else (the Cargo.toml section-merge doctrine applied to Rust items). Hand-tuned constraints inside replaced structs are overwritten, so the same git-baseline guard applies. Structs with no matching spec handler (pre-rename leftovers, hand-added instructions) are left in place and reported. |
| `--all` | bool | false | Generate the Rust scaffold and all artifacts |
| `--lean` | bool | false | Generate Lean 4 proofs |
| `--lean-output` | Path | `./formal_verification/Spec.lean` | Lean output path |
| `--kani` | bool | false | Generate Kani proof harnesses (spec-model — verifies the spec's effect block against its own `ensures` clauses). |
| `--kani-output` | Path | `./programs/tests/kani.rs` | Kani output path. Lives **inside the program package** so `cargo kani --tests` resolves `programs/Cargo.toml` without a hand-authored root shim. |
| `--kani-impl` | bool | false | Generate **impl-targeted** Kani harnesses (v2.26): calls the user's real Anchor handler against a symbolic `Accounts` context and asserts the spec's `ensures` clauses. Pairs with `--kani` (spec-model harnesses live in a separate file). Even without this flag, emission is auto-triggered when any handler declares `modifies` listing fields absent from its `effect` block — the LP-shape signal indicating the impl is expected to fill those fields. Anchor target only in v2.26. |
| `--kani-impl-output` | Path | `./programs/tests/kani_impl.rs` | Impl-targeted Kani harness output path. Separate file from `--kani-output` so `cargo kani --harness` can target either set without ambiguity. |
| `--kani-impl-brownfield` | bool | false | Emit the **brownfield** Anchor impl-Kani shape (#162): a state-struct harness (symbolic state → agent-fill: apply the real effect + validity gate → assert `ensures`) instead of the greenfield `Accounts` context + `accounts.handler(...)` shape, which does not resolve against a pre-existing Anchor program (shared Accounts structs, `Context<T>` + `Args`, associated-fn handlers). Snapshots/assume/assert are generated; only the struct construction and effect application are `todo!()`. Implies emission (no separate `--kani-impl` needed). Anchor target only. |
| `--kani-impl-context` | bool | false | Emit the **Context/instruction** impl-Kani shape (#169): drives the REAL `#[derive(Accounts)]` constraint gate — `<Ctx>::try_accounts` over symbolic leaked-backing `AccountInfo`s — then the real instruction fn through a `Context` (the one agent-fill site), asserting instruction-level authorization: generated signer-gate asserts per spec-`signer` account plus the `ensures`. `try_deserialize` is stubbed to the spec-generated symbolic state ctor (needs `pragma state_struct`), bypassing the Borsh wall. The real `#[derive(Accounts)]` struct name comes from `pragma context_struct = <Struct>` (or `= <handler>::<Struct>` per handler; default `PascalCase(handler)`). Composes with the #182 Pubkey/PDA/Clock/log/CPI stubs. Implies emission. Anchor target only. |
| `--test` | bool | false | Generate unit tests |
| `--test-output` | Path | `./programs/tests/unit.rs` | Unit test output path. Lives in `tests/` so cargo auto-discovers the target (the pre-v2.47 `src/tests.rs` default was never included by the scaffold's `lib.rs`, so the tests never compiled or ran). Legacy `src/tests.rs` files are still recognized by regen-drift. |
| `--proptest` | bool | false | Generate proptest harnesses |
| `--proptest-output` | Path | `./programs/tests/proptest.rs` | Proptest output path. Lives inside the program package (see `--kani-output`). |
| `--crucible` | bool | false | Generate a coverage-guided fuzz harness (v2.18). Anchor target only; sBPF specs are skipped with a note (assembly is Lean-verified); Pinocchio specs error early. Output is a self-contained `fuzz/<prog>/` directory with `Cargo.toml`, `src/main.rs` (the harness), and `idls/`. Action-body `accounts::X { ... }` literals emit as `todo!()` for agent-fill (same as handler bodies). |
| `--crucible-output` | Path | `./fuzz` | Parent directory for the generated harness. Final tree lives at `<dir>/<prog>/`. |
| `--integration` | bool | false | Generate in-process SVM integration tests. Quasar targets only — skipped with a note on `anchor` / `pinocchio` (the scaffold's `quasar_svm` + client-crate imports don't compile there) |
| `--integration-output` | Path | `./programs/tests/integration_tests.rs` | Integration test output path |
| `--ci` | bool | false | Generate GitHub Actions CI workflow |
| `--ci-output` | Path | `.github/workflows/verify.yml` | CI workflow output path |
| `--ci-asm` | String | - | sBPF assembly source (for CI verify step) |
| `--ci-ratchet` | Path | - | Anchor IDL the generated CI should lint with `qedgen readiness`. When set, the emitted `verify.yml` runs ratchet after the verification jobs — any breaking / unsafe finding fails the build. Path is repo-root-relative (e.g. `target/idl/escrow.json`) |
| `--fill` | bool | false | **DEPRECATED (v3.0 removal).** Emits stdout prompt blocks per handler with `todo!()`. The agent can fill these directly via Read / Edit — grep for `todo!()` in `programs/`, look up the handler in the spec, edit in place. Flag still runs in v2.x but prints a deprecation warning. |
| `--handler` | String | - | Restrict `--fill` to one handler by name (deprecated with `--fill`). |
| `--fill-tests` | bool | false | **DEPRECATED (v3.0 removal).** Same shape as `--fill` for `tests/integration_tests.rs`. Agent fills directly. |

#### MIR-default dispatch

Every codegen backend routes through `mir::Mir`. As of v2.32 the MIR
migration is complete: `lean_gen_mir` / `kani_mir` / `codegen_mir` /
`proptest_gen_mir` are the *sole* codegen paths. There are no
`QEDGEN_LEGACY_*` escape hatches and no parallel legacy renderers — the
legacy `lean_gen.rs`, `kani.rs`, `proptest_gen.rs`, and the legacy
`codegen::generate` were all deleted (`codegen.rs`'s shared helpers live
on as `codegen_shared.rs`). Output is locked by checked-in snapshot
suites (`tests/{mir,kani,codegen,proptest}_snapshot.rs`).

`lean_gen_mir` handles every spec shape, including sBPF
(`mir.is_assembly` → `render_sbpf`). For sBPF specs (`pragma sbpf`)
only `--lean` and `--ci` emit — the Rust scaffold and every
Rust-shaped backend (`--kani` / `--kani-impl` / `--test` /
`--proptest` / `--crucible` / `--integration`) are skipped with a
note, since assembly is verified via Lean proofs + client-side tests,
not generated Rust artifacts. The canonical sBPF regen command is:

```bash
qedgen codegen --lean --spec <spec>.qedspec --lean-output formal_verification/Spec.lean
```

#### Scaffold-once vs. always-regenerate

`codegen` distinguishes files that are **always regenerated** from the spec
(pure derived artifacts) from files that are **scaffolded once** and then
become user-owned (business logic, tactic bodies, integration glue). On the
second run, scaffold-once files are detected as present and skipped with an
advisory line on stderr; their always-regenerated siblings next to them are
refreshed.

| Path | Policy |
|---|---|
| `programs/<name>/src/instructions/mod.rs` | Always regenerated (pure `pub mod` declarations) |
| `programs/<name>/src/instructions/<handler>.rs` | Scaffolded once (user-owned body; `#[qed]` tied to spec) |
| `programs/<name>/src/lib.rs` | Scaffolded once (user-owned crate root) |
| `programs/<name>/src/guards.rs` | Always regenerated |
| `programs/<name>/src/errors.rs` | Always regenerated |
| `tests/integration/*.rs` | Scaffolded once (user-owned integration tests) |
| `programs/tests/kani.rs` | Always regenerated |
| `programs/tests/kani_impl.rs` | Always regenerated (when `--kani-impl` or auto-triggered) |
| `programs/tests/proptest.rs` | Always regenerated |
| `formal_verification/Spec.lean` | Always regenerated |
| `formal_verification/Proofs.lean` | Scaffolded once (user-owned preservation proofs) |
| `.github/workflows/verify.yml` | Always regenerated |

`Proofs.lean` bootstrapping uses `proofs_bootstrap::bootstrap_if_missing` —
it never overwrites. Once a user-owned file exists, the only way to pick up
new theorems from a changed spec is to add them by hand (or delete the file
and re-run). `qedgen reconcile` flags the delta.

#### `#[qed]` drift attributes

Every scaffolded handler function is stamped with

```rust
#[qed(verified,
      spec      = "../../program.qedspec",
      handler   = "deposit",
      spec_hash = "7e1a48d93b2c0f65")]
pub fn deposit(...) -> Result<()> { ... }
```

and the `hash = "..."` body-hash field is filled in by
`qedgen check --drift --update-hashes` (or manually) once the handler body
stabilises. At compile time the `qedgen-macros` proc macro:

1. Reads the spec file referenced by `spec`
2. Extracts the `handler <handler> { ... }` block verbatim
3. Hashes it (SHA-256, first 16 hex chars)
4. Compares against the `spec_hash` literal — `compile_error!` on mismatch
5. Hashes the function signature + body and compares against `hash` — same

This turns "edit the spec, forget to regen" into a compile error and
"edit a verified function, forget to re-verify" into a compile error.

`#[qed]` attribute arguments (all strings, all optional after `verified`):

| Arg | Purpose |
|---|---|
| `verified` | Marker keyword (required first) |
| `spec` | Path to the `.qedspec` file, relative to the `.rs` source |
| `handler` | Name of the `handler { ... }` block in that spec |
| `hash` | SHA-256-hex16 of the fn signature + body; omit to get a `compile_error` with the computed value |
| `spec_hash` | SHA-256-hex16 of the spec-side handler block text |

See SKILL.md **Step 4d — drift reconciliation** for the full agent-driven
workflow; this page is the flag reference only.

## Proof generation

### `generate`
Generate Lean 4 proofs via Leanstral API (pass@N sampling).

```bash
$QEDGEN generate --prompt-file /tmp/prompt.txt --output-dir /tmp/proof --passes 4 --validate
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--prompt-file` | Path | required | Path to prompt file |
| `--output-dir` | Path | required | Output directory |
| `--passes` | int | 4 | Number of independent completions |
| `--temperature` | float | 0.6 | Sampling temperature |
| `--max-tokens` | int | 16384 | Max tokens per completion |
| `--validate` | bool | false | Validate with `lake build` |
| `--mathlib` | bool | false | Include Mathlib in validation workspace |

### `fill-sorry`
Fill sorry markers in a Lean file using Leanstral.

```bash
$QEDGEN fill-sorry --file formal_verification/Spec.lean --validate
$QEDGEN fill-sorry --file formal_verification/Spec.lean --escalate
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--file` | Path | required | Lean file with sorry markers |
| `--output` | Path | overwrites input | Output path |
| `--passes` | int | 3 | Attempts per sorry |
| `--temperature` | float | 0.3 | Sampling temperature |
| `--max-tokens` | int | 16384 | Max tokens |
| `--validate` | bool | false | Validate with `lake build` |
| `--escalate` | bool | false | Auto-escalate to Aristotle if sorry remains |

## Aristotle (Harmonic theorem prover)

### `aristotle submit`
Submit a Lean project for long-running sorry-filling.

```bash
$QEDGEN aristotle submit --project-dir formal_verification --wait
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--project-dir` | Path | required | Lean project directory |
| `--prompt` | String | "Fill in all sorry..." | Custom prompt |
| `--output-dir` | Path | same as project-dir | Output directory |
| `--wait` | bool | false | Block until completion |
| `--poll-interval` | int (sec) | 30 | Polling interval; clamped to [5, 3600] |

### `aristotle status`
Check project status; with `--wait`, poll until terminal and download the result.

```bash
$QEDGEN aristotle status <project-id>
$QEDGEN aristotle status <project-id> --wait --output-dir formal_verification
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `<project-id>` | String | required | Project ID returned by `aristotle submit` |
| `--wait` | bool | false | Poll until terminal status, then download |
| `--poll-interval` | int (sec) | 30 | Polling interval; clamped to [5, 3600]. Requires `--wait` |
| `--output-dir` | Path | `.` | Where to extract the result. Requires `--wait` |

### `aristotle result`
Download a completed project's solution archive.

```bash
$QEDGEN aristotle result <project-id> --output-dir formal_verification
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `<project-id>` | String | required | Project ID |
| `--output-dir` | Path | `.` | Where to extract the result |

### `aristotle cancel`
Cancel a running project.

```bash
$QEDGEN aristotle cancel <project-id>
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `<project-id>` | String | required | Project ID to cancel |

### `aristotle list`
List recent projects.

```bash
$QEDGEN aristotle list
$QEDGEN aristotle list --limit 25 --status IN_PROGRESS
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--limit` | int | 10 | Maximum number of projects to show |
| `--status` | String | none | Filter by status (e.g. `IN_PROGRESS`, `COMPLETE`, `FAILED`) |

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
upgrade landmines before the program ever ships: missing `version: u8`
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
[`docs/design/qedsvm-discharge.md`](../docs/design/qedsvm-discharge.md).

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

## Utility

### `consolidate`
Merge multiple proof projects into a single Lean project.

```bash
$QEDGEN consolidate --input-dir /tmp/proofs --output-dir formal_verification
```

### `feedback`
File a GitHub issue with the last command's failure context.

```bash
# Walk through the most recent failure (reads `.qed/last-error.log`).
$QEDGEN feedback --note "lint flags X but my spec declares it"

# Print the title and body without filing anything.
$QEDGEN feedback --dry-run

# Skip the interactive confirmation (CI / scripts).
$QEDGEN feedback --yes
```

| Flag | Type | Default | Notes |
|---|---|---|---|
| `--note <text>` | string | — | Free-form description of what happened. Top of the issue body. |
| `--title <text>` | string | auto | Override the derived title (`[qedgen <version>] <command> failed: <line>`). |
| `--spec <path>` | path | auto | Override the auto-resolved `.qedspec` path used for the excerpt. |
| `--dry-run` | bool | false | Print to stdout; no local artifact, no remote submission. |
| `--yes` | bool | false | Skip the interactive y/N prompt. Required in non-interactive shells. |
| `--no-open` | bool | false | Suppress the browser open on the pre-filled-URL fallback path. |

Submission order: local copy to `.qed/feedback/<timestamp>.md` (silent) → preview → confirmation → `gh issue create` → pre-filled GitHub URL fallback if `gh` is unavailable. Override the target repo with `QEDGEN_FEEDBACK_REPO=owner/repo`.

The bundled context is the most recent command's stderr (captured automatically into `.qed/last-error.{log,json}` by `main()`'s error path), the qedgen version, OS/arch, detected runtime, and a `.qedspec` excerpt centered on the error's line hint when one is parseable.

## Environment variables

| Variable | Required for | Description |
|---|---|---|
| `MISTRAL_API_KEY` | `generate`, `fill-sorry` | Mistral API key. Free at [console.mistral.ai](https://console.mistral.ai) |
| `ARISTOTLE_API_KEY` | `aristotle` commands | Harmonic API key. Get at [aristotle.harmonic.fun](https://aristotle.harmonic.fun) |
| `QEDGEN_HOME` | - | Override global home directory (default: `~/.qedgen/`) |
| `QEDGEN_VALIDATION_WORKSPACE` | - | Override validation workspace path |
| `QEDGEN_FEEDBACK_REPO` | `feedback` | Override the issue target (default: `QEDGen/solana-skills`) |

## Error handling

| Error | Fix |
|---|---|
| `qedgen requires a git repo` | Run `git init` in the project root |
| First `lake build` is slow | Without Mathlib: seconds. With `--mathlib`: 15-45 min first time, cached after. |
| `could not resolve 'HEAD' to a commit` | Remove `.lake/packages/mathlib`, run `lake update` |
| Rate limiting (429) | Built-in exponential backoff in `fill-sorry` |
