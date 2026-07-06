# QEDGen v2.41.0 — v3.0-prep simplification pass

**Status:** shipped (PR #160, tracker: whole-codebase review 2026-07-05).
**Theme:** the post-v2.35 cleanup greenlight — kill the duplication-with-
sync-comments defect class, delete confirmed-dead code, and split the god
functions, **without changing generated output** except three deliberately
reviewed behavior fixes. No new DSL features; no breaking changes (those
stay reserved for v3.0).

Net **≈ −2,100 LOC** across the eight tranches (plus the example removals),
and roughly **ten "keep in sync" contracts eliminated** — four of which had
already silently drifted.

## What shipped

### 1. Shared hash-core crate (T3) — a realized drift-bug class, structurally killed

`crates/qedgen-hash-core/` now owns the single canonical spec/body hashing
(`sha256_hex16`, `canonical_token_string`, `extract_handler_block`,
`normalize_spec_block`, `spec_context_digest`, `scan_balanced_block`); the
CLI (`crates/qedgen`) and the proc macro (`crates/qedgen-macros`) both
depend on it and their hand-kept byte-for-byte mirrors are deleted. This is
the exact divergence that produced the `--update-hashes` bug (drift.rs
`to_string()` vs macro `canonical_token_string`). A new
`tests/stamp_crosscheck.rs` hard-asserts the checked-in
`#[qed(verified, hash = …)]` stamp values, so the two paths can never drift
again.

### 2. Byte-identical dedup across the codegen backends (T4)

One `resolve_account_view` helper replaces nine pasted kani account-view
ladders; one `render_adt_wrapper` / `consistent_variant_fields` replaces the
ADT-wrapper rendering in multiple emit sites; the Anchor/Quasar `kani_impl`
emitters (90 % identical) merge with their header text as data; the SPL/System
CPI builders are parameterized by module path; a shared `lean_names` module
replaces the "keep in sync" `safe_name`/`param_sig_str`/`map_type` mirrors;
the R28 PDA-check firing predicate is unified between guard emission and
error-enum emission (drift here meant guards referencing an `InvalidPda`
variant `emit_errors` never emitted). The multi_account/single-account
liveness+environments drift is healed (the single-account side gained the
bare-field rewrite the multi side already had).

### 3. Behavior fixes (T5) — each snapshot-reviewed, deliberately separated

- **`unit_test.rs` migrated to the shared guard/effect lowering.** It now
  renders Lean `=` and the `and`/`or` connectives every other backend
  already handled, and lowers effects through `stmt_effect_triple` instead
  of string-matching raw `op.effects`. The generated multisig unit tests
  **now compile and run** — three generated-compile-error classes
  (`_member_index` param detection, `[ident as usize]` array subscripts,
  pubkey literal shape) previously kept the file from building at all; the
  multisig-hardcoded seed field names are gone, derived from the spec state
  instead.
- **Fingerprint keys for the proptest/crucible banners.** `compute_fingerprint`
  now inserts the `tests/proptest.rs` and `fuzz/src/main.rs` sections those
  banners look up, so they carry a `spec-hash:` line instead of shipping
  hash-less — closing a latent drift-detection gap.
- **One `Expr` walker spine.** A single exhaustive `for_each_child`
  (`ast.rs`, no `_` arm — the `Stmt` discipline extended to expressions)
  replaces five hand-rolled recursive walkers. `walk_apps` gains descent
  into `Match`/`Let`/`IfThenElse`/record/`Ctor`/`Field` positions it
  silently skipped, so the typechecker no longer misses helper calls in
  those positions. Triage over all bundled specs + fixtures: zero lint
  deltas; a regression test pins the gained descent.

### 4. God-function splits (T7)

`check_completeness` (`check/lints/mod.rs`) drops from ~3,400 to ~260 lines —
Rules 1–17 are extracted into per-family submodules behind a small `LintCtx`,
with their ~1,770 lines of tests migrated next to them. `generate_guards`
splits 625 → 212 lines along its labeled seams; the Pinocchio impl-harness
emitter 383 → 45 lines plus five named emitters; `run.rs` parses each spec
once per dispatch arm (Codegen was reparsing up to 9×) with the stage logic
lifted into `verify::`/`check::`. One `fs_walk::collect_rs_files` (documented
union skip-list) replaces fifteen hand-rolled recursive `.rs` walkers; drift
and reconcile share one syn-based `#[qed(verified)]` walker (a ~180-line
byte scanner deleted).

### 5. Constructors / `Default` / builders (T6), dead-code and allow scrub (T1/T2), test harness (T8)

A `CompletenessWarning` builder collapses ~60 literals; `BackendReport`
constructors 17 more; `#[derive(Default)]` on `ParsedHandler` /
`PinocchioHandlerProfile` kills the six/five 30-field literals; the probe
`Category::tag()` derives its 14 hand-duplicated tag sites. T1 deleted the
206-line dead `generate_imported_mirror`, the dead `ParsedOperation` model,
and the stale `QEDGEN_LEGACY_*` test plumbing; T2 removed 117 stale
`#[allow(dead_code)]` attributes and the genuinely-dead items the compiler
then surfaced. T8 collapses the four snapshot suites onto one shared
`SnapshotHarness` and fixes the stale-binary footgun once (the harness now
always rebuilds `qedgen`).

## Deliberate behavior changes (beyond generated-output parity)

- **`pinocchio_profile` regex fallback dropped.** Unparseable Rust no longer
  silently under-infers — profiling now fails loudly, naming the offending
  file and line, surfaced as a warning at the kani-impl consumer. (≈ −700 LOC.)
- **`--integration-output` default moved** from `./src/integration_tests.rs`
  to `./programs/tests/integration_tests.rs` — it no longer scaffolds an
  integration test *inside* the source tree, matching every sibling artifact
  flag. `regen-drift` probes both the new and legacy paths so existing
  projects keep drift coverage.
- **Probe/adapt file walkers** use the canonical union skip-list; the probe
  finding-set is unchanged (verified across the corpus), but
  `crucible_brownfield` now honors the skips it previously lacked.

## Example set

- **`percolator` removed.** Its checked-in stamps were stale (stale on the
  v2.40.0 binary too); the codegen/parse coverage it uniquely carried
  (records, `Fin`, ADT state, `Map[N] Record`, per-requires error names)
  moved to the `issue-8/pool.qedspec` fixture and inline unit-test specs.
- **`dropset` sBPF fixture replaced.** It was vendored third-party content;
  its only role here was as the sole old-syntax (`instruction`-block) spec
  behind the #88 regression gate and the `render_sbpf` golden. A generic
  `vault_lock_sbpf.qedspec` exercises the identical DSL surface (all guard
  shapes, both layouts, all five property kinds).

## Verification

Every tranche merged with a green `cargo test` and byte-identical snapshots
except the T5 regens and one reviewed 3-line cosmetic proptest hunk;
repeated old-vs-new binary A/B sweeps (check / codegen / probe / reconcile
over escrow, lending, multisig, cross-program-vault, sbpf) confirmed parity.
Final gate: **1,170 tests / 0 failures**, `clippy -D warnings` clean, fmt
clean, supply-chain (`cargo audit` + `cargo deny`) clean, example Lean
builds green.

## Deferred to v3.0 (unchanged)

`project/fill.rs` + the `--fill`/`--fill-tests` plumbing (~650 LOC) remains
soft-deprecated, slated for hard removal at the v3.0 cut. Follow-ups
surfaced during the pass: fold `requires` into the generated unit-test guard
fns (the multisig `*_guard_rejects_invalid` tests are vacuous until then),
insert the `kani_impl` framework fingerprint keys, and the
`write_without_read` lint's run-to-run output ordering.
