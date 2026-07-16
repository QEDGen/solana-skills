# QEDGen v2.43.0 — evidence-aware probes, domain fuzzing, and tree-native codegen

**Status:** released. **Scope:** 12 merged PRs / 20 commits since
v2.42.0. **Theme:** audit evidence becomes explicit and replayable, the auditor
gains a portable domain-invariant pipeline, and every active expression backend
now renders from the typed `ExprTree` rather than pre-rendered strings.

## 1. Probe evidence model and executable confirmation (#226, #232–#234)

- Probe output moves to **schema v3** with `candidates[]`, `engine_runs[]`,
  `coverage`, and a top-level `outcome`. An empty `findings[]` can no longer hide
  predicate hits, skipped files, a blocked harness, or a budget-zero dry run.
- `findings[]` keeps the reproducer-only contract. Static/spec hits without
  executable confirmation remain non-severity candidates.
- `ProbeOutput::envelope` is the single schema-version construction seam; fuzz
  and non-fuzz modes can no longer drift versions.
- Ambiguous `--program` combinations now fail at clap validation rather than
  silently skipping a requested fuzz/root/spec engine.
- Crucible minimizes and replays crashes, parses `[FUZZ_FINDING]` evidence, names
  the violated invariant/property, preserves distinct invariant findings, drops
  non-reproducing crashes, and exposes replay success in coverage.
- `ArithmeticOverflowWrapping` gains the first opt-in boundary witness:
  `probe --spec` generates a standalone harness; `--execute-repros` runs it and
  promotes a reproduced wrapping-semantic violation to a finding.

### Probe compatibility

Consumers that pin `version == 2` must migrate to schema v3. Existing v2 fields
remain present, but consumers must inspect `outcome`, `engine_runs`, and
`candidates` before interpreting an empty finding list. See
`docs/design/probe-schema-v3-migration.md`.

The boundary witness confirms the declared wrapping operator at a concrete type
boundary. It does **not** execute the deployed program or establish attacker
reachability; auditors must retain the implementation/source review step before
describing exploitability.

## 2. Domain-invariant audit and fuzz pipeline (#220–#221)

- Domain extraction is independent of ordinary probe/build success and records
  source-cited asset flows, quantities/units, paired operations, lifecycle,
  authorities, economic equations, and external assumptions.
- Versioned dossier, interview, sequence, binding, account-overlay, handoff,
  manifest, and replay-report schemas make the audit portable and resumable.
- `qedgen ratify` converts accepted domain facts into the spec/handoff while
  retaining rejected/deferred provenance.
- Crucible now has explicit `protocol`, `skeleton`, and `domain` modes. Domain
  execution requires ratified facts and executable assertions.
- Deterministic domain sequences bind explicit accounts/arguments, compile into
  byte-exact seeds, replay before exploratory fuzzing, and persist provenance
  hashes and signal-aware outcomes.
- Related codegen fixes cover PDA materialization, unresolved seeds, unsupported
  sums, environment bindings, external references, harness location independence,
  and preservation of agent-filled harnesses.

## 3. Tree-native MIR and backend rendering (#222, #224)

- `mir::Expr` is reduced to `{ tree, source_span }`; six per-target expression
  strings and their fallback rewriters are removed.
- CPI substitution, let bindings, Lean transitions/theorems, Rust/Kani/proptest
  guards and effects, unit tests, Pinocchio zeropod binding, and imported-account
  mirror routing all render from the typed tree.
- Spec-level `let` bindings now elaborate in Lean rather than leaking free
  variables. Unit-test guards now render real `requires` predicates rather than
  vacuous `true` guards.
- `RefImpl` remains intentionally verbatim because it is a whole function body,
  not a cross-backend expression carrier.
- Generated examples and the widened snapshot corpus pin parity across Anchor,
  Pinocchio, imported interfaces, ADT state, CPI, and let-binding paths.

## 4. Generated Rust formatting seam (#217)

- Every generated `.rs` file routes through one rustfmt-aware write seam.
- Formatting happens before verified-body hash stamping, preventing permanent
  macro/hash drift from rustfmt's token changes.
- Large inline Pinocchio/Crucible templates are real `.rs` template files and a
  dedicated integration gate keeps templates and Rust snapshots fmt-clean.
- rustfmt remains a soft dependency: unavailable/rejected formatting preserves
  the original generated source and emits one warning.

## 5. Kani model expansion (#216)

- Deterministic uninterpreted `UfMap32`/`UfMap64` models back PDA, hash, keccak,
  blake3, and secp256k1 stubs, with length-delimited keys and injectivity axioms.
- The vendored Kani prelude expands to 15 machine-checked helper contracts.
- New `Bytes32`/`Bytes64` DSL/MIR types cover hashes and signatures through all
  codegens, with conservative unwind-floor guidance.
- Vec-reading properties without an explicit `kani_vec_bound` now warn, and
  numeric pragma values parse correctly.

## 6. Auditor reliability and packaging (#218–#219)

- The auditor entrypoint is compact and venue-neutral, with deterministic
  target/spec/runtime preflight, bounded profiles, explicit evidence states,
  strict reproducer handling, installed-copy drift checks, and a portable bench
  skill.
- The former monolithic handbook is split into routed manual-review, category,
  and reporting references without dropping content.
- npm packaging now includes the auditor bench skill and runs the skill,
  domain-artifact, corpus, and preflight contract gates.
- Release preparation also closes a packaging hygiene gap: nested Rust/Lean
  build caches, local agent state, audit findings, review artifacts, and debug
  binaries are now excluded from npm tarballs. The final dry-run must be
  inspected before publish.

## Compatibility and known limits

- No DSL removals. The probe JSON schema changes from v2 to v3; strict consumers
  must update.
- `--program` combined with `--fuzz`, `--root`, or `--spec` is now rejected
  instead of silently ignoring one requested engine.
- `--execute-repros` currently has one O(1) boundary witness and does not yet
  enforce its declared subprocess timeout. Heavier future reproducers must add
  bounded child-process execution before using this path.
- Crash classification is bounded even under a crash storm. Replay triage
  deterministically samples at most 32 crashes and four value variants per
  action shape, has a 30-second total ceiling and a two-second per-replay
  timeout, kills the complete subprocess group on timeout, and skips unbounded
  `tmin --all` work for oversized crash sets. Coverage reports replay incomplete
  whenever sampling or a deadline prevents full classification. The live
  domain-boundary smoke and a 3,747-crash regression both pass.

## Release gates

The release PR must show green results for version consistency, auditor/domain
contracts, rustfmt, clippy with warnings denied, the full Rust workspace suite,
generated-example drift, frozen locks, zero unintended Lean `sorry`, strict Lean
builds, Kani prelude proofs, cargo-audit with the documented ignore set, and
cargo-deny. See `docs/RELEASING.md` for the exact commands.
