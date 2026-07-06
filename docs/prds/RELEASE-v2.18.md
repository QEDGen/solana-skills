# Release v2.18.0 — Named invariants, named counterexamples, Crucible fuzz engine, agent-fill doctrine

v2.18 folds in the v2.17 invariants work (no v2.17 tag was cut) and adds
Crucible-as-probe-engine as the second probe path alongside the existing
pattern-match predicates. Findings from both engines flow into the same
`Finding` / `Reproducer` surface so the auditor subagent and JSON
consumers see them uniformly. The release also formalizes an
**agent-fill-as-direct-edit doctrine** — no new `--fill` verbs for new
backends, and the existing `--fill` / `--fill-tests` are soft-deprecated.

## What's in

### Named invariants — handler-side preserves / establishes (v2.17 carry-over)

`.qedspec` invariants now generate per-handler preservation harnesses
in Kani and proptest, not just Lean theorems. The handler-side
`invariant Foo` clause means *preserves Foo* (assume pre, assert post);
the new `establishes Foo` keyword means *establish only* (no
pre-assume) and is the right shape for init / one-shot handlers.

- **`ParsedInvariant.rust_expr` end-to-end.** Adapter populated this
  field since v2.14 but only Lean emitted; v2.18 wires it through
  `rust_codegen_util::emit_invariant_predicates` (shared helper) into
  both `kani.rs` and `proptest_gen.rs`. Each `(handler, invariant)`
  pair where the handler carries `invariant Foo` or `establishes Foo`
  emits a `verify_{handler}_{preserves|establishes}_{inv}` Kani
  harness and a parallel proptest. Kani's summary line counts the new
  harnesses.
- **`establishes` keyword.** New `HandlerClause::Establishes(String)`
  variant + `ParsedHandler.establishes: Vec<String>`. Parsed
  identically to `invariant Foo`; backends pick the right harness
  shape (`is_establish` skips the pre-state assume).
- **Two regression fixtures** under
  `examples/regressions/invariants/`: `repro-handler-invariant-clause.qedspec`
  (preserves) and `repro-establishes-clause.qedspec` (establishes).
  Two new `chumsky_adapter::tests` consume them.

### Named counterexamples in `qedgen verify` human output (v2.17 carry-over)

`verify_kani_parse` and the proptest parser already extracted spec-named
`(var, value)` assignments from backend output; only the human renderer
in `verify::format_human` was skipping them. v2.18 renders them as
column-aligned `name = value` rows under each failing backend, with
failure message, source location, and proptest seed when present.
JSON consumers saw the data all along via `BackendReport.counterexamples`.

Sample output:

```
[FAIL] kani       (4567 ms)
       counterexample: probe_overflow_transfer
         assertion failed: post == pre.checked_add(amount).unwrap_or(0)
         at tests/kani.rs:42:5
           pre    = 18446744073709551615ul
           amount = 1ul
           post   = 0ul
```

4 unit tests in `verify::tests` pin the format. This is the substrate
the agent-driven iteration loop needs: agent sees the failing values,
proposes the spec edit (tighten `requires`, mark effect `+=!`/`+=?`),
re-runs.

### Crucible-as-probe-engine — three-phase integration

v2.18 adds [Crucible](https://github.com/asymmetric-research/crucible)
(asymmetric-research, MIT) as a second probe engine alongside the
pattern-match predicates in `probe.rs`. The pattern-match path runs
static predicates over the spec; the Crucible path runs coverage-guided
fuzzing of the deployed `.so` and converts each crash into a `Finding`
with `Reproducer::Crucible`. Both engines emit into the same
`findings[]`.

**P1 — `qedgen codegen --crucible`** (new module `crucible_gen.rs`)

Mechanical translation from `.qedspec` to a Crucible fuzz harness at
`fuzz/<prog>/`. State → `Fixture` struct fields with `#[derive(Clone)]`.
Handlers → `action_*` methods. Spec invariants with `rust_expr` →
`invariant_test` body with `fuzz_assert!`. Auth-named identities →
`Rc<Keypair>` signers. Action call shape:
`.program(id).call(instruction::X { args }).accounts(todo!()).send()` —
the `accounts::X { ... }` literal emits `todo!()` because the Anchor
accounts struct is richer than the spec's accounts block carries
(PDA derivation, sysvars). The agent fills these in place via
Read/Edit; **no `--fill --crucible` flag** (see doctrine below).
Workspace=[] in Cargo.toml isolates Solana v3 + Anchor 1.0.1 from
the parent crate (which may pin earlier). Anchor target only in
v2.18; sBPF specs error early.

**P2 — `qedgen probe --fuzz <budget>`** (new module `crucible_probe.rs`)

Drives discovery → build → smoke → run → triage → dedupe. Defaults
optimized for actionable findings out of the gate:

- **Auto-`tmin` every crash via `crucible tmin --all`** before
  surfacing. Minimization is implicit in the pipeline, not a
  post-step the user runs. Minimized chains are typically 1-3 actions.
- **Smoke pre-flight (~30s)** with `SMOKE_FINDING_CAP=4`: if smoke
  surfaces ≥4 distinct findings, stop early and surface those instead
  of burning the full budget on duplicates. Bypassable via `--no-smoke`.
- **Auto-discover Anchor IDL** at `target/idl/<prog>.json`, symlink
  into the harness `idls/` (idempotent — pre-existing files preserved).
- **Crash categorization** by `(success, error_code)`: invariant
  violation → High (semantic bug), runtime abort with `Custom(N)` →
  Medium ("spec is silent on this error path"), runtime panic without
  error code → Medium, build error / timeout → suppress (no evidence,
  no finding per `feedback_probes_reproducible_only`).
- **Dedupe by `(handler, category_tag, error_code-or-0)`** — first
  crash per pair is the canonical reproducer; subsequent crashes
  accumulate in `Reproducer::Crucible.extra_seeds`.
- **Stateless default**; `--stateful` opt-in for chain coverage.

`Reproducer::Crucible` carries `harness_path`, `crash_path`, a
ready-to-run `crucible show <harness> <crash> --replay` invocation,
the minimized `action_sequence`, `extra_seeds`, and the resolved
Crucible binary version for re-validation pinning.

**P3 — `qedgen verify --crucible <budget>`**

Thin alias over P2: wraps `run_fuzz_probe` findings as a
`BackendReport` with per-finding `Counterexample`s, so action
sequences render through the same v2.18 named-trace human surface as
Kani / proptest. The crucible backend runs independently of other
backends — its failure surfaces as one `BackendReport` without
blocking proptest / Kani / Lean reports.

**Plumbing.** New `Reproducer::Crucible` enum variant. New
`CrucibleCrashMetadata` struct (serde replica of Crucible's
`<hash>.meta.json` schema — we don't pull `crucible-fuzz-cli` as a
library because the LibAFL transitive dep tree is heavy). New
`deps::require_crucible()` point-of-use check with install hint.
`Category::CrucibleFuzzCrash` variant in `probe::Category`.

### Agent-fill-as-direct-edit doctrine

After landing P1 we considered shipping a `qedgen codegen --crucible --fill`
flow paralleling the existing `--fill` for handler bodies. We decided
not to:

- The existing `--fill` emits prompts to stdout for the agent to
  consume; the agent then reads the prompt and edits the file. But
  the agent already has Read/Edit tools — the prompt-emission layer
  is the agent talking to itself.
- For new backends (Crucible, future), the SKILL.md teaches the agent
  the direct-edit pattern: grep for `todo!()`, read the spec block, edit
  the literal in place. Then `cargo build`; iterate.

**Soft-deprecation.** The existing `qedgen codegen --fill` and
`--fill-tests` flags print a deprecation warning when invoked and
still run as before — no breaking change in v2.18.x. Scheduled for
hard removal in v3.0 alongside the spec-cleanup work.

Aligns with [agent+LSP is QEDGen's analysis substrate] — for `--fill`,
the prompt-emission layer **is** the syn/AST scanner: redundant work
the agent's own tools handle natively.

### Real-data regression fixture

`crates/qedgen/test-fixtures/real-crucible-crash.meta.json` is an
actual crash captured from `crucible run` against Crucible's bundled
escrow example. The 6-action chain ends in a seeded `withdraw at slot
10 should have been rejected` violation. Three new
`crucible_probe::tests` parse + categorize + handler-derive against
this real shape rather than synthetic JSON — a hedge against silent
schema drift if Crucible's `.meta.json` format changes upstream.

### Validation findings folded in

Live validation against Crucible v0.x surfaced three CLI/API
mismatches the PRD couldn't catch from documentation alone:

1. `crucible run` takes `-C <dir>`, not cwd-relative invocation
2. `crucible tmin --all` is the right shape (single subprocess); the
   per-crash form expects filename-only with no `--timeout` flag
3. `.send()` returns `Result<TxOutcome, _>` — codegen now does
   `.as_ref().map(|o| o.is_success()).unwrap_or(false)` instead of
   the documented-but-wrong `.is_success()` direct call

All three fixed before tagging.

## What's deferred

- **`accounts::X { ... }` literal fill prompt.** v2.18 emits `todo!()`
  at each accounts struct site. The agent fills via direct edit (per
  doctrine above) — no follow-up `--fill` verb is planned.
- **Anchor 0.29 IDL conversion.** v2.18 requires Anchor 0.30+ IDL
  JSON (Crucible's `crucible_idl_gen` macro detects `kind ==
  "rootNode"` for Codama vs. Anchor 0.30+). Older IDLs need
  `anchor idl convert` first; we don't shell that out.
- **sBPF / Pinocchio / Quasar Crucible emission.** Anchor target only
  in v2.18. sBPF specs (`pragma sbpf`) error early.
- **Lean per-handler preservation theorems for invariants.** Today's
  Lean emits the invariant as a standalone sorried theorem; the
  per-(handler, invariant) preservation theorems are a separate piece
  of work touching the bundled example `Spec.lean` files. Estimated
  2-3 days; backlog.
- **Dedupe by assertion message.** v2.18 dedupes by error code; the
  more precise dedupe (re-run `crucible show --replay` and parse the
  `FUZZ_FINDING` line for the actual assertion message) is v2.18.1.

## Migration

- **`Category` enum has a new variant** (`CrucibleFuzzCrash`).
  Consumers exhaustively matching on `Category` need to add an arm.
  The enum is `#[derive(Serialize)]` with `rename_all = "snake_case"`,
  so JSON consumers see `"category": "crucible_fuzz_crash"` and
  shouldn't be affected unless they pattern-match.
- **`Reproducer` enum has a new variant** (`Crucible { ... }`).
  Same applies — JSON deserializers pinned by `#[serde(tag = "kind")]`
  see a new tag value.
- **`qedgen codegen --fill` / `--fill-tests` print a deprecation
  warning** but continue to run. No script changes required in
  v2.18.x; v3.0 will remove the flags. New backends should follow
  the direct-edit pattern documented in SKILL.md.
- **`qedgen probe` schema bumps to `version: 1` with a new optional
  `--fuzz` flag.** Existing JSON consumers pinning the pattern-match
  finding shape continue to work; Crucible findings flow through the
  same `findings[]` with `category: "crucible_fuzz_crash"`.
- **`qedgen verify` gains 4 new flags** (`--crucible`,
  `--crucible-harness-dir`, `--crucible-no-smoke`, `--crucible-stateful`).
  Default behavior unchanged when `--crucible` is omitted.

## Release gates

- `cargo fmt --check` — clean
- `cargo clippy -- -D warnings` — clean
- `cargo test` — **543 lib tests + 4 ignored integration + 1 manifest test, 0 failures**
  (was 514 in last tagged release; +29 across `crucible_gen` (8),
  `crucible_probe` (21 including 3 real-data), `verify::tests` (4
  named-counterexample render), 2 new `chumsky_adapter` tests for
  `establishes` / handler-invariant linkage, minus a few that
  collapsed during refactoring)
- `bash scripts/check-readme-drift.sh` — clean (17 commands documented)
- `bash scripts/check-version-consistency.sh` — 2.18.0 across
  Cargo.toml + package.json
- `bash scripts/check-lake-build.sh --strict` — every
  `examples/*/formal_verification/` builds clean
- Zero unintended `sorry` in `examples/**/*.lean` (CLAUDE.md filter
  applied; matches are all macro doc-comments / template strings, not
  proof tactic positions)
- `qedgen check --frozen` lock-currency check passes for all 5
  bundled spec dirs (escrow, escrow-split, lending, multisig,
  and the perp-dex example). Multisig has 1 pre-existing
  `excluded_op_modifies_property` warning (predates v2.17, tracked
  separately).

## Carryover from v2.16.1

No changes to: pattern-match probe predicates, Mollusk sandbox
infrastructure, structured counterexample types
(`Counterexample`, `CounterexampleVar`), upstream binary hash
pinning, ratchet (`qedgen readiness` / `qedgen check-upgrade`),
Lean proof toolchain, Aristotle / Leanstral integrations,
Anchor / Quasar codegen targets.
