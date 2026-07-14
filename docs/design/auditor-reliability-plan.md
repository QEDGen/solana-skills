# Auditor Reliability Improvement Plan

> **Status (2026-07): implemented, with deltas.** This is the
> pre-implementation plan, kept for rationale. As shipped: the category and
> manual-pass material landed as `references/category-catalog.md` and
> `references/manual-review-passes.md` (per §11) plus
> `references/report-and-grading.md`, routed from a slim
> `references/audit-handbook.md`, and `references/model-selection.md` was
> added; the compile gate is a
> synchronous `--compile` flag rather than an async handle; the CLI-contract
> check asserts clap field definitions in `cli.rs`, not full schema or
> snippet validation.

## Goal

Make auditor runs reproducible, technically defensible, and consistent across skill.sh-compatible venues. Prioritize trustworthy findings over catalog breadth without assuming a particular agent harness.

## Phase 1 — Fix invocation correctness

### 1. Establish one canonical skill source

- Treat `skills/qedgen-auditor/` in this repository as authoritative.
- Add an install/sync script that copies it to an explicit venue-owned destination without assuming a harness-specific home directory.
- Add a drift check comparing all skill files and references byte-for-byte.
- Run the drift check in CI and before releases.

Acceptance criteria:

- Repository and installed skill hashes match.
- A test invocation loads the expected version.
- The digest reports the skill version or source commit.

### 2. Make target and spec detection deterministic

- Accept an explicit target root and spec path whenever supplied.
- Otherwise discover `*.qedspec`, `qed.toml`, and supported Cargo manifests.
- Reject ambiguous multi-program or multi-spec discovery with a concise selection request.
- Scope runtime detection to the selected program; unrelated `.s` files must not affect it.

Acceptance criteria:

- Fixtures cover named specs, nested specs, multiple specs, monorepos, mixed Rust/assembly repositories, and missing specs.
- Every run reports the resolved target, runtime, spec path, and audit mode.

### 3. Make preflight truthful

- Start compilation asynchronously but retain its process handle and exit status.
- Never report “compiles clean” before completion.
- Run checks from the selected crate or workspace with the appropriate package selector.
- Distinguish “read audit available” from “reproducer execution available.”
- Define the minimum supported `qedgen` version and enforce it programmatically.

Acceptance criteria:

- Preflight cannot report a false successful build.
- Missing or stale tooling produces a precise degradation statement.
- Read-driven analysis can continue when probe tooling is unavailable.

## Phase 2 — Repair the finding model

### 4. Introduce evidence levels independent of severity

Use four evidence states:

- `confirmed`: a reproducer fired.
- `structural`: the vulnerable path is established from source, but execution was unavailable.
- `hypothesis`: intent or reachability remains unresolved.
- `rejected`: disproved or suppressed.

Severity should describe impact only after reachability is established. Hypotheses must not appear as confirmed vulnerabilities.

Acceptance criteria:

- Every finding has severity, evidence level, reachability conditions, and repro status.
- `inconclusive` is no longer presented as equivalent to a validated structural finding.
- Digest counts distinguish confirmed, structural, hypothetical, and suppressed items.

### 5. Revalidate every catalog predicate

Build a structured record for each category:

- Exact vulnerable condition.
- Safe patterns and framework guarantees.
- Runtime/version applicability.
- Required composition assumptions.
- Minimal vulnerable fixture.
- Minimal safe fixture.
- Expected evidence mechanism.
- Source or authoritative technical basis.

Begin with the highest-risk predicates:

- `discriminator_collision`
- `missing_rent_exemption_check_on_init`
- `realloc_zero_init_data_leak`
- `sentinel_null_key_array_short_circuit`
- `pda_canonical_bump`
- `account_not_reloaded_after_cpi`

Remove or downgrade any category that cannot pass paired vulnerable/safe fixtures.

Acceptance criteria:

- Every CRITICAL/HIGH category has positive and negative regression fixtures.
- Framework-enforced safety is documented and suppresses the predicate.
- Speculative prerequisites, such as signing with the zero public key, cannot produce MED+ findings.

### 6. Separate catalog facts from audit heuristics

- Keep mechanically testable categories in the catalog.
- Move broad reasoning prompts into a shorter manual-review document.
- Label empirical heuristics as heuristics rather than vulnerability predicates.
- Require a concrete affected handler and reachable attack path before promotion.

Acceptance criteria:

- A catalog category is falsifiable.
- A manual pass can generate a candidate, but not directly a confirmed finding.
- Duplicate coverage between passes and categories is eliminated.

## Phase 3 — Make orchestration bounded and portable

### 7. Replace implicit multi-run behavior with an explicit audit profile

| Profile | Runs | Repro effort | Intended use |
|---|---:|---|---|
| Quick | 1 | Confirm obvious HIGH/CRIT | Early development |
| Standard | 1 thorough pass | Required HIGH/CRIT repros | Default |
| High assurance | 2–3 independent passes | Full repro and deduplication | Pre-deploy/mainnet |

- Never create an unbounded convergence loop.
- Detect whether delegation is supported.
- Fall back to sequential independent passes when it is not.
- Record which pass surfaced each finding.

Acceptance criteria:

- Every profile has fixed resource ceilings.
- Standard mode works without subagents.
- High-assurance mode requires explicit selection or an explicit “thorough” request.

### 8. Define sBPF support precisely

Recommended policy:

- Source-pattern auditing of handwritten assembly is unsupported.
- Spec-aware analysis is supported if the CLI genuinely operates independently of Rust source.
- In mixed repositories, only the selected target determines support.
- Never claim source-level coverage for assembly.

Acceptance criteria:

- One unambiguous policy appears everywhere in the skill.
- An sBPF fixture verifies the supported route and rejection message.

## Phase 4 — Add reliability evaluation

### 9. Create a labeled auditor benchmark suite

Include:

- Known vulnerable programs.
- Patched equivalents.
- Framework-safe lookalikes.
- Multi-handler composition bugs.
- Intent-dependent cases.
- Anchor, native Rust, Pinocchio, and qedgen-codegen targets.

Track:

- Recall by category.
- False-positive rate.
- Severity agreement.
- Reproducer success rate.
- Run-to-run variance.
- Time to first confirmed MED+.
- Token and wall-clock cost.

Acceptance criteria:

- No category ships based only on narrative precedent.
- Release gates prevent regression in false-positive rate.
- Benchmark results identify whether extra independent runs justify their cost.

### 10. Test the skill as an executable workflow

Add automated checks for:

- All referenced files exist.
- Every documented CLI flag is supported.
- Shell snippets parse and preserve exit status.
- Output templates match actual JSON schemas.
- Artifact paths are writable and internally consistent.
- Installed and repository skill versions match.

Acceptance criteria:

- Documentation and workflow tests run in CI.
- A CLI change that invalidates the skill fails CI.

## Phase 5 — Reduce complexity

### 11. Split the skill into routed references

Keep the main `SKILL.md` focused on:

- Trigger and scope.
- Preflight.
- Audit phases.
- Evidence and severity rules.
- Output contract.
- Reference routing.

Move details into:

- `references/runtime-detection.md`
- `references/category-catalog.md`
- `references/manual-review-passes.md`
- `references/reproducer-contract.md`
- `references/severity-and-evidence.md`
- `references/orchestration-profiles.md`

Acceptance criteria:

- The main skill is short enough to remain salient during execution.
- Required references are selected by runtime and audit profile.
- No rule is duplicated across files.

## Recommended pickup order

1. Synchronize the installed skill.
2. Fix target/spec detection and preflight status.
3. Introduce evidence levels.
4. Correct the unsound predicates identified during review.
5. Add vulnerable/safe category fixtures.
6. Bound multi-run orchestration.
7. Clarify sBPF support.
8. Split the skill and add workflow CI.
9. Build the broader benchmark suite.

The first three items are the reliability baseline. Until the correct skill is loaded, the correct target is selected, and evidence is distinguished from severity, improvements to audit breadth will not produce dependable results.
