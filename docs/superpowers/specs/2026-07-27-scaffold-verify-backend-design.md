# Scaffold Verification Backend Design

## Context

Issue #364 identifies a gap in the user-facing verification loop: QEDGen can
generate a program crate, report zero spec errors, and run model harnesses
without ever compiling the generated program itself. Maintainer CI compiles a
fixed example corpus, but that cannot cover the shapes in an arbitrary user's
spec.

The `verify` command already represents proptest, Kani, Lean, and Miri runs as
`BackendReport` values. The scaffold compile check belongs in that same
pipeline so it shares reporting, fail-fast behavior, JSON output, evidence
recording, and exit handling.

## Goals

- Add a `scaffold` verification backend that runs `cargo check --tests` in the
  program crate supplied by `--program`.
- Add an explicit `--scaffold` CLI flag.
- Auto-enable scaffold verification when `--program` is supplied and no
  backend flags are selected.
- Run scaffold verification before heavier backends so `--fail-fast` stops
  promptly on uncompilable generated code.
- Report actionable Cargo/rustc diagnostics through the existing human and
  JSON `BackendReport` formats.
- Persist the scaffold result in `.qed/verify-evidence.json` without allowing
  compilation alone to authorize `#[qed(verified)]`.
- Make `verify --strict` reject an enabled scaffold backend that could not run.

## Non-goals

- Do not add a post-codegen compile option.
- Do not change code generation or repair compile failures.
- Do not make scaffold compilation semantic conformance evidence.
- Do not add scaffold compilation to the semantic backend-obligation manifest.
- Do not add Cargo flags such as `--locked`, `--offline`, or target-specific
  build arguments.
- Do not change verification runs that omit `--program`.

## CLI Behavior

`--scaffold` is an explicit verify backend selector and requires `--program`.
The selection rules are:

| Invocation shape | Scaffold behavior |
| --- | --- |
| `verify --program <crate>` with no backend flags | Auto-enabled |
| `verify --program <crate> --scaffold` | Enabled explicitly |
| `verify --program <crate> --kani` | Not enabled |
| `verify --program <crate> --kani --scaffold` | Enabled with Kani |
| `verify` without `--program` | Unchanged |
| `verify --scaffold` without `--program` | CLI usage error |

The existing definition of "backend flag" expands to include `--scaffold`
wherever `run.rs` decides whether a command is backend-only, whether it should
return after an upstream or probe-reproducer stage, and whether it should
perform on-disk backend discovery.

## Architecture

### Backend integration

`VerifyOpts` gains:

- `scaffold: bool`
- `program_dir: Option<PathBuf>`

`verify::run` registers `ScaffoldBackend` before Proptest, Kani, Lean, and
Miri. Its `enabled` method reads `opts.scaffold`; its `run` method passes the
selected program directory to a focused `run_scaffold` function.

`run_scaffold`:

1. Starts the backend timer.
2. Returns `Skipped` when no program directory was supplied.
3. Returns `Skipped` when `<program>/Cargo.toml` does not exist, naming the
   missing manifest.
4. Runs `cargo check --tests` with `current_dir` set to the program directory.
5. Returns `Passed` on exit status zero.
6. Returns `Failed` with a concise Cargo/rustc diagnostic on nonzero exit.
7. Returns `Skipped` with an installation/path hint when Cargo cannot be
   spawned.

The program directory is not canonicalized before execution. Cargo and the
existing evidence layer already provide path-specific errors, and preserving
the user-provided display path keeps diagnostics understandable.

### Default selection

When any explicit backend flag is present, `run.rs` honors exactly those
flags. When none is present, existing harness discovery remains intact and
`scaffold` becomes true only if `--program` was supplied. This keeps
`verify --program <crate> --kani` Kani-only while making the flagless
`verify --program <crate>` close the compile gap automatically.

### Failure summaries

The scaffold backend combines Cargo stdout and stderr, then extracts a bounded,
deterministic diagnostic:

- begin at the first line starting with `error` when present;
- retain that error block and nearby source context;
- retain Cargo's final `could not compile` line when present;
- cap the rendered detail so JSON and terminal output do not absorb an entire
  dependency build log.

If no structured error marker exists, the summary uses the final non-empty
output lines. The detail must always explain that `cargo check --tests`
failed.

### Strict mode

An ordinary skipped scaffold backend remains nonfatal, matching current
missing-harness behavior. Under `--strict`, an enabled scaffold report with
`Skipped` status causes exit code 1 after the report is printed. A `Failed`
report already fails every verify run through `VerifyReport::ok`.

This gate reads the backend report directly. The backend-obligation manifest
continues to mean semantic obligations requested by the spec; adding a
program-build pseudo-obligation would weaken that model.

### Evidence semantics

The existing evidence serializer records every backend name and status, so no
schema change is required. `verify::evidence::build` must explicitly keep
`scaffold` outside the set of implementation-bound conformance backends.
A passing scaffold check proves that the selected source tree compiles, not
that handlers implement the spec.

## Error Handling

- Missing program argument for explicit `--scaffold`: clap usage error.
- Missing `Cargo.toml`: skipped with exact expected path.
- Cargo executable unavailable: skipped with an actionable installation/PATH
  message; strict mode turns the skip into a gate failure.
- Cargo/rustc nonzero exit: failed backend with the relevant diagnostic.
- A scaffold failure participates in `--fail-fast` because it runs first.
- No failure path mutates generated source or the program manifest.

## Testing

Tests follow red-green TDD and use real temporary Cargo crates:

1. A valid minimal crate returns a passing scaffold report.
2. A crate with an unresolved Rust name returns a failed report containing the
   rustc error and source location.
3. A directory without `Cargo.toml` returns a skipped report naming the
   manifest.
4. CLI selection tests prove flagless `--program` auto-enables scaffold.
5. CLI selection tests prove an explicit model backend does not auto-enable
   scaffold.
6. CLI selection tests prove `--scaffold` composes with another backend.
7. A fail-fast test proves scaffold runs before later backends.
8. A strict-mode test proves an enabled skipped scaffold exits nonzero.
9. An evidence test proves a passing scaffold backend does not set
   `implementation_verified`.
10. Existing verify human/JSON rendering tests continue to cover the generic
    report shape.

Final verification runs the targeted tests, `cargo test`,
`cargo clippy -- -D warnings`, and `npm test`.

## Documentation

Update the verify CLI help and the verify command reference to explain:

- `--scaffold` requires `--program`;
- flagless `verify --program <crate>` automatically checks the crate;
- explicit backend selection remains exact;
- compilation is not semantic verification or stamp authority;
- skipped scaffold checks fail under `--strict`.

## Compatibility

The only newly failing default path is a flagless verify invocation that
already supplies `--program` and points at a crate that does not compile. That
is the intended behavior change. Invocations without `--program`, and
invocations selecting explicit model backends without `--scaffold`, retain
their existing behavior.
