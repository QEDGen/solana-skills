# QEDGen v2.44.0 — spec elicitation and the IDL-aware spec-less probe

**Status:** released. **Scope:** 6 merged PRs / 10 commits since v2.43.0.
**Theme:** the brownfield entry path gets a spine — the spec-less probe now
consumes on-disk IDLs, hypothesizes invariants from anchored evidence, lowers
confirmed answers into executable spec clauses, and stamps verified handlers
only from recorded verify evidence.

## 1. Spec elicitation — hypothesizer, ratify lowering, stamp verb (#243)

Implements the spec-elicitation PRD Phases 0–5
(`docs/design/spec-elicitation-prd.md`):

- Every spec-less envelope carries a `run_id` and a `spec_readiness` block
  (hypotheses total / lowerable / by class), so an audit session knows how much
  spec is one confirmation away.
- The **hypothesizer** emits evidence-anchored invariant hypotheses across
  seven classes — authorization, lifecycle-init-once, arithmetic-bound,
  conservation, CPI-integrity, unwired-guard, state-machine. The precision
  rule is strict: no evidence anchor (a declared enforced signer, an Anchor
  `init` constraint, a held `require!` bound, an IDL `has_one` relation, …) →
  no hypothesis. Each hypothesis names its lowering, payoff, and backend
  reachability up front.
- **`qedgen elicit`** structures the confirm/reject/BUG answer flow;
  **`qedgen ratify`** lowers confirmed hypotheses into executable spec
  clauses (auth clauses, `requires` bounds, lifecycle transitions) rather
  than prose.
- **`qedgen stamp`** is the new attribute writer: it emits
  `#[qed(verified, hash = …)]` only for handlers whose
  implementation-verified evidence is recorded in
  `.qed/verify-evidence.json` (written by every `qedgen verify` run since
  this release). No evidence → no stamp; the attribute can no longer outrun
  verification.
- `qedgen adapt` and `qedgen spec --idl` are **soft-deprecated** (removal in
  v3.0): scaffold-to-spec goes through probe elicitation, attribute writing
  through `stamp`.

## 2. IDL-aware spec-less probe (#236, #239, #246 — issues #235, #238, #241)

The bootstrap probe now treats an on-disk IDL as an enrichment overlay, with
source discovery staying ground truth:

- **Enrichment (#236):** per-handler `idl_accounts` (signer/writable flags)
  and `idl_args` (DSL-vocabulary types) ride on `handlers[]`; the consumed
  IDL is named in `idl_path`. On Anchor/Quasar — where the framework
  *enforces* declared signer flags — an IDL-derived intent tag narrows
  `applicable_categories` for handlers the body classifier left untagged
  (body classification always wins). Codama/Shank flags enrich but never
  narrow on non-framework runtimes. A Pinocchio bootstrap with empty source
  discovery fills `handlers[]` from the Codama instruction list. Handler-set
  disagreement between source and IDL surfaces as `idl_source_drift`
  candidates in both directions, never silently reconciled.
- **Derivable-IDL hint (#239):** with no IDL on disk, the envelope reports
  `derivable_idl` keyed on the detected runtime first — every Anchor/Quasar
  build emits `target/idl/*.json`, so an unbuilt checkout is one build away —
  then on `shank`/`codama` markers. Runtime keying (not a root `Cargo.toml`
  grep) covers workspaces, including the previously-empty unbuilt-workspace
  shape.
- **Workspace-member discovery (#246):** probing `--root programs/<name>`
  now finds an IDL kept at the *workspace* root (`target/idl/`, committed
  `idl/`) via a name-matched ancestor walk — a sibling program's IDL is
  never consumed, and a miss at the workspace root degrades to the same
  derivable hint as any pre-build absence. Previously the same repo yielded
  IDL intent-narrowing from one cwd and none from another.

## 3. Mechanized dead-guard sweep (#242 — issue #240)

The §3f manual audit pass (defined-but-unenforced error variants) is now a
deterministic probe engine: every `#[error_code]` variant wired into no
enforcement call-site surfaces as an `unwired_error_variant` candidate on the
spec-less envelope. A dead guard is an absence, so it is a candidate for
triage — never a reproducer-backed finding.

## 4. Probe regression corpus (#237 — issue #231)

In-repo pass/fail gates for probe behavior (recall/precision *measurement*
stays in the auditor bench):

- `spec/<category>/{vulnerable,safe,hard_safe}.qedspec` triples for all eight
  spec-aware predicates. Contract: vulnerable fires its own category;
  safe/hard_safe stay silent on it; **no** safe fixture may emit a confirmed
  finding of any category — a false confirmed finding is a release blocker.
  hard_safe fixtures are shaped to fool token-matching scanners while staying
  safe under the structural predicate.
- `specless/<scenario>/` project trees exercising the IDL overlay end to end:
  enrichment + narrowing + drift, Pinocchio handler fill, derivable hints
  (marker and unbuilt-framework), the dead-guard sweep, and the
  workspace-member ancestor walk.

## 5. CI and docs

- Lean verification workflows are manual-only for now; the lake-build gate
  runs as a manual release step (`scripts/check-lake-build.sh --strict`),
  and cold-run cache death-spirals are fixed (caches save even on
  cancellation).
- `docs/llms-full.txt` staleness and flag-level `references/cli.md` gaps
  fixed; probe/elicit/stamp surfaces documented in `references/cli.md` and
  `docs/framework-support.md`.

## Compatibility

- The spec-less envelope gains fields (`idl_path`, `derivable_idl`,
  `run_id`, `spec_readiness`, `hypotheses[]`, per-handler `idl_accounts` /
  `idl_args` / `discovered_via`) — additive, schema v3 unchanged.
- `qedgen verify` now writes `.qed/verify-evidence.json` as a side effect;
  `qedgen stamp` consumes it.
- `adapt` / `spec --idl` continue to work but print deprecation guidance;
  plan migration before v3.0.
