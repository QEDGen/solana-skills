---
name: qedgen
description: Find the bugs your tests miss. Define what your Solana program must guarantee in a .qedspec; QEDGen validates it, generates tests and proofs, and scaffolds agent-fill Rust code. Trigger when the user asks for "qedgen", "qedspec", "verify my code", "prove correctness", formal verification, property testing, generated Kani/proptest/Lean artifacts, or Solana program correctness.
---

# QEDGen

## Trigger And Mission

Use this skill when the user wants to verify Solana program behavior, write or review a `.qedspec`, generate verification artifacts, onboard an existing Anchor program, or keep generated artifacts in sync.

Mission:
- Read the source before writing the spec.
- Treat `.qedspec` as the single source of truth.
- Use `qedgen check` to validate the spec.
- Use `qedgen codegen` to scaffold generated artifacts.
- Fill generated Rust handler TODOs as an agent task, then build and test.
- Use `qedgen verify` and drift gates to keep proofs and code synchronized.

Do not present generated Rust as complete business logic. Anchor and Quasar output is an implementation scaffold. Handler files can intentionally contain `todo!()` for transfers, events, CPI wiring, and non-mechanical effects until the agent fills them.

## How To Run QEDGen

Prefer the installed skill wrapper when available:

```bash
QEDGEN="$HOME/.agents/skills/qedgen/tools/qedgen"
```

From a repo checkout, the local binary also works:

```bash
cargo run -p qedgen-solana-skills -- <command>
```

Every write path expects a git repo. If the command errors outside a repo, run `git init` or move into the project root.

Common commands:

```bash
$QEDGEN check --spec program.qedspec
$QEDGEN codegen --spec program.qedspec --all
$QEDGEN verify --spec program.qedspec
$QEDGEN reconcile --spec program.qedspec --code programs/ --proofs formal_verification/
```

Release and repo-maintenance gates:

```bash
bash scripts/check-version-consistency.sh
bash scripts/check-readme-drift.sh
$QEDGEN check --regen-drift
```

Read `references/cli.md` for the full CLI surface and flags.

## Flow: Validate -> Scaffold -> Fill -> Verify

Step 1. Understand the program.

Read the Rust source, tests, account model, authorities, PDAs, token flows, arithmetic, and lifecycle. For a returning QEDGen project, read the `.qedspec` next to the code. Do not treat `Spec.lean` as source; it is generated.

Step 2. Validate the spec.

```bash
$QEDGEN check --spec program.qedspec --coverage
$QEDGEN check --spec program.qedspec --json
```

Fix lint, coverage, import, lifecycle, arithmetic, and CPI-shape findings in the `.qedspec` first. The spec should describe the intended behavior before codegen or proof work begins.

Step 3. Scaffold generated artifacts.

```bash
$QEDGEN codegen --spec program.qedspec --target anchor --all
```

Use `--target quasar` for Quasar. Pinocchio is reserved and should not be promised as complete.

Step 4. Fill generated Rust.

Open generated handler files that contain `todo!()`. Fill business logic using the guard calls, state structs, and spec effects as the contract. Then run the framework build and tests until compile-clean:

```bash
cargo check --manifest-path programs/Cargo.toml
cargo test --manifest-path programs/Cargo.toml
```

Step 5. Verify generated backends.

```bash
$QEDGEN verify --spec program.qedspec --proptest
$QEDGEN verify --spec program.qedspec --kani
$QEDGEN verify --spec program.qedspec --lean
```

Run only the backends relevant to artifacts present in the project. For generated examples in this repo, also run:

```bash
$QEDGEN check --regen-drift
```

## Brownfield Onboarding

For an existing Anchor program:

```bash
$QEDGEN adapt --program programs/my_program --out program.qedspec
```

Then fill TODOs in the `.qedspec`, validate it, and cross-check against the live program:

```bash
$QEDGEN check --spec program.qedspec --anchor-project programs/my_program
```

After the spec covers each handler, stamp source drift attributes:

```bash
$QEDGEN adapt --program programs/my_program --spec program.qedspec
```

Paste the emitted `#[qed(verified, ...)]` attributes above the matching handler functions. Future handler-body, accounts-constraint, or spec edits should fail the build until the attributes are intentionally refreshed.

If handler dispatch is non-standard, use explicit overrides:

```bash
$QEDGEN adapt --program programs/my_program --handler deposit=processor::deposit
```

For IDL-only onboarding:

```bash
$QEDGEN spec --idl target/idl/my_program.json
```

IDL scaffolds are shape-only. They need source review before they can express semantic guarantees.

## Codegen Ownership

Generated and always safe to regenerate:

| Path | Owner | Notes |
|---|---|---|
| `Cargo.toml` | QEDGen | Framework dependencies and macro dependency |
| `src/state.rs` | QEDGen | Account/state structs and lifecycle status |
| `src/events.rs` | QEDGen | Event structs |
| `src/errors.rs` | QEDGen | Error enum plus operational variants |
| `src/guards.rs` | QEDGen | Requires, aborts, lifecycle, PDA, and token-authority checks |
| `src/math.rs` | QEDGen | Emitted only when helper arithmetic is needed |
| `src/instructions/mod.rs` | QEDGen | Module declarations and Quasar re-exports |
| `tests/kani.rs` | QEDGen | Kani harnesses |
| `tests/proptest.rs` | QEDGen | Property-test harnesses |
| `src/tests.rs` | QEDGen | Unit tests when requested |
| `src/integration_tests.rs` | QEDGen | Integration-test scaffold when requested |
| `formal_verification/Spec.lean` | QEDGen | Lean model generated from `.qedspec` |

User-owned after first scaffold:

| Path | Owner | Notes |
|---|---|---|
| `src/lib.rs` | User or agent | Crate shell can gain custom imports/modules |
| `src/instructions/<handler>.rs` | User or agent | Business logic and generated TODOs live here |
| `formal_verification/Proofs.lean` | User or agent | Durable Lean proofs |
| Existing project tests | User or agent | Do not replace with generated tests |

Generated support code should compile around intentional handler TODOs. If support code fails to compile, fix the generator or generated support. If handler business logic is missing, fill the handler.

## Proof Handoff

Use proof engineering only when tests and bounded model checking are insufficient.

Use proptest for:
- Fast counterexamples during spec iteration.
- Randomized state transitions.
- Cheap regression checks.

Use Kani for:
- Access control.
- Arithmetic safety.
- Conservation and isolation invariants.
- Bounded state-machine properties.

Use Lean for:
- DeFi math that needs symbolic reasoning beyond bounded search.
- Wide arithmetic solvency arguments.
- Inductive sBPF bytecode proofs.
- Proof obligations where Kani/proptest cannot give enough confidence.

Use Leanstral for routine sorry filling and Aristotle for harder long-running proof search. Read `references/proof-patterns.md` before proof repair and `references/sbpf.md` for sBPF.

Always run `lake build` after editing Lean and run `qedgen check` after proofs compile so orphan or missing obligations are reported.

## References

Load references on demand. Do not bulk-load all files.

| Reference | Use When |
|---|---|
| `references/cli.md` | Full command and flag details |
| `references/qedspec-dsl.md` | DSL syntax and modeling patterns |
| `references/qedspec-imports.md` | `import`, `qed.toml`, `qed.lock`, `--frozen`, upstream checks |
| `references/qedspec-anchor.md` | Anchor adapter and brownfield coverage checks |
| `references/adversarial-probes.md` | Agent-walked attack-surface checklist |
| `references/proof-patterns.md` | Lean proof tactics and repair patterns |
| `references/support-library.md` | Lean support library types and lemmas |
| `references/sbpf.md` | sBPF assembly verification |
| `references/kani-examples.md` | Longer Kani harness examples moved out of the skill |
| `references/brownfield-testing.md` | Existing-test strategy for brownfield projects |
| `references/skill-operations.md` | Git hygiene, learning capture, environment, and error handling |
| `references/release-history.md` | Version-feature history moved out of the skill |
