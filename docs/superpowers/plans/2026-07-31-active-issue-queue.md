# Active Issue Queue Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close #398, #399, #395, #401, #402, and #400 with independently tested changes in one reviewable pull request.

**Architecture:** Reuse existing source discovery, IDL overlay, category types, artifact gates, and auditor validation entry points. Each issue is one self-contained commit in priority order; compatibility remains unchanged for callers that do not opt into new behavior.

**Tech Stack:** Rust 2021, Clap, `solana-ratchet-core`, Bash, jq, JSON Schema draft 2020-12, GitHub Actions.

## Global Constraints

- #341 remains blocked on Crucible and is not modified.
- #87 remains deferred and is not modified.
- #123 remains an open non-blocking roadmap tracker.
- Existing `readiness` and `check-upgrade` behavior is unchanged without `--root`.
- No test introduced by this work may require network access.
- Every production behavior change starts with a focused failing test.
- Each issue workstream lands as its own commit.

---

### Task 1: Source-aware readiness and upgrade reports (#398)

**Files:**
- Modify: `crates/qedgen/src/cli.rs`
- Modify: `crates/qedgen/src/run.rs`
- Modify: `crates/qedgen/src/verify/ratchet.rs`
- Modify: `crates/qedgen/src/probe/idl_overlay.rs`
- Modify: `crates/qedgen/src/probe/mod.rs`
- Test: `crates/qedgen/src/verify/ratchet.rs`
- Test: `crates/qedgen/tests/ratchet_cli.rs`
- Reuse fixture: `crates/qedgen/tests/fixtures/probe-corpus/specless/anchor-idl`

**Interfaces:**
- Consumes: existing `probe::idl_overlay` source/IDL reconciliation and Ratchet `Report`.
- Produces: `ReadinessOpts { idl, framework, root: Option<PathBuf> }` and `CheckUpgradeOpts { ..., root: Option<PathBuf> }`.
- Produces: `probe::idl_source_drift(root: &Path, idl: &Path) -> anyhow::Result<IdlSourceDrift>`, where `IdlSourceDrift` contains sorted `source_only` and `idl_only` handler names.

- [ ] **Step 1: Add failing library tests for both drift directions**

  In `verify/ratchet.rs`, construct options with the existing Anchor drift
  fixture and assert:

  ```rust
  assert!(report.findings.iter().any(|f|
      f.rule_id == "QED001" && f.path == ["ix:emergency_withdraw"]));
  assert!(report.findings.iter().any(|f|
      f.rule_id == "P006" && f.path.iter().any(|p| p == "ix:reconcile")
          && f.severity != Severity::Unsafe));
  ```

  Add a second test with `root: None` that pins the current P-rule IDs and
  severities.

- [ ] **Step 2: Run the focused tests and verify RED**

  Run:

  ```bash
  cargo test -p qedgen-solana-skills verify::ratchet::tests::readiness_with_root_reports_source_only_handler -- --exact
  cargo test -p qedgen-solana-skills verify::ratchet::tests::readiness_without_root_preserves_idl_only_report -- --exact
  ```

  Expected: compilation fails because `root` and the QED001 reconciliation do
  not exist.

- [ ] **Step 3: Extract a public drift summary from the existing overlay**

  Add a narrow `IdlSourceDrift` value type and helper in `idl_overlay.rs`.
  It must reuse `discover_anchor_handlers`, the existing IDL parser, and the
  overlay matching rules; it must not duplicate name normalization. Export it
  through `probe/mod.rs` for `verify/ratchet.rs`.

- [ ] **Step 4: Extend options and translate drift into Ratchet findings**

  Add optional roots to both option structs. After the ordinary Ratchet report
  is built:

  ```rust
  if let Some(root) = &opts.root {
      apply_source_drift(&mut report, root, &opts.idl)?;
  }
  ```

  QED001 is `Unsafe`, uses path `["ix:<handler>"]`, names the source-only
  handler in its message, and documents an acknowledgement flag. Findings
  whose `path` contains an IDL-only `ix:<handler>` segment are demoted to
  `Additive` and annotated as stale IDL surface. Keep all untouched findings
  in their original order; append QED001 findings sorted by handler.

- [ ] **Step 5: Add failing CLI tests for `--root` plumbing and JSON output**

  In `tests/ratchet_cli.rs`, invoke `readiness --idl ... --root ... --json` and
  `check-upgrade --old ... --new ... --root ... --json`. Assert QED001 appears,
  `emergency_withdraw` is named, and an invalid root exits 3 with a contextual
  error.

- [ ] **Step 6: Run CLI tests and verify RED**

  Run:

  ```bash
  cargo test -p qedgen-solana-skills --test ratchet_cli root
  ```

  Expected: Clap rejects `--root`.

- [ ] **Step 7: Add CLI fields and dispatch plumbing**

  Add `root: Option<PathBuf>` to both Clap variants, document that
  `check-upgrade` applies it to `--new`, and pass it through `run.rs`.

- [ ] **Step 8: Run focused and neighboring tests**

  Run:

  ```bash
  cargo test -p qedgen-solana-skills verify::ratchet
  cargo test -p qedgen-solana-skills --test ratchet_cli
  cargo test -p qedgen-solana-skills probe::idl_overlay
  ```

  Expected: all pass.

- [ ] **Step 9: Commit #398**

  ```bash
  git add crates/qedgen/src/cli.rs crates/qedgen/src/run.rs \
    crates/qedgen/src/verify/ratchet.rs crates/qedgen/src/probe/idl_overlay.rs \
    crates/qedgen/src/probe/mod.rs crates/qedgen/tests/ratchet_cli.rs
  git commit -m "feat(readiness): reconcile source and IDL surfaces"
  ```

### Task 2: Canonical category identity (#399)

**Files:**
- Modify: `crates/qedgen/src/probe/mod.rs`
- Modify: `crates/qedgen/src/probe/pinocchio_probe.rs`
- Modify: `crates/qedgen/src/probe/probe_repro.rs`
- Modify: `crates/qedgen/src/adapt/pinocchio_extractor.rs`
- Modify: `crates/qedgen/tests/fixtures/pinocchio-fixtures/ptoken-transfer/expected_findings.json`
- Modify: `skills/qedgen-auditor/references/category-catalog.md`
- Modify: `scripts/check-auditor-skill.sh`

**Interfaces:**
- Consumes: `Category::tag()`.
- Produces: `fn applicable_categories(runtime: &Runtime) -> Vec<Category>`.
- Produces: `pub fn applicable_categories_public(runtime: &Runtime) -> Vec<String>` as the stable serialization adapter.

- [ ] **Step 1: Add failing tests for typed category round-trips**

  Add tests that demand both
  `pinocchio_unchecked_amount_arith` and
  `pinocchio_unchecked_lamport_arith`, assert every applicable value maps
  through `Category::tag()`, and assert tags are unique.

- [ ] **Step 2: Run category tests and verify RED**

  Run:

  ```bash
  cargo test -p qedgen-solana-skills probe::tests::applicable_categories_are_canonical -- --exact
  ```

  Expected: failure because applicable categories are raw strings and the enum
  has only `PinocchioUncheckedArith`.

- [ ] **Step 3: Split the Pinocchio arithmetic category and type the work list**

  Replace `PinocchioUncheckedArith` with:

  ```rust
  PinocchioUncheckedAmountArith,
  PinocchioUncheckedLamportArith,
  ```

  Amount mutation sites choose the first; direct lamport mutation sites choose
  the second. Update extractor and reproducer matches exhaustively.
  `applicable_categories()` builds `Vec<Category>` with cloned enum values;
  only the public/output boundary maps `Category::tag()` to strings.

- [ ] **Step 4: Update fixtures and run Pinocchio tests**

  Update expected finding tags according to the probed site type, then run:

  ```bash
  cargo test -p qedgen-solana-skills pinocchio
  cargo test -p qedgen-solana-skills adapt::pinocchio_extractor
  ```

  Expected: all pass with the split tags.

- [ ] **Step 5: Add a failing shell gate for catalog/category reconciliation**

  Extend the auditor preflight fixture to inject an orphan category and assert
  `scripts/check-auditor-skill.sh` fails with
  `category identity drift`. Run:

  ```bash
  bash scripts/test-auditor-preflight.sh
  ```

  Expected: the injected orphan is not detected.

- [ ] **Step 6: Implement the catalog reconciliation gate**

  Parse canonical tags from the enum tag match and catalog names from
  `^### \`` headings. Compare both directions. Keep explicit sorted arrays for
  intentional model-only and probe-only identities beside the gate; fail on
  every unallowlisted orphan. Add narrative entries for the split Pinocchio
  arithmetic categories.

- [ ] **Step 7: Run focused gates and commit #399**

  Run:

  ```bash
  bash scripts/check-auditor-skill.sh
  bash scripts/test-auditor-preflight.sh
  cargo test -p qedgen-solana-skills probe
  ```

  Then:

  ```bash
  git add crates/qedgen/src/probe crates/qedgen/src/adapt/pinocchio_extractor.rs \
    crates/qedgen/tests/fixtures/pinocchio-fixtures/ptoken-transfer/expected_findings.json \
    skills/qedgen-auditor/references/category-catalog.md \
    scripts/check-auditor-skill.sh scripts/test-auditor-preflight.sh
  git commit -m "refactor(probe): canonicalize category identity"
  ```

### Task 3: Compile the exact `codegen --all` artifact set (#395)

**Files:**
- Modify: `crates/qedgen/tests/generated_artifact_gate.rs`
- Modify if job timeout requires it: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: existing codegen test helpers and `gate_target_dir()`.
- Produces: ignored test `anchor_all_artifacts_compile_without_execution`.

- [ ] **Step 1: Add the ignored compile-only test**

  Generate the escrow Anchor example with one `--all` invocation, assert the
  complete documented file set including integration, Lean, Crucible, and CI
  outputs, redirect local macro dependencies, then run:

  ```rust
  Command::new("cargo")
      .arg("check")
      .arg("--tests")
      .arg("--manifest-path")
      .arg(&cargo_toml)
      .env("CARGO_TARGET_DIR", gate_target_dir());
  ```

- [ ] **Step 2: Verify RED**

  Run:

  ```bash
  cargo test -p qedgen-solana-skills --test generated_artifact_gate \
    anchor_all_artifacts_compile_without_execution -- --ignored --exact
  ```

  Expected: failure in the previously uncompiled combined artifact path or a
  missing file-set assertion.

- [ ] **Step 3: Make the minimal generator/test-fixture correction**

  Fix only defects exposed by the exact `--all` compile. Do not weaken file-set
  assertions or execute the Parallax test binary.

- [ ] **Step 4: Run the new test and existing artifact gates**

  Run:

  ```bash
  cargo test -p qedgen-solana-skills --test generated_artifact_gate \
    anchor_all_artifacts_compile_without_execution -- --ignored --exact
  cargo test -p qedgen-solana-skills --test codegen_determinism
  cargo test -p qedgen-solana-skills --test documented_lane_journeys
  ```

- [ ] **Step 5: Commit #395**

  ```bash
  git add crates/qedgen/tests/generated_artifact_gate.rs .github/workflows/ci.yml \
    crates/qedgen/src/codegen crates/qedgen/src/run.rs
  git commit -m "test(codegen): compile the complete all artifact set"
  ```

  Stage only paths actually changed.

### Task 4: Benchmark schemas and tiered scoring (#401, #402)

**Files:**
- Create: `skills/qedgen-auditor-bench/schemas/corpus-manifest.schema.json`
- Create: `skills/qedgen-auditor-bench/schemas/normalized-report.schema.json`
- Create: `skills/qedgen-auditor-bench/schemas/score.schema.json`
- Create: `skills/qedgen-auditor-bench/schemas/validate.sh`
- Create: `skills/qedgen-auditor-bench/fixtures/synthetic/corpus-manifest.json`
- Create: `skills/qedgen-auditor-bench/fixtures/synthetic/normalized-report.json`
- Create: `skills/qedgen-auditor-bench/fixtures/synthetic/score.json`
- Modify: `skills/qedgen-auditor-bench/SKILL.md`
- Modify: `scripts/check-auditor-skill.sh`
- Modify: `scripts/test-auditor-preflight.sh`

**Interfaces:**
- Produces difficulty enum: `["smoke", "standard", "hard", "adversarial"]`.
- Produces score contract with `per_difficulty`, `tier_entry_counts`,
  optional `aggregate`, and `comparison.composition_valid`.

- [ ] **Step 1: Add failing preflight tests for missing schemas and tier rules**

  Extend `test-auditor-preflight.sh` to remove one schema in its scratch copy
  and to remove the canonical phrase
  `MUST NOT collapse mixed difficulty tiers into one headline score`.
  Both mutations must make the checker fail.

- [ ] **Step 2: Run the preflight and verify RED**

  Run:

  ```bash
  bash scripts/test-auditor-preflight.sh
  ```

  Expected: the new negative cases unexpectedly pass.

- [ ] **Step 3: Write schemas and valid synthetic fixtures**

  Use draft 2020-12, `additionalProperties: false`, stable QEDGen schema URIs,
  required fields from #401, and `$defs` for labeled findings and domain
  expectations. The score schema requires every reported tier to have counts
  and requires `composition_valid: false` when compared tier counts differ.

- [ ] **Step 4: Write the jq validator and verify invalid fixtures fail**

  `schemas/validate.sh` validates each synthetic file structurally with jq,
  checks schema URI/version constants, verifies referenced corpus entry IDs,
  and compares tier count maps. Add temporary invalid copies in the shell test
  and assert clear failures for a missing `difficulty` and mismatched
  comparison composition.

- [ ] **Step 5: Document tiered scoring in the benchmark skill**

  Define the four difficulty values, require per-tier recall and precision,
  prohibit a collapsed headline, require counts beside optional aggregates,
  and mark model/skill comparisons invalid when tier composition differs.

- [ ] **Step 6: Wire validation into the live auditor gate**

  `check-auditor-skill.sh` requires all schemas and fixtures, invokes
  `schemas/validate.sh`, and checks canonical scoring language.

- [ ] **Step 7: Run validators and commit #401/#402**

  Run:

  ```bash
  bash skills/qedgen-auditor-bench/schemas/validate.sh
  bash scripts/check-auditor-skill.sh
  bash scripts/test-auditor-preflight.sh
  ```

  Then:

  ```bash
  git add skills/qedgen-auditor-bench scripts/check-auditor-skill.sh \
    scripts/test-auditor-preflight.sh
  git commit -m "feat(benchmark): validate manifests and tiered scoring"
  ```

### Task 5: Knowledge-base provenance gate (#400)

**Files:**
- Create: `skills/qedgen-auditor/scripts/check-knowledge-bases.sh`
- Create: `skills/qedgen-auditor/references/basis-legacy-allowlist.txt`
- Modify: `skills/qedgen-auditor/references/category-catalog.md`
- Modify: `docs/security-primer.md`
- Modify: `scripts/check-auditor-skill.sh`
- Modify: `scripts/test-auditor-preflight.sh`

**Interfaces:**
- Accepts basis forms:
  - `Basis: source:<repository>@<revision>:<path-or-symbol>`
  - `Basis: url:https://...`
  - `Basis: fixture:<repository-relative-path>`
  - `Basis: corpus:<stable-id>`
  - `Basis: prose:<summary>` only when named in the legacy allowlist.

- [ ] **Step 1: Add failing shell tests**

  In scratch copies, remove a category basis, point a fixture basis at a
  nonexistent path, add an unknown category with prose-only basis, and remove a
  primer grep basis. Assert each produces a specific diagnostic.

- [ ] **Step 2: Run preflight and verify RED**

  Run:

  ```bash
  bash scripts/test-auditor-preflight.sh
  ```

  Expected: all new mutations evade the current checker.

- [ ] **Step 3: Implement the focused knowledge-base checker**

  Parse every catalog heading as one entry and require exactly one `Basis:`
  before the next heading. Validate prefixes and fixture paths. Permit
  prose-only bases only for explicitly listed historical category names and
  print warnings for them. Parse each primer `**Grep for:**` block and require a
  following `**Basis:**` line before the next section.

- [ ] **Step 4: Add bases to catalog and primer**

  Prefer existing checked-in probe fixtures. Use source paths/symbols for
  runtime-defined behavior and documentation URLs for specification-defined
  behavior. Give historical corpus prose a stable `prose:` basis plus explicit
  allowlist entry; do not invent unverifiable incident IDs.

- [ ] **Step 5: Report reverse coverage**

  Warn for catalog categories with no `fixture:` or `corpus:` basis and print a
  sorted summary. This warning is non-fatal in the initial rollout.

- [ ] **Step 6: Wire and run all auditor gates**

  Run:

  ```bash
  bash skills/qedgen-auditor/scripts/check-knowledge-bases.sh
  bash scripts/check-auditor-skill.sh
  bash scripts/test-auditor-preflight.sh
  ```

  Expected: exit 0; historical warning summary is deterministic.

- [ ] **Step 7: Commit #400**

  ```bash
  git add skills/qedgen-auditor scripts/check-auditor-skill.sh \
    scripts/test-auditor-preflight.sh docs/security-primer.md
  git commit -m "docs(auditor): require verifiable knowledge bases"
  ```

### Task 6: Integrated verification and pull request

**Files:**
- Modify only if verification exposes a regression in changed scope.
- Review: all files changed from `origin/main`.

**Interfaces:**
- Produces: one pushed branch and one GitHub pull request closing #398, #399,
  #395, #401, #402, and #400.

- [ ] **Step 1: Format and lint changed code**

  Run:

  ```bash
  cargo fmt --all -- --check
  bash -n scripts/check-auditor-skill.sh
  bash -n scripts/test-auditor-preflight.sh
  bash -n skills/qedgen-auditor-bench/schemas/validate.sh
  bash -n skills/qedgen-auditor/scripts/check-knowledge-bases.sh
  ```

- [ ] **Step 2: Run the combined focused suite**

  Run:

  ```bash
  cargo test -p qedgen-solana-skills verify::ratchet
  cargo test -p qedgen-solana-skills probe
  cargo test -p qedgen-solana-skills --test ratchet_cli
  cargo test -p qedgen-solana-skills --test codegen_determinism
  cargo test -p qedgen-solana-skills --test documented_lane_journeys
  cargo test -p qedgen-solana-skills --test generated_artifact_gate \
    anchor_all_artifacts_compile_without_execution -- --ignored --exact
  bash scripts/check-auditor-skill.sh
  bash scripts/test-auditor-preflight.sh
  ```

- [ ] **Step 3: Run the repository-wide Rust suite**

  Run:

  ```bash
  cargo test -p qedgen-solana-skills
  ```

  Record any pre-existing or environment-only failures separately; do not call
  the branch complete while a changed-scope failure remains.

- [ ] **Step 4: Review issue coverage and diff hygiene**

  Run:

  ```bash
  git diff --check origin/main...HEAD
  git status --short
  git log --oneline origin/main..HEAD
  git diff --stat origin/main...HEAD
  ```

  Confirm no #341 RCA, build output, third-party corpus content, or unrelated
  user changes are present.

- [ ] **Step 5: Push and open the PR**

  ```bash
  git push -u origin feat/active-issue-queue
  gh pr create --repo QEDGen/solana-skills \
    --base main \
    --head feat/active-issue-queue \
    --title "feat: close active QEDGen correctness and benchmark gaps" \
    --body-file /tmp/solana-skills-active-issues-pr.md
  ```

  The PR body summarizes each isolated commit, lists exact verification
  commands, and includes:

  ```text
  Closes #398
  Closes #399
  Closes #395
  Closes #401
  Closes #402
  Closes #400
  ```
