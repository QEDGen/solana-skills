# CLI: probe and ratify

`probe`, `ratify` — the audit data layer and spec elicitation.

Part of the [CLI Reference](../cli.md).

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

