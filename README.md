<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/logo-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="docs/logo-light.png">
    <img src="docs/logo-dark.png" alt="QEDGen" width="260">
  </picture>
</p>

<h3 align="center">Proofs, not promises.</h3>
<p align="center"><em>Ship without fear.</em></p>

<p align="center">
  <a href="https://qedgen.dev">Website</a> &middot;
  <a href="https://github.com/qedgen/solana-skills/blob/main/SKILL.md">Docs</a> &middot;
  <a href="https://github.com/qedgen/solana-skills/issues">Issues</a>
</p>

<p align="center">
  <a href="https://github.com/qedgen/solana-skills/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License"></a>
  <a href="https://qedgen.dev"><img src="https://img.shields.io/badge/site-qedgen.dev-38bdf8" alt="Website"></a>
</p>

---

Write what your Solana program must guarantee in a `.qedspec` file. QEDGen validates the spec, finds bugs your tests miss, then generates the verification artifacts and implementation scaffold needed to keep them fixed: **property tests**, **Kani harnesses**, **Lean 4 proofs**, **agent-fill program scaffolds**, and **CI workflows** — all from a single source of truth. Frameworks: **Anchor**, **Quasar**, and **Pinocchio** (greenfield scaffold via `qedgen init --target ...`), plus **sBPF assembly** through the dedicated Lean/qedsvm proof path. Brownfield source audit covers **Anchor / Quasar / Pinocchio / native Rust** via `qedgen probe` (with Miri-backed UB detection for Pinocchio) and lifts findings into a ratifiable spec. The auditor does not audit sBPF assembly; use `qedgen asm2lean` or a `.qedspec` for that target.

> **Alpha:** expect bugs and breaking API changes while the project evolves.

```bash
npx skills add qedgen/solana-skills
```

> Works with Claude Code, Cursor, Windsurf, GitHub Copilot, and any agent supporting the [Agent Skills](https://agentskills.io) spec.

**Or just point your agent here.** Paste this to your coding agent and it installs QEDGen and gets to work:

> Set up QEDGen for this program — follow `https://qedgen.dev/llms-full.txt`

That page opens with a **Quickstart for agents** (install, then audit your existing program or write a `.qedspec` from scratch), followed by the full reference. The shorter [`qedgen.dev/llms.txt`](https://qedgen.dev/llms.txt) is the index version.

## How it works

```
.qedspec ──► check (lint/report) ──► codegen --all ──► agent fill ──► verify ──► ∎
                │                         │                 │          ▲       │
                ├── lint (instant)        ├── Rust scaffold  │          ├─► Leanstral (fast)
                ├── coverage matrix       ├── Lean stubs     │          └─► Aristotle (deep)
                └── drift reports         ├── Kani harnesses └── cargo/lake/proptest
                                          └── tests + CI
```

1. **Define guarantees** — write a `.qedspec` describing what your program must guarantee, or let your agent generate one from the code or IDL
2. **Validate** — `qedgen check` runs spec lint, coverage, drift checks, and reports; run generated proptest/Kani/Lean backends with `qedgen verify`
3. **Generate** — `qedgen codegen --all` produces test harnesses, Lean stubs, CI workflows, and an agent-fill Rust scaffold from the single spec
4. **Prove** — your agent fills proof obligations; Leanstral handles routine sub-goals (seconds), Aristotle handles the hardest ones (minutes–hours)

## What it verifies

| Property | Approach |
|---|---|
| **Access control** | Signer checks, authority constraints |
| **CPI correctness** | Correct program, accounts, flags, and discriminator for each invocation (axiomatic, pure `rfl`) |
| **State machines** | Lifecycle correctness, one-shot safety |
| **Conservation** | Named `invariant`s and `property`s preserved (or `establishes`-ed) across operations. Per-handler proptest + Kani harnesses fire when the body has a Rust rendering; Lean theorems back the proofs. |
| **Fuzz-discovered paths** | Coverage-guided fuzzer (`qedgen codegen --crucible` + `qedgen probe --fuzz`) drives the deployed `.so` with mutated typed-action sequences. Crashes auto-`tmin` to minimal reproducers, dedupe by `(handler, outcome)`, and surface via `qedgen verify --crucible` with action-sequence counterexamples. |
| **Arithmetic safety** | Overflow/underflow for fixed-width integers, U64 bounds |
| **Input validation** | Account count, duplicates, data length, discriminators, parameter bounds — each guard maps to a specific error exit |
| **Memory correctness** | Stack/heap disjointness, pointer arithmetic (sBPF) |
| **Pinocchio soundness** | `unsafe`-serde and arithmetic site catalogue via `qedgen probe --program <root>`; `qedgen verify --miri` runs the generated repros under `cargo +nightly miri test` and surfaces UB / aliasing / overflow plus Miri-fail / Mollusk-pass divergence as Critical findings. |
| **PDA integrity** | Program-derived address derivation and 4-chunk comparison (sBPF) |
| **Deploy safety** | On-chain shape for Anchor **and Quasar** programs — version fields, reserved padding, pinned discriminators, signer coverage, PDA seed continuity — via `qedgen readiness` and `qedgen check-upgrade` (ratchet). |

CPI calls are axiomatic — we verify the program passes correct parameters. SPL Token internals and the Solana runtime are trusted.

**Proofs prove correctness. Ratchet proves deployability.** The P-rule preflight (`qedgen readiness`) catches future-upgrade landmines in a single IDL before the first deploy; the R-rule diff (`qedgen check-upgrade`) catches every breaking change between an old and new IDL once the program is live.

## Quick start

```bash
# 1. Install
npx skills add qedgen/solana-skills
```

Two paths from here — pick the one that matches what you have:

### A. Existing program (brownfield) — audit first, spec second

The audit-first flow works on Anchor, Quasar, Pinocchio, and native Rust.
sBPF assembly uses the proof-first path instead. Pitch: _"Find the bugs that are already there, then turn each
finding into a spec property that locks it in."_

```bash
# In Claude Code / Codex / Cursor, invoke the auditor subagent on the
# program you're onboarding:
#   /qedgen-auditor

# The auditor surfaces fired findings under .qed/findings/. When you
# re-enter /qedgen, the conversion table at
# skills/qedgen-auditor/references/finding_to_spec.md maps each finding
# class to the spec construct (property, requires, invariant, lifecycle)
# that locks it in. Walk through examples/rust/brownfield-onboarding/
# for an end-to-end demo on a real bug class.

# Want a spec without a full audit? The probe hypothesizes the
# invariants your program appears to enforce (evidence-anchored:
# signer bindings, held bound checks, init constraints, CPI roles,
# the status enum) and ranks them for confirmation:
qedgen probe --program ./my_program --emit-spec-candidates \
  --audit-dir .qed/audit/$(date +%F)                   # Anchor / Quasar / Pinocchio / native
# Answer accept/reject/bug per hypothesis (the agent writes them to
# .qed/audit/<ts>/answers.json), then apply the confirmed ones as
# executable clauses — the result is guaranteed to parse and lint:
qedgen ratify --audit-dir .qed/audit/$(date +%F) \
  --out my_program.qedspec

# Deprecated (functional in v2.x, removed in v3.0): the TODO-shell
# scaffolds `qedgen adapt --program` and `qedgen spec --idl` — the
# probe flow above subsumes both (the IDL is one of its evidence
# sources), and attribute stamping moved to `qedgen stamp`.
```

### B. New program (greenfield) — start from spec

```bash
# 1. Initialize the project — records the spec path in .qed/config.json
qedgen init --name my_program --spec my_program.qedspec --target anchor

# 2. Validate and generate artifacts (no --spec needed from inside the project)
qedgen check
qedgen codegen --all

# 3. Fill generated Rust handler TODOs, then run backend verification
qedgen verify
```

### Stuck? File feedback

```bash
# Bundles the most recent failure's context (stderr, env, spec excerpt)
# into a GitHub issue. Local copy always saved to .qed/feedback/.
qedgen feedback --note "what went wrong"
```

`.qed/config.json` pins the spec location so subsequent commands don't need
`--spec <path>` — `qedgen check`, `codegen`, `verify`, and `reconcile` all
walk up from the current directory, find the nearest `.qed/`, and resolve.
Explicit `--spec` still works when you want to point at something specific.

Lean and Kani toolchains are installed automatically the first time
they're needed. API keys are not — sign up at the providers below and
export them yourself before running `fill-sorry` or `aristotle`:

```bash
# Lean + Mathlib (only needed for formal proofs)
qedgen setup --mathlib

# API keys (only needed for sorry-filling and deep proof search)
export MISTRAL_API_KEY=your_key_here                    # sign up at https://console.mistral.ai (free tier available)
export ARISTOTLE_API_KEY=your_key_here                  # sign up at https://aristotle.harmonic.fun
```

## Usage

### Brownfield — audit-first first contact (recommended)

Works across Anchor, Quasar, Pinocchio, and native Rust. Spec-writing from a cold start is unmotivated work; the audit gives you something to write the spec _about_. For sBPF assembly, skip the source auditor and use the dedicated Lean/qedsvm proof path.

In your harness (Claude Code, Codex, Cursor), invoke the auditor:

```text
/qedgen-auditor
```

The auditor surfaces fired findings under `.qed/findings/`. Each finding ships with a reproducer (Mollusk transaction trace, Kani counterexample, proptest seed, or Miri repro) — no advisory-tier output, only bugs with a concrete witness. When you re-enter `/qedgen`, the conversion table at `skills/qedgen-auditor/references/finding_to_spec.md` maps each finding family (authorization, arithmetic, lifecycle, paired validators, intent drift, …) to the spec construct that locks it in as a regression guard.

End-to-end walkthrough on a real bug class: `examples/rust/brownfield-onboarding/`.

### Brownfield — spec elicitation (turn what the code already enforces into a spec)

The front door for code→spec is the probe. Every spec-less run hypothesizes program-specific invariants from evidence it can cite — a single authority-bound signer, a `require!(amount <= …)` the body already enforces, an Anchor `#[account(init)]` constraint, a resolved SPL-token `Transfer { from, to, authority }`, an unwired error variant, the IDL's status enum — and prints them ranked on stderr with the payoff of confirming each. Confirmations land in `answers.json`; `ratify` lowers each confirmed hypothesis to a real, executable clause (`auth <signer>`, `requires … else …`, lifecycle transitions, `transfers { … }`) and refuses to report success unless the result parses and lints. A claim it cannot lower without placeholders is reported `confirmed, not executable` — never smuggled in as a comment.

```bash
# Hypothesize + write the audit working set (skeleton, hypotheses.json, …)
qedgen probe --program ./programs/my_program --emit-spec-candidates \
  --audit-dir .qed/audit/$(date +%F)

# Confirm in conversation; the agent records {id → accept|reject|bug}
# to .qed/audit/<ts>/answers.json, then:
qedgen ratify --audit-dir .qed/audit/$(date +%F) --out my_program.qedspec
```

Every result is labelled with its assurance level: a ratified clause is **checking**; a generated model proptest that passes is **model-tested**; only a source-bound backend (impl-Kani, Miri, Mollusk repros) earns **implementation-verified**. Answering **bug** on a hypothesis files a missing-enforcement finding instead of a clause — elicitation doubles as a bug-catcher.

**Deprecated scaffolds** (functional in v2.x with a warning; removed in v3.0): `qedgen adapt --program` (TODO-shell skeleton — the probe writes the same skeleton as a byproduct and offers confirmable hypotheses on top) and `qedgen spec --idl` (the IDL is now a probe evidence source — signer flags, `has_one` relations, and status enums feed the hypotheses directly). `adapt --program --spec` (attribute emission) became [`qedgen stamp`](#verification-drift-detection). Quasar brownfield ingest isn't wired today (Quasar greenfield via `qedgen init --target quasar` is fully supported).

**Pinocchio / native.** The entry point is probe + ratify. `qedgen probe --program <root>` runtime-detects from `Cargo.toml` (`pinocchio` dep → Pinocchio mode; native programs use the native extractor or generic bootstrap envelope). Override with `--runtime pinocchio|native` when detection misses. `--runtime sbpf` can identify the target, but the source auditor deliberately declines assembly audits; use `qedgen asm2lean` or a `.qedspec` instead.

```bash
# Pinocchio: enumerate `unsafe`-serde / arithmetic sites, parse
# adjacent `// SAFETY:` comments, emit per-site Mollusk + Miri repro
# prompts the auditor subagent expands into runnable tests.
qedgen probe --program ./programs/my_pinocchio_program

# Run the generated Miri repros — UB / aliasing / overflow surface as
# findings; Miri-fail / Mollusk-pass divergence is Critical.
qedgen verify --miri

# Lift findings into a ratifiable spec (works across runtimes):
qedgen probe --program ./programs/my_program --emit-spec-candidates \
  --audit-dir .qed/audit/$(date +%F)
qedgen ratify --audit-dir .qed/audit/$(date +%F) --out my_program.qedspec
```

Native ships as preview — coverage is narrower than Anchor/Pinocchio because there are no framework conventions to anchor extractors on.

Once the spec exists, gate CI on it staying in sync with the program:

```bash
# Errors if the spec declares a handler that's not in the program
# (stale spec) or a `pub fn` that's not modelled in the spec
# (uncovered handler). Pure read; no codegen, no writes.
qedgen check --spec my_program.qedspec --anchor-project ./programs/my_program
```

### Greenfield — Anchor, Quasar, or Pinocchio

The same `.qedspec` codegens to any of three framework targets via
`--target`. Anchor is the default; `--target quasar` emits a Blueshift
Quasar (`#![no_std]` + `quasar_lang`) crate with explicit discriminators
and `Ctx<X>` instead of `Context<X>`; `--target pinocchio` emits a
Pinocchio (`#![no_std]`) crate with `entrypoint!` byte-discriminant
dispatch, `zeropod` zero-copy state, and `&AccountInfo` account structs
with `.handler()` methods.

```bash
# Anchor (default)
qedgen init --name my_program --spec my_program.qedspec
qedgen codegen --spec my_program.qedspec --all

# Quasar
qedgen init --name my_program --spec my_program.qedspec --target quasar
qedgen codegen --spec my_program.qedspec --target quasar --all

# Pinocchio
qedgen init --name my_program --spec my_program.qedspec --target pinocchio
qedgen codegen --spec my_program.qedspec --target pinocchio --all
```

Lean proofs, Kani harnesses, proptest harnesses, and CI workflows are
target-agnostic — they're driven by the spec, not the framework, so
the verification artifacts are identical across `--target` choices.
The deploy-safety lint (`qedgen readiness` / `qedgen check-upgrade`)
speaks Anchor and Quasar IDLs; see the *Deploy-safety lint* section
below.

### Spec-driven pipeline

```bash
# Initialize a new verification project from a .qedspec
qedgen init --name my_program

# Validate the spec (lint + coverage)
qedgen check --spec my_program.qedspec
qedgen check --spec my_program.qedspec --json           # machine-readable output

# Generate all committed artifacts from .qedspec
qedgen codegen --spec my_program.qedspec --all          # scaffolds Rust, Lean, Kani, tests, CI

# If Rust scaffolds were generated, the agent fills TODO business logic,
# then runs cargo check / cargo test until the scaffold is compile-clean.

# Or generate selectively
qedgen codegen --spec my_program.qedspec                # Rust handler scaffold only (agent-filled)
qedgen codegen --spec my_program.qedspec --lean         # + Lean proofs
qedgen codegen --spec my_program.qedspec --kani         # + Kani harnesses (spec-model)
qedgen codegen --spec my_program.qedspec --kani-impl    # + impl-targeted Kani (calls user's Anchor handler)
qedgen codegen --spec my_program.qedspec --test         # + unit tests
qedgen codegen --spec my_program.qedspec --proptest     # + proptest harnesses
qedgen codegen --spec my_program.qedspec --integration  # + in-process SVM integration tests

# Check with drift detection and verification report
qedgen check --spec my_program.qedspec --coverage       # operation × property matrix
qedgen check --spec my_program.qedspec --explain        # Markdown verification report
qedgen check --spec my_program.qedspec --code ./programs --kani ./programs/tests/kani.rs  # drift detection

# Repo maintenance gate: bundled examples match current codegen
qedgen check --regen-drift
```

### sBPF verification

sBPF-specific declarations (`instruction`, `pubkey`, per-instruction `errors`)
live inside `pragma sbpf { ... }` — the core DSL stays platform-agnostic, and
`qedgen` infers the assembly target from the pragma's presence.

```
spec Transfer

pragma sbpf {
  instruction transfer_sol { ... }
}
```

```bash
# Transpile sBPF assembly to Lean 4
qedgen asm2lean --input src/program.s --output formal_verification/Program.lean

# Verify sBPF proofs (checks source hash, regenerates if stale)
qedgen check --spec my_program.qedspec --asm src/program.s
```

### CPI contracts — `interface` + `call`

When your program invokes another (SPL Token, System Program, an AMM, …),
declare the callee's contract as an `interface` and write `call` at the
invocation site. The Rust side gets a real CPI builder; Lean proofs pick up
the callee's declared `ensures` as hypotheses.

```
interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts { from : writable, type token
               to   : writable, type token
               authority : signer }
    ensures amount > 0
  }
}

handler exchange : State.Open -> State.Closed {
  call Token.transfer(from = taker_ta, to = initializer_ta,
                      amount = taker_amount, authority = taker)
}
```

```bash
# Scaffold a Tier-0 interface from an Anchor IDL (shape only — no ensures)
qedgen interface --idl target/idl/jupiter.json --out interfaces/jupiter.qedspec

# Or vendor it into .qed/interfaces/<program>.qedspec (the canonical location
# for tool-managed library specs — pointed at by `.qed/config.json`)
qedgen interface --idl target/idl/jupiter.json --vendor
```

`qedgen check` emits `[shape_only_cpi]` for any `call` whose target lacks
`ensures`, making the gap between "my Rust compiles" and "my program is
verified" visible. See [docs/design/spec-composition.md](docs/design/spec-composition.md)
for the full tier model.

### Generate proofs from a prompt

```bash
qedgen generate \
  --prompt-file /tmp/analysis/property.prompt.txt \
  --output-dir /tmp/proof \
  --passes 4 \
  --validate
```

### Fill hard sub-goals

```bash
# Leanstral (fast, seconds)
qedgen fill-sorry \
  --file formal_verification/Spec.lean \
  --passes 3 \
  --validate

# Auto-escalate to Aristotle if sorry markers remain
qedgen fill-sorry \
  --file formal_verification/Spec.lean \
  --passes 3 \
  --validate \
  --escalate
```

### Aristotle (when Leanstral fails)

```bash
# Submit and wait inline
qedgen aristotle submit --project-dir formal_verification --wait

# Or submit, detach, and poll later
qedgen aristotle submit --project-dir formal_verification
qedgen aristotle status <project-id> --wait --output-dir formal_verification

# List / cancel
qedgen aristotle list
qedgen aristotle cancel <project-id>
```

### Upstream binary pinning

When a `.qedspec` `import`s another program's interface (e.g. SPL Token),
the import can pin an `upstream_binary_hash` — the SHA-256 of the on-chain
`.so`. `qedgen verify --check-upstream` diffs each pinned hash against
what's actually deployed via `solana program dump`, so a callee program
upgraded out from under your proofs surfaces as a verification failure
instead of a silent risk.

```bash
# Compare every pinned upstream hash to the on-chain bytes (auto-on when
# qed.lock declares any pinned binary_hash; pass --check-upstream
# explicitly in scripts / CI for safety).
qedgen verify --check-upstream

# Override the cluster (defaults to the one in ~/.config/solana/cli/config.yml)
qedgen verify --check-upstream --rpc-url https://api.mainnet-beta.solana.com

# CI gate — refuse to reach the network. Pinned-but-no-fetch reports as Error.
qedgen verify --check-upstream --offline

# Offline development — suppress the upstream check even when a pin is
# present. Mismatches demote to Info; verify exits zero. Do NOT use in CI.
qedgen verify --check-upstream --upstream-stale-ok
```

`qedgen verify --check-upstream` treats a mismatched pin as a **CRIT**
finding and exits non-zero. The same diff also runs under `qedgen check
--frozen`, where mismatches surface as **P2** warnings by default — the
spec ships green, the operator sees the warning. Pair with `--strict`
to escalate `check --frozen` mismatches to **CRIT** for release-blocking
CI.

```bash
# Local CI — warn on a stale pin but stay green
qedgen check --frozen

# Release CI — fail on a stale pin
qedgen check --frozen --strict
```

Requires the [Solana CLI](https://docs.solana.com/cli/install-solana-cli-tools)
on `PATH` (qedgen shells out to `solana program dump`). Combine with
`--proptest` / `--kani` / `--lean` to run the binary check alongside
the harness backends in one invocation. Network / CLI errors always
surface as P2 (never CRIT) so a missing Solana toolchain doesn't
silently false-positive CI.

### Verification drift detection

After verifying a function, stamp it with `#[qed(verified)]` to detect future changes — either to the function body *or* to its spec contract. `qedgen stamp` emits the attributes ready to paste, and it is gated: every `qedgen verify` run records its evidence to `.qed/verify-evidence.json`, and `stamp` refuses unless that record matches both the spec and program source being stamped and carries a passing **implementation-bound** backend (Miri or a `kani_impl` harness). Probe reproducers confirm findings rather than conformance, and checking or model-tested results are not eligible for `#[qed(verified)]`:

```bash
qedgen verify --spec my_program.qedspec --program ./programs/my_program --kani --kani-path ./programs/my_program/src/kani_impl.rs
qedgen stamp --program ./programs/my_program --spec my_program.qedspec
```

```rust
use qedgen_macros::qed;

#[qed(verified,
      spec = "my_program.qedspec",
      handler = "deposit",
      hash = "5af369bb254368d3",
      spec_hash = "c3d4e5f67890abcd")]
pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> {
    guards::deposit(&ctx, amount)?;
    // user business logic
}
```

Both hashes are pure compile-time checks — the macro expands to the function unchanged, so there's zero runtime cost. `hash` fires when the body changes; `spec_hash` fires when the `.qedspec` handler block changes.

```bash
# Unified drift report — Rust handlers + Lean theorems vs spec
qedgen reconcile --spec my_program.qedspec --json

# Scan and stamp hashes on all #[qed(verified)] functions
qedgen check --spec my_program.qedspec --drift programs/src/ --update-hashes

# CI gate — exit 1 if any verified function has changed
qedgen check --spec my_program.qedspec --drift programs/src/

# Transitive drift — also check if callees of verified functions changed
qedgen check --spec my_program.qedspec --drift programs/src/ --deep
```

`qedgen reconcile` is the agent-friendly entry point: it combines Rust-side `spec_hash` mismatches with Lean-side orphan/missing theorem findings into one machine-readable report, ready for an LLM to consume and act on.

### Discharge against pinned bytes (experimental, v3.0 target)

The bundled CPI-callee `ensures` and the sBPF refinement bridge are *axiomatized against a `binary_hash` pin* today — qedgen names the bytes, but doesn't yet prove they honor the contract. The discharge seam closes that gap by handing a name-level obligation to qedsvm's `qedlift`, which proves it against the decoded program bytes (offsets resolved from the IDL on the qedsvm side). See [`docs/design/qedsvm-discharge.md`](docs/design/qedsvm-discharge.md).

This is the **producer half** of that seam, scoped to the v1 soundness boundary: a single-field constant increment (`<field> += <int literal>`). Parameter deltas, non-`+=` ops, and multi-effect handlers are rejected.

```bash
# Emit the name-level refinement descriptor (JSON) qedlift consumes.
# Carries semantics only — account, mutated field name, constant delta.
qedgen descriptor --spec vault.qedspec --handler deposit

# Chain it end to end: build the descriptor, shell out to a built qedlift,
# and report a discharge verdict (sorry-free proof against the bytes).
qedgen discharge --spec vault.qedspec --handler deposit \
  --so target/deploy/vault.so --idl idl/vault.json \
  --qedlift path/to/qedlift
```

`--account` overrides the descriptor's account (default: the spec's first account type, else the program name) — use the IDL account name so qedlift can resolve offsets. No meaning crosses the boundary: `discharge` reads only qedlift's exit status and whether it emitted a sorry-free proof.

### Consolidate proofs

```bash
qedgen consolidate \
  --input-dir /tmp/proofs \
  --output-dir my_program/formal_verification
```

### File feedback as a GitHub issue

When `check`, `codegen`, or `verify` fails in a way you didn't expect — or you're stuck and want a maintainer to see your context — `qedgen feedback` bundles the last command's stderr, your environment, and the relevant `.qedspec` excerpt into a GitHub issue.

```bash
# Walk you through filing the last failure as an issue.
qedgen feedback --note "lint flags MathOverflow but my spec already declares it"

# Print the title/body to stdout without filing anything.
qedgen feedback --dry-run

# Skip the interactive prompt (CI, scripts).
qedgen feedback --yes
```

Submits via `gh issue create` if you're logged into GitHub CLI; otherwise prints a pre-filled URL. Override the target repo with `QEDGEN_FEEDBACK_REPO=owner/repo` (forks, internal mirrors). A local copy is always written to `.qed/feedback/<timestamp>.md` so nothing is lost if you skip the remote step.

### Generate CI workflow

```bash
qedgen codegen --spec my_program.qedspec --ci                    # Lean-only verification workflow
qedgen codegen --spec my_program.qedspec --ci --ci-asm src/program.s  # Add sBPF source hash check
qedgen codegen --spec my_program.qedspec --ci --ci-ratchet target/idl/my_program.json  # + ratchet readiness lint on every build
```

### Release gates

```bash
bash scripts/check-version-consistency.sh
bash scripts/check-readme-drift.sh
bash scripts/check-lake-build.sh --strict
qedgen check --regen-drift
cargo audit --deny warnings \
    --ignore RUSTSEC-2024-0436 --ignore RUSTSEC-2024-0388 \
    --ignore RUSTSEC-2025-0141 --ignore RUSTSEC-2025-0161 \
    --ignore RUSTSEC-2026-0097
cargo deny check
```

`cargo audit` and `cargo deny check` enforce the supply-chain gate
defined in `deny.toml`: zero unignored RustSec vulnerabilities, only
permissive licenses (MIT / Apache-2.0 / BSD / ISC / etc.), and only
`crates.io` as a dep source (no git-branch pins). The five ignored
RUSTSEC IDs cover `paste`, `derivative`, `bincode`, and
`libsecp256k1` maintenance notices plus `rand`'s custom-logger
unsoundness; all are transitive, justified in `deny.toml`, and do not
match QEDGen's runtime usage. Install once with `cargo install --locked
cargo-audit cargo-deny`; CI runs both in the dedicated `supply-chain`
job on every push and PR.

`qedgen check --regen-drift` regenerates bundled `examples/rust/*`
artifacts in temporary directories and fails if committed generated
support code, harnesses, or `Spec.lean` drift from the current generator.
Every generated example root must include `qed.toml`; examples without
imports can use an empty `[dependencies]` table.

`scripts/check-lake-build.sh` runs `lake build` in every bundled
`examples/*/formal_verification/` (rust + sBPF), surfacing
`Spec.lean` and `Proofs.lean` failures that the Rust-side gates
above don't catch. `--strict` fails on missing `.lake/`/manifests
(cold checkout — run `lake update` once first); drop `--strict` for
a non-release sanity check. Add `--only <pattern>` to scope to a
single example. The repository's Lean workflows are temporarily manual-only
because a cold example sweep can take more than two hours; routine push/PR CI
still runs generation snapshots and example drift checks. Run the Lake workflow
manually for Lean/codegen changes and before a release.

### Deploy-safety lint (ratchet)

`qedgen readiness` runs before the first deploy: one IDL in, a verdict out (`READY`, `UNSAFE`, or `BREAKING`) plus every specific future-upgrade landmine it finds. `qedgen check-upgrade` runs on every subsequent release: diff the deployed IDL against the candidate and fail the build on any change that would silently corrupt on-chain state, break existing clients, or orphan PDAs. Both work against Anchor IDLs (`anchor build`) and Quasar IDLs (`quasar build`) — the framework is autodetected from `Anchor.toml` / `Quasar.toml` in the working directory, or you can force it with `--quasar`.

```bash
# Pre-deploy — lint one IDL for mainnet-readiness
qedgen readiness --idl target/idl/my_program.json
qedgen readiness --idl target/idl/my_program.json --json          # machine-readable
qedgen readiness --idl target/idl/my_program.json --quasar        # Quasar IDL

# Post-deploy — diff old vs new and block breaking upgrades
qedgen check-upgrade --old ratchet.lock --new target/idl/my_program.json

# Acknowledge an intentional unsafe change
qedgen check-upgrade --old ratchet.lock --new target/idl/my_program.json \
  --unsafe allow-field-append --migrated-account EscrowState
```

Exit codes mirror ratchet's CLI conventions: `0 = additive/safe`, `1 = breaking`, `2 = unsafe`. Under the hood qedgen embeds [ratchet](https://github.com/saicharanpogul/ratchet) as a library, so the rule catalog stays in sync with upstream — run `qedgen readiness --list-rules` (P-rules) or `qedgen check-upgrade --list-rules` (R-rules) to see the full set. Pair with `--json` for a machine-readable dump. A worked Quasar IDL pair (v1 → v2) lives at [`crates/qedgen/tests/fixtures/quasar-readiness/`](crates/qedgen/tests/fixtures/quasar-readiness/).

**Why both.** qedgen's `#[qed(verified)]` hash-stamps the *function body*, so a rename of an `#[account]` struct compiles with a stale-but-valid proof even though the on-chain discriminator is now different and every existing account of that type is orphaned. `qedgen check-upgrade`'s `R006 account-discriminator-change` catches that class of failure; the proof layer alone doesn't look at it.

## Codegen internals

Starting in v2.30, `qedgen codegen` routes every backend (Lean, Kani,
Anchor / Quasar / Pinocchio, proptest) through a typed intermediate
representation (`mir::Mir`) instead of consuming the parsed spec AST
directly. The flip is transparent — no flag to enable, no behavior
change for any existing spec (verified via byte-equivalent snapshots
across every pilot fixture: 6 Lean × 6 Kani × 6 Anchor × 6 proptest =
24 fixture-snapshot lock-ins, plus an additional cross-program-vault
end-to-end check). The v2.31 Pinocchio scaffold is MIR-native from the
start — it has no legacy renderer and no escape hatch.

**Why it matters.** A typed IR replaces shared-by-convention dispatch
across the four codegens with a single `Stmt` enum every codegen has
to match exhaustively. Cross-codegen divergence (a new spec feature
that one backend understands and the others silently ignore) becomes
a compile error rather than a runtime drift. The codegen surface
isn't smaller yet — Lean / Kani / Anchor / proptest still live in
parallel `*_mir.rs` modules alongside the legacy `*.rs` modules
behind escape hatches — but the divergence-prevention payoff lands
immediately for any new feature added against MIR.

**No escape hatches.** As of v2.32 the migration is complete: the four
MIR codegens (`lean_gen_mir` / `kani_mir` / `codegen_mir` /
`proptest_gen_mir`) are the *sole* paths — there are no `QEDGEN_LEGACY_*`
env vars and no parallel legacy renderers. (sBPF specs emit Lean proofs
only; `--kani` / `--proptest` skip assembly targets, which are verified
via Lean + client-side tests.)

**Roadmap.**
- **v2.30** — MIR carry-through complete; legacy paths reachable via env vars during a soak.
- **v2.31** — soak.
- **v2.32** — **migration finished:** deleted the legacy `lean_gen.rs`, `kani.rs`,
  `proptest_gen.rs` and the legacy `codegen::generate`; removed all four
  `QEDGEN_LEGACY_*` hatches. Records + sBPF ported to MIR for Lean (Kani/proptest
  skip assembly); `codegen.rs`'s shared helpers live on as `codegen_shared.rs`.

## Examples

### Rust / Anchor

- **[Escrow](examples/rust/escrow/)** — Token escrow with lifecycle proofs
- **[Escrow (split)](examples/rust/escrow-split/)** — Escrow with handlers split across instruction files (multi-file `qed.toml` layout)
- **[Lending](examples/rust/lending/)** — Lending pool with multi-account state
- **[Multisig](examples/rust/multisig/)** — Multi-signature vault with voting

### sBPF Assembly

- **[Counter](examples/sbpf/counter/)** — PDA counter
- **[Tree](examples/sbpf/tree/)** — Red-black tree
- **[Transfer](examples/sbpf/transfer/)** — SOL transfer via System Program CPI
- **[Slippage](examples/sbpf/slippage/)** — AMM slippage guard

### Ratchet (Quasar IDL)

## Requirements

- Rust toolchain (auto-installed if missing)

Lean toolchain installs automatically the first time it's needed; API
keys must be obtained from the providers and exported by the user
before running the corresponding commands:

- Lean 4 / elan — for `lake build` and formal proofs (auto-installed)
- [Solana CLI](https://docs.solana.com/cli/install-solana-cli-tools) — only for `qedgen verify --check-upstream` (shells out to `solana program dump`). Install yourself.
- `MISTRAL_API_KEY` — for `fill-sorry` and `generate`. Sign up at [console.mistral.ai](https://console.mistral.ai) (free tier available).
- `ARISTOTLE_API_KEY` — for `aristotle` deep proof search. Sign up at [aristotle.harmonic.fun](https://aristotle.harmonic.fun).

### Environment variables

| Variable | Purpose | When needed |
|---|---|---|
| `MISTRAL_API_KEY` | Leanstral API access (`fill-sorry`, `generate`) | Lean proofs |
| `ARISTOTLE_API_KEY` | Aristotle long-running proof search | Hard sub-goals |
| `QEDGEN_HOME` | Override global home directory (default: `~/.qedgen`) | Always |
| `QEDGEN_VALIDATION_WORKSPACE` | Override validation workspace path | Lean proofs |

## License

[MIT](LICENSE)
