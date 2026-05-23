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
$QEDGEN init --name dropset  --spec dropset.qedspec --asm src/dropset.s
$QEDGEN init --name engine   --spec engine.qedspec --mathlib
$QEDGEN init --name counter  --spec counter.qedspec --target anchor
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--name` | String | required | Project name (alphanumeric + underscores) |
| `--spec` | Path | - | Spec path (file or directory) — written into `.qed/config.json` so `check`/`codegen` can resolve it automatically |
| `--asm` | Path | - | sBPF assembly source (runs asm2lean automatically) |
| `--mathlib` | bool | false | Include Mathlib dependency |
| `--target` | enum | - | Also generate the program crate + Kani harnesses for the named framework target. Values: `anchor` (Anchor-compatible Rust), `quasar` (Blueshift Quasar — `#![no_std]`, explicit discriminators, `Ctx<X>`), `pinocchio` (reserved CLI surface; codegen not yet implemented — selecting it errors). Requires `--spec`. Omit to skip program scaffolding entirely. |
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
| `--coverage` | bool | false | Show operation × property matrix (also enabled by default) |
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
| `--proptest` | bool | false | Run proptest harnesses (`cargo test --release`) |
| `--proptest-path` | Path | `./programs/tests/proptest.rs` | Proptest harness file |
| `--kani` | bool | false | Run Kani BMC harnesses (`cargo kani --tests`) |
| `--kani-path` | Path | `./programs/tests/kani.rs` | Kani harness file |
| `--lean` | bool | false | Run Lean proofs (`lake build`) |
| `--lean-dir` | Path | `./formal_verification` | Lean project directory |
| `--fail-fast` | bool | false | Stop on the first failing backend |
| `--json` | bool | false | Machine-readable output for CI |
| `--check-upstream` | bool | false | Diff each pinned `upstream_binary_hash` against the on-chain `.so` via `solana program dump`. Skips deps without a pinned hash. Non-zero exit on any mismatch. |
| `--rpc-url` | String | Solana CLI default | Override RPC endpoint passed to `solana program dump --url <rpc>` |
| `--offline` | bool | false | Refuse to reach the network. Any dep that would require an on-chain fetch reports as Error. CI-gate friendly. |
| `--upstream-stale-ok` | bool | false | Suppress the upstream binary-hash check even when the lock declares pinned hashes. Mismatches demote to Info; verify exits zero. Offline-dev only — do not use in CI. Pairs with the auto-on behavior of `--check-upstream`. |
| `--probe-repros` | bool | false | Run probe reproducers under `<project>/target/qedgen-repros/` (PLAN-v2.16 D4). Each repro is a Mollusk-driven Rust test asserting a probe finding's bug fires; the verb captures pass/fail per finding so the auditor / next probe invocation can drop findings whose repros didn't reproduce. Pre-populated repros (v3-pending) — emits `note: no repros found` placeholder until the agent-fill workflow lands. |
| `--crucible` | u64 | none | Run the coverage-guided fuzz engine for the given wall-clock seconds. Thin alias over `probe --fuzz` — folds findings into the BackendReport so they render through the same named-trace human surface as Kani / proptest. |
| `--crucible-harness-dir` | Path | `./fuzz/<prog>/` | Harness directory for `--crucible`. |
| `--crucible-no-smoke` | bool | false | Skip the 30s smoke pre-flight. |
| `--crucible-stateful` | bool | false | Stateful action-chain mode for `--crucible`. |
| `--recursive` | bool | false | v2.27 Track D3 — DFS-walk the transitive proof-package closure (deduped by path) and run `lake build` per layer. Per-layer PASS/FAIL is reported; failed layers print the first ~10 lines of stderr/stdout. Exits non-zero on any layer failure; emits "every imported proof package built clean" when all pass. No-op success when the spec imports nothing with `verified = true` in `qed.lock`. |
| `--require-verified` | bool | false | v2.27 Track D2 — exits non-zero before any backend dispatches if any imported Tier-1+ interface (binary_hash + `ensures`) did NOT ship a `.qed/proofs/<Iface>.lean + lakefile.lean` package alongside. Tier-0 (no ensures) and sentinel-pinned natives (all-zero binary_hash) are exempt. Default-off in v2.27 because the bundled stdlib still ships Stance 1 for `import System from "system"` (no bundled proof package for Pubkey-param handlers). |

### `probe`
Probe a `.qedspec` for category-coverage gaps (spec-aware mode) or
walk a brownfield project root and emit a per-handler work list
(spec-less / `--bootstrap` / `--program` mode). Output is JSON,
consumed by the auditor subagent. Spec-aware emits `findings`;
spec-less emits `runtime`, `handlers`, `applicable_categories`.
v2.16 schema bumps to `version: 2` with the addition of an optional
`reproducer` field on findings (drop-on-fail pipeline; findings
without a confirmed reproducer are silently dropped — see
`feedback_probes_reproducible_only.md`). v2.19 schema bumps to
`version: 3` when `--emit-spec-candidates` is set, adding a
`clusters[]` array that the auditor subagent surfaces through the
scaffold-to-spec interview. v2.20 extends the bootstrap envelope
with `dispatcher_kind: "shank_central_match"` for native programs
where `qedgen probe --bootstrap` detects a central-match dispatcher
in `lib.rs` (S2.1 Shank adapter), and each `handlers[]` entry now
carries per-handler `applicable_categories` + `intent_tag`
narrowed by handler-body heuristic (S2.2 — authority-gated /
trader-gated / permissionless).

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
# invariant_test() body is empty (protocol-level panics, unwraps,
# borrow-mut errors, and overflow surface via Crucible's host loop).
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
# bail (deferred to v2.23+; native will gate on Shank).
$QEDGEN probe --fuzz 300 --root programs/my_pinocchio_program
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | optional | Path to `.qedspec` (spec-aware mode) — conflicts with `--bootstrap` and `--program` |
| `--bootstrap` | bool | false | Spec-less mode — walk a project root and emit the auditor work list. Requires `--root`. |
| `--root` | Path | optional | Project root for spec-less mode (the program crate dir). v2.21 also paired with `--fuzz` (no `--spec`) for brownfield protocol-mode Crucible — emits a harness at `<root>/.qed/fuzz/<prog>/` whose `invariant_test()` body is empty so only intrinsic crashes (panic / unwrap-on-None / `BorrowMutError` / arithmetic overflow) fire. v2.22 lifts the runtime gate for Pinocchio when a Codama / Anchor 0.30 IDL is on disk (canonical paths: `idl.json`, `program/idl.json`, `idl/*.json`, `target/idl/*.json`); native + sBPF still bail with a deferral message targeting v2.24+ (v2.23 shipped the pre/post property lowering trust fix + brownfield first-contact onboarding flow instead of touching this gate). |
| `--program` | Path | optional | v2.19 user-facing alias for `--bootstrap --root <path>` (the Pinocchio-shape probe entry point; auto-routes via `Cargo.toml` detection so the same flag works for Anchor / native crates too, falling back to the generic spec-less envelope when not Pinocchio) |
| `--runtime` | enum | auto | Override runtime detection. Values: `pinocchio`, `anchor`, `quasar`, `native`, `sbpf`. Only `pinocchio` has dedicated probe output today; the others fall back to the generic bootstrap envelope. |
| `--emit-spec-candidates` | bool | false | v2.19 — lift findings into candidate spec clauses (clusters) the auditor subagent surfaces through the scaffold-to-spec interview. Schema bumps to v3 with a `clusters[]` field. v2-shape consumers see no change when the flag is off. |
| `--audit-dir` | Path | optional | v2.19 — when paired with `--emit-spec-candidates`, write the full audit working set (`interview.md`, `clusters.json`, `skeleton.qedspec`) to this directory. Companion `qedgen ratify --audit-dir <path>` consumes the three files to produce the final spec. Conventionally `.qed/audit/<timestamp>/`. |
| `--fuzz` | u64 | none | Wall-clock seconds. Runs the coverage-guided fuzz engine alongside (or instead of) the pattern-match predicates. v2.21+: requires `--spec <path>` (spec-driven invariants) OR `--root <project-path>` (brownfield protocol-mode); passing both layers spec invariants on top of protocol crash detection. Findings come back in the same `findings[]` with `category: crucible_fuzz_crash` and a `Reproducer::Crucible`. Budget `0` emits the harness scaffold (brownfield only) and exits without building / running the fuzzer — handy for previewing what the agent needs to fill before paying the Crucible build cost. |
| `--harness-dir` | Path | `./fuzz/<prog>/` | Crucible harness directory. Matches `codegen --crucible` output. |
| `--no-smoke` | bool | false | Skip the 30s smoke pre-flight that stops early on high-rate duplicate findings. |
| `--stateful` | bool | false | Stateful action-chain mode. Higher throughput, longer crash chains. |

### `ratify`
v2.19 — consume the working set emitted by `qedgen probe
--emit-spec-candidates --audit-dir <path>` (an `interview.md` checked
by the user, a `clusters.json`, and a `skeleton.qedspec`) and produce
the final `.qedspec`. Decisions on `interview.md` route as follows:

- `[x] accept` → cluster's candidate clause merged into the handler
  body or top-level invariant set of the output `.qedspec`.
- `[x] narrow` → clause emitted per-handler instead of program-wide.
- `[x] reject` → cluster dropped from the spec, but appended to
  `<project_root>/.qed/plan/scoping.md` with the user's rationale
  (the rejected-decision log).
- `[x] bug` → emitted as a finding file under
  `<project_root>/.qed/findings/scaffold-to-spec-<id>.md`. Used when
  the implicit precondition the cluster surfaced is a real
  missing-enforcement bug, not a spec gap.

```bash
$QEDGEN ratify --audit-dir .qed/audit/2026-05-16 \
              --out my_program.qedspec
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--audit-dir` | Path | required | Directory previously written by `probe --emit-spec-candidates --audit-dir`. Must contain `interview.md`, `clusters.json`, `skeleton.qedspec`. |
| `--out` | Path | derived | Output path for the generated `.qedspec`. Defaults to `<project_root>/<project_name>.qedspec`, derived from the audit-dir grandparent. |
| `--scoping-out` | Path | `<project_root>/.qed/plan/scoping.md` | Override the rejected-cluster scoping-notes path (append-on-write). |
| `--findings-dir` | Path | `<project_root>/.qed/findings/` | Override the directory bug-flagged cluster findings are written to. |

## Code generation

### `codegen`
Generate committed artifacts from a qedspec. Default (no flags) generates
the program Rust skeleton only (Anchor-compatible; see the generated
`Cargo.toml` for dependency configuration).

Requires a git repo (see [Require-git guard](#require-git-guard)).

`--spec` is optional — when omitted, resolved via the nearest
`.qed/config.json`'s `spec` field. Explicit `--spec` overrides.

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
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | optional | Spec file or directory. Defaults to `.qed/config.json spec` |
| `--target` | enum | `anchor` | Framework target for the Rust program crate. Values: `anchor` (Anchor-compatible, fully implemented — default); `quasar` (Blueshift `quasar_lang`, fully implemented); `pinocchio` (reserved CLI surface; scaffold codegen not yet implemented). For Pinocchio specs, the scaffold step is skipped but `--kani` / `--proptest` / `--lean` / `--integration` / `--ci` still run — pass `--target pinocchio --kani --proptest --lean` (or `--all`) to generate just the verification backends without an unwanted Anchor scaffold. Bare `--target pinocchio` (no backend flags) errors with the same message. Verification backends are spec-driven and target-agnostic by design (see the comment at the top of any generated `tests/kani.rs`). |
| `--output-dir` | Path | `./programs` | Output directory for Rust skeleton |
| `--all` | bool | false | Generate all artifacts |
| `--lean` | bool | false | Generate Lean 4 proofs |
| `--lean-output` | Path | `./formal_verification/Spec.lean` | Lean output path |
| `--kani` | bool | false | Generate Kani proof harnesses (spec-model — verifies the spec's effect block against its own `ensures` clauses). |
| `--kani-output` | Path | `./programs/tests/kani.rs` | Kani output path. Lives **inside the program package** so `cargo kani --tests` resolves `programs/Cargo.toml` without a hand-authored root shim. |
| `--kani-impl` | bool | false | Generate **impl-targeted** Kani harnesses (v2.26): calls the user's real Anchor handler against a symbolic `Accounts` context and asserts the spec's `ensures` clauses. Pairs with `--kani` (spec-model harnesses live in a separate file). Even without this flag, emission is auto-triggered when any handler declares `modifies` listing fields absent from its `effect` block — the LP-shape signal indicating the impl is expected to fill those fields. Anchor target only in v2.26. |
| `--kani-impl-output` | Path | `./programs/tests/kani_impl.rs` | Impl-targeted Kani harness output path. Separate file from `--kani-output` so `cargo kani --harness` can target either set without ambiguity. |
| `--test` | bool | false | Generate unit tests |
| `--test-output` | Path | `./programs/src/tests.rs` | Unit test output path |
| `--proptest` | bool | false | Generate proptest harnesses |
| `--proptest-output` | Path | `./programs/tests/proptest.rs` | Proptest output path. Lives inside the program package (see `--kani-output`). |
| `--crucible` | bool | false | Generate a coverage-guided fuzz harness (v2.18). Anchor target only; sBPF / Pinocchio specs error early. Output is a self-contained `fuzz/<prog>/` directory with `Cargo.toml`, `src/main.rs` (the harness), and `idls/`. Action-body `accounts::X { ... }` literals emit as `todo!()` for agent-fill (same as handler bodies). |
| `--crucible-output` | Path | `./fuzz` | Parent directory for the generated harness. Final tree lives at `<dir>/<prog>/`. |
| `--integration` | bool | false | Generate in-process SVM integration tests |
| `--integration-output` | Path | `./src/integration_tests.rs` | Integration test output path |
| `--ci` | bool | false | Generate GitHub Actions CI workflow |
| `--ci-output` | Path | `.github/workflows/verify.yml` | CI workflow output path |
| `--ci-asm` | String | - | sBPF assembly source (for CI verify step) |
| `--ci-ratchet` | Path | - | Anchor IDL the generated CI should lint with `qedgen readiness`. When set, the emitted `verify.yml` runs ratchet after the verification jobs — any breaking / unsafe finding fails the build. Path is repo-root-relative (e.g. `target/idl/escrow.json`) |
| `--fill` | bool | false | **DEPRECATED (v3.0 removal).** Emits stdout prompt blocks per handler with `todo!()`. The agent can fill these directly via Read / Edit — grep for `todo!()` in `programs/`, look up the handler in the spec, edit in place. Flag still runs in v2.x but prints a deprecation warning. |
| `--handler` | String | - | Restrict `--fill` to one handler by name (deprecated with `--fill`). |
| `--fill-tests` | bool | false | **DEPRECATED (v3.0 removal).** Same shape as `--fill` for `tests/integration_tests.rs`. Agent fills directly. |

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
| `--list-rules` | bool | false | Print the catalog of P-rules applied and exit |
| `--json` | bool | false | Machine-readable output |

### `check-upgrade`
Diff an old vs new Anchor IDL and flag every upgrade-unsafe change.
Catches the failure modes `solana program upgrade` won't — field
reorders, discriminator changes, orphaned accounts, PDA seed drift,
signer/writable tightening.

```bash
# Standard upgrade diff
$QEDGEN check-upgrade --baseline old.json --candidate new.json

# Acknowledge a specific finding so it reports as Additive
$QEDGEN check-upgrade --baseline old.json --candidate new.json \
  --ack R007=ProgramId

# Declare a migration / realloc was added in source
$QEDGEN check-upgrade --baseline old.json --candidate new.json \
  --has-migration TreasuryV2 --has-realloc UserConfig

# Print the rule catalog and exit
$QEDGEN check-upgrade --list-rules
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--baseline` | Path | required | Baseline IDL (the one on-chain today) |
| `--candidate` | Path | required | Candidate IDL (the one the upgrade would ship) |
| `--ack` | String | - | Acknowledge a specific finding so it reports as Additive (repeatable). Pass `--list-rules` to see the full flag catalog. |
| `--has-migration` | String | - | Declare an account as having a migration in source; demotes R003/R004 findings for that account to Additive (repeatable) |
| `--has-realloc` | String | - | Declare an account as having `realloc = ...` in source; demotes R005 for that account to Additive (repeatable) |
| `--list-rules` | bool | false | Print the catalog of R-rules applied and exit |
| `--json` | bool | false | Machine-readable output |

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
