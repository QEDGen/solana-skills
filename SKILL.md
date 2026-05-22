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

## First Contact (Brownfield)

If the user invokes you on an **existing** Solana program with no real `.qedspec` (or only a skeleton), do **not** route them straight into spec-writing. Spec-writing from a cold start is unmotivated work. Instead, route them through `/qedgen-auditor` first; the auditor surfaces real findings in their code, and *then* the spec captures those findings as permanent regression guards. The pitch:

> "I see this is an existing Solana program. Before we write a spec, let me hand off to `/qedgen-auditor` to find what's already broken. We'll lock those findings in as a spec so they don't come back."

### Brownfield indicators (agent-side detection)

Walk the filesystem (Read / Glob via the harness's tools; no new CLI needed per `[[feedback_agent_lsp_substrate]]`). The repo is brownfield-onboarding when **any** of:

- `Cargo.toml` exists at the root or under `programs/` / `program/`, with Rust source under `src/` or `programs/*/src/`, **and** no `*.qedspec` file anywhere in the tree.
- A `*.qedspec` exists at the root but contains no `state { }` block (template-only skeleton). Skeleton specs are a near-universal "I tried, got stuck" signal.
- An Anchor IDL (`target/idl/*.json`) exists but no committed `.qedspec`.

### What to issue

When detected, recommend the cross-skill switch in your harness's idiom (Claude Code TUI: suggest `/qedgen-auditor`; Codex / Cursor / etc.: name the skill the user should invoke next). Do not programmatic-spawn the auditor — per `[[feedback_audit_as_subagent]]`, the auditor is a harness-native subagent that the user enters explicitly. Your job here ends at the **recommendation** and a one-line summary of what they'll get.

The user re-enters `/qedgen` after the audit produces `.qed/findings/`; the audit-side handoff section in `skills/qedgen-auditor/SKILL.md` and the `references/finding_to_spec.md` mapping table drive the conversion from findings to spec constructs.

### Greenfield path stays unchanged

If the repo has no `Cargo.toml` (or none of the brownfield indicators fire), proceed to the standard validate → scaffold → fill → verify flow. The brownfield branch only intercepts first-contact when there's already-deployed code to audit.

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

**No `--fill` flag.** The agent reads the generated files, greps for `todo!()`, looks up the matching handler / accounts / effect in the `.qedspec`, and edits each body in place. The old `qedgen codegen --fill` / `--fill-tests` flags emitted structured prompts to stdout for the agent to consume — useful before agents had file tools, ceremony now. They're soft-deprecated in v2.18 (print a warning, still run) and will be removed in v3.0. Same direct-edit pattern applies to integration tests and Crucible action bodies.

Step 5. Verify generated backends.

```bash
$QEDGEN verify --spec program.qedspec --proptest
$QEDGEN verify --spec program.qedspec --kani
$QEDGEN verify --spec program.qedspec --kani-impl   # v2.26 — calls user's real Anchor handler (opt-in)
$QEDGEN verify --spec program.qedspec --lean
$QEDGEN verify --spec program.qedspec --crucible 300   # coverage-guided fuzz (5 min)
$QEDGEN verify --spec program.qedspec --check-upstream # v2.26 — pin-mismatch is CRIT (use --upstream-stale-ok offline)
```

v2.26 split the Kani layer into two harness shapes: `--kani` runs the
v2.25 ensures-preservation harness against the spec-translated transition
fn (catches spec-internal inconsistency); `--kani-impl` runs the new
impl-targeted harness against the user's *real* Anchor handler against
a symbolic `Accounts` context (catches impl-violates-spec). The impl
harness is opt-in via the flag, and auto-triggers when any handler has
`modifies ⊋ effect.lhs` (the LP-shape signal) or any `ref_impl` carries
potentially-overflowing arithmetic over bounded-numeric params (the
`ref_impl_unbounded_arith` lint shape).

The Crucible fuzz path is a separate engine: it drives the deployed `.so`
with mutated typed-action sequences and crashes from real execution. Run
`$QEDGEN probe --spec program.qedspec --fuzz 300` to get the JSON
findings list directly, or `--crucible 300` on verify to fold them into
the BackendReport. First-time setup needs `crucible` on PATH (see
`references/cli.md`) plus a built harness from `codegen --crucible`.

After `codegen --crucible`, the generated `fuzz/<prog>/src/main.rs`
contains one `todo!("agent-fill: accounts::X { ... } from spec accounts
block")` site per handler. Fill these directly — no `--fill` flag.
The agent reads the spec's `accounts` block for the handler, cross-
references the program's Anchor `Context<X>` struct (or the IDL JSON
the user drops at `idls/<prog>.json`), and constructs the literal in
place. Then run `cargo build --features invariant_test` in the harness
dir and iterate on any compile errors. The IDL is auto-discovered from
`target/idl/<prog>.json` when present; drop it there before the build.

Failing harnesses surface with spec-named values (the binder name from the spec, not `var_3`):

```
[FAIL] kani       (4567 ms)
       counterexample: probe_overflow_transfer
         assertion failed: post == pre.checked_add(amount).unwrap_or(0)
         at tests/kani.rs:42:5
           pre    = 18446744073709551615ul
           amount = 1ul
           post   = 0ul
```

Use the named values to propose the next spec edit (tightening a `requires`, adding an `aborts_if`, marking an effect `+=!`/`+=?`), then re-run `qedgen verify`.

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

## Invariants vs Properties

Two related but distinct constructs in `.qedspec`:

- **`property` / `preserved_by`** — a predicate over `state` that some named set of handlers must preserve. Use when the predicate is the headline correctness claim for those handlers (`pool_solvency preserved_by all`, `votes_bounded preserved_by [create_vault, propose, ...]`). Generates per-handler proptest/Kani harnesses and Lean preservation theorems. v2.23: properties whose bodies reference `old(...)` lower to a binary predicate `fn p(pre: &State, post: &State) -> bool`, and the preservation harness captures pre-state before the handler call so the obligation is real. The `vacuous_property_lowering` lint surfaces any property whose lowered Rust collapses to a structural tautology (`s.x cmp s.x`) when the source AST carries `old(...)` — a regression guard on the structural fix.
- **`invariant` + handler-side `invariant Name` / `establishes Name`** — a named predicate referenced from inside handler blocks. Use when the same predicate is asserted by multiple handlers and the handler-side claim is what you want the spec to highlight. The handler clause is the join: `invariant Foo` means *preserves* (assume Foo pre-state, assert post), `establishes Foo` means *establishes* (no pre-assume, assert post only — useful for init / one-shot transitions).

```fsharp
invariant root_set :
  state.root != ZERO_ROOT

handler init : State.Active -> State.Active {
  establishes root_set
  effect { root := <derived_pda> }
}

handler update : State.Active -> State.Active {
  invariant root_set       // preserves: assumes root_set pre, asserts post
  requires state.root != ZERO_ROOT
  effect { root := <new_root> }
}
```

Both forms generate Rust-side BMC + proptest harnesses when the body has a `rust_expr` and at least one handler links to it. Description-only invariants (`invariant name "..."`) are documentation only — no Rust harness emits.

Pick `property` when the handler list is short and the property name reads naturally as the claim ("conservation"). Pick `invariant` when the predicate is reused as a *thing* across many handlers, especially when some establish it and others preserve it.

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
