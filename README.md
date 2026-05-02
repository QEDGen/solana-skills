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

Write what your Solana program must guarantee in a `.qedspec` file. QEDGen validates the spec, finds bugs your tests miss, then generates the verification artifacts and implementation scaffold needed to keep them fixed: **property tests**, **Kani harnesses**, **Lean 4 proofs**, **agent-fill program scaffolds**, and **CI workflows** — all from a single source of truth. Frameworks: **Anchor** and **Quasar** (greenfield scaffold via `qedgen init --target ...`), plus **sBPF assembly**.

NOTE: Project is alpha stage, we are constantly shipping. So there would be bugs and breaking API changes.

```bash
npx skills add qedgen/solana-skills
```

> Works with Claude Code, Cursor, Windsurf, GitHub Copilot, and any agent supporting the [Agent Skills](https://agentskills.io) spec.

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
| **Conservation** | Custom invariants (token totals, vault bounds) preserved across operations |
| **Arithmetic safety** | Overflow/underflow for fixed-width integers, U64 bounds |
| **Input validation** | Account count, duplicates, data length, discriminators, parameter bounds — each guard maps to a specific error exit |
| **Memory correctness** | Stack/heap disjointness, pointer arithmetic (sBPF) |
| **PDA integrity** | Program-derived address derivation and 4-chunk comparison (sBPF) |
| **Deploy safety** | On-chain shape for Anchor **and Quasar** programs — version fields, reserved padding, pinned discriminators, signer coverage, PDA seed continuity — via `qedgen readiness` and `qedgen check-upgrade` (ratchet). |

CPI calls are axiomatic — we verify the program passes correct parameters. SPL Token internals and the Solana runtime are trusted.

**Proofs prove correctness. Ratchet proves deployability.** The P-rule preflight (`qedgen readiness`) catches future-upgrade landmines in a single IDL before the first deploy; the R-rule diff (`qedgen check-upgrade`) catches every breaking change between an old and new IDL once the program is live.

## Quick start

```bash
# 1. Install
npx skills add qedgen/solana-skills

# 2. Initialize the project — records the spec path in .qed/config.json
qedgen init --name my_program --spec my_program.qedspec --target anchor

# 3. Validate and generate artifacts (no --spec needed from inside the project)
qedgen check
qedgen codegen --all

# 4. Fill generated Rust handler TODOs, then run backend verification
qedgen verify

# 5. Audit a brownfield project before adopting a spec — emits the
#    auditor work list (per-handler categories) consumed by the
#    `qedgen-auditor` agent skill, or run spec-aware against an
#    existing .qedspec for category-coverage findings.
qedgen probe --spec my_program.qedspec
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

### Existing Anchor programs (brownfield)

`qedgen adapt` and `qedgen spec --idl` both target Anchor today;
brownfield ingest for Quasar isn't yet wired (Quasar greenfield via
`qedgen init --target quasar` is fully supported — see below).

```bash
# Option A — from an Anchor IDL (program ABI only)
qedgen spec --idl target/idl/my_program.json

# Option B — from the Anchor program's source (recommended)
# Walks src/lib.rs, finds the #[program] mod, follows each forwarder
# (inline / free fn / Type::method / ctx.accounts.method), and emits
# a .qedspec skeleton with one handler block per instruction plus a
# breadcrumb to where each handler body lives.
qedgen adapt --program ./programs/my_program

# Review and complete the TODO items in the generated .qedspec
# Then use the same pipeline as greenfield:
qedgen init --name my_program
qedgen codegen --spec my_program.qedspec --lean
cd formal_verification && lake build
```

`qedgen adapt` carries forward what it can read from the source: handler names, argument types, the `Context<X>` accounts struct, and a pointer to the actual handler body in your repo. Lifecycle, requires, effects, and transfers stay as TODOs for you or your agent to fill in. `qedgen spec --idl <path>` is the IDL-only fallback when you don't have source.

Once the spec is filled in, gate CI on it staying in sync with the program:

```bash
# Errors if the spec declares a handler that's not in the program
# (stale spec) or a `pub fn` that's not modelled in the spec
# (uncovered handler). Pure read; no codegen, no writes.
qedgen check --spec my_program.qedspec --anchor-project ./programs/my_program
```

### Greenfield — Anchor or Quasar

The same `.qedspec` codegens to either framework via `--target`. Anchor
is the default; pass `--target quasar` for a Blueshift Quasar
(`#![no_std]` + `quasar_lang`) program crate with explicit
discriminators and `Ctx<X>` instead of `Context<X>`.

```bash
# Anchor (default)
qedgen init --name my_program --spec my_program.qedspec
qedgen codegen --spec my_program.qedspec --all

# Quasar
qedgen init --name my_program --spec my_program.qedspec --target quasar
qedgen codegen --spec my_program.qedspec --target quasar --all
```

Lean proofs, Kani harnesses, proptest harnesses, and CI workflows are
target-agnostic — they're driven by the spec, not the framework, so
the verification artifacts are identical across `--target` choices.
The deploy-safety lint (`qedgen readiness` / `qedgen check-upgrade`)
also speaks both Anchor and Quasar IDLs; see the *Deploy-safety lint*
section below.

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
qedgen codegen --spec my_program.qedspec --kani         # + Kani harnesses
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
# Compare every pinned upstream hash to the on-chain bytes
qedgen verify --check-upstream

# Override the cluster (defaults to the one in ~/.config/solana/cli/config.yml)
qedgen verify --check-upstream --rpc-url https://api.mainnet-beta.solana.com

# CI gate — refuse to reach the network. Pinned-but-no-fetch reports as Error.
qedgen verify --check-upstream --offline
```

Requires the [Solana CLI](https://docs.solana.com/cli/install-solana-cli-tools)
on `PATH` (qedgen shells out to `solana program dump`). Combine with
`--proptest` / `--kani` / `--lean` to run the binary check alongside
the harness backends in one invocation.

### Verification drift detection

After verifying a function, stamp it with `#[qed(verified)]` to detect future changes — either to the function body *or* to its spec contract:

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

### Consolidate proofs

```bash
qedgen consolidate \
  --input-dir /tmp/proofs \
  --output-dir my_program/formal_verification
```

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
```

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
single example.

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

Exit codes mirror ratchet's CLI conventions: `0 = additive/safe`, `1 = breaking`, `2 = unsafe`. Under the hood qedgen embeds [ratchet](https://github.com/saicharanpogul/ratchet) as a library, so the rule catalog stays in sync with upstream — run `qedgen readiness --list-rules` (P-rules) or `qedgen check-upgrade --list-rules` (R-rules) to see the full set. Pair with `--json` for a machine-readable dump. A worked Quasar example lives at [`examples/quasar-readiness/`](examples/quasar-readiness/).

**Why both.** qedgen's `#[qed(verified)]` hash-stamps the *function body*, so a rename of an `#[account]` struct compiles with a stale-but-valid proof even though the on-chain discriminator is now different and every existing account of that type is orphaned. `qedgen check-upgrade`'s `R006 account-discriminator-change` catches that class of failure; the proof layer alone doesn't look at it.

## Examples

### Rust / Anchor

- **[Escrow](examples/rust/escrow/)** — Token escrow with lifecycle proofs
- **[Escrow (split)](examples/rust/escrow-split/)** — Escrow with handlers split across instruction files (multi-file `qed.toml` layout)
- **[Lending](examples/rust/lending/)** — Lending pool with multi-account state
- **[Multisig](examples/rust/multisig/)** — Multi-signature vault with voting
- **[Percolator](examples/rust/percolator/)** — Perpetual DEX risk engine

### sBPF Assembly

- **[Counter](examples/sbpf/counter/)** — PDA counter
- **[Tree](examples/sbpf/tree/)** — Red-black tree
- **[Dropset](examples/sbpf/dropset/)** — On-chain order book
- **[Transfer](examples/sbpf/transfer/)** — SOL transfer via System Program CPI
- **[Slippage](examples/sbpf/slippage/)** — AMM slippage guard

### Ratchet (Quasar IDL)

- **[Quasar readiness](examples/quasar-readiness/)** — `qedgen readiness` and `qedgen check-upgrade` against a Blueshift Quasar IDL, plus a v1 → v2 diff that exercises the breaking-change rules

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
