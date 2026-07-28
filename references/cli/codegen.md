# CLI: code generation

`codegen` — every generated artifact and its flags.

Part of the [CLI Reference](../cli.md).

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
| `--no-check-compiles` | bool | false | Skip the post-codegen compile check (#364). |

#### Post-codegen compile check (#364)

After writing the program crate, `codegen` runs `cargo check --tests` over
it and reports any error, so a codegen defect is caught in the command that
produced it rather than in a later `cargo build`.

It runs only when the dependency tree already resolves (a `Cargo.lock` in
the crate or its parent). A first generation therefore stays fast and
offline, and prints one line saying the check was deferred; by the second
run the answer arrives in seconds. It never changes the exit code —
`codegen`'s contract is to write files, and a brownfield crate that fails
to build for unrelated reasons must not make `codegen` unusable.
[`verify --scaffold`](validation.md#verify) is the gating surface and always
runs. Opt out with `--no-check-compiles`.

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

