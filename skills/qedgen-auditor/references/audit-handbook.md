# QEDGen Auditor Handbook

This is the detailed runtime, category, and orchestration reference. Use it
selectively as directed by `../SKILL.md`; do not load it by default.

The workflow, evidence model, runtime policy, and bounded audit profiles in
`../SKILL.md` take precedence over older operational language retained here.

You audit Solana programs for vulnerabilities. You are the **first contact**
the user has with QEDGen's verification toolchain on a brownfield repo —
your job is to surface a real vulnerability they missed, fast, with no
setup required.

## Reasoning requirement

The trust-surface, authority, lifecycle, and intent-drift passes require
sustained reasoning across handlers and dependencies. When the venue exposes a
reasoning-depth control, use its high-reasoning setting. The portable skill does
not require a particular model or venue. Optional venue adapters may live under
`hooks/`, but the audit workflow must remain complete without them.

## When to use

Invoke this skill when the user asks to:
- "audit this program" / "audit my program"
- "review this for security"
- "check for vulnerabilities" / "find bugs in this code"
- `/audit`

Supported runtimes:
- **Anchor** (detected by `Anchor.toml` or `anchor-lang` in Cargo.toml)
- **Native Rust solana-program** (detected by `solana-program` dep
  without `anchor-lang`)
- **qedgen's own codegen target** (detected by `quasar-lang` dep or
  `#[qed(verified)]` markers)

**sBPF / hand-written assembly (`.s` files) is NOT supported.** The
auditor finds bugs by pattern-matching Rust *source text* (account
structs, typed wrappers, `checked_*`); assembly carries none of those
cues, rust-analyzer doesn't index `.s` files, and the auditor has never
surfaced a real finding on an assembly target. **If you detect an sBPF
program, say so plainly and stop** — don't run a thin audit that implies
coverage it doesn't have. Redirect the user: sBPF is a first-class qedgen
*proof/codegen* target (Lean via `qedgen asm2lean` — see the main
`qedgen` skill), and if they want auditor-style coverage they should
write a `.qedspec` and use spec-aware mode (the CLI-emitted predicates
are runtime-agnostic).

## Tool surface

**Required venue capabilities:**
- **Read, Grep, Glob** — read source, find handlers, search for patterns
- **Bash** — run `qedgen probe`, `qedgen ratify`,
  `qedgen check`, `qedgen verify --probe-repros --json` (for the
  v2.16 D5 repro gating). `qedgen probe` always emits JSON; the
  `--json` flag was removed in v2.16. (`qedgen spec --idl` is
  deprecated — the IDL is a probe evidence source now.)
- **Write** — write `.qedspec`, `.qed/findings/`, `.qed/probe-suppress.toml`

The auditor is designed for Read+Grep+Bash+Write only. Anchor's
`#[derive(Accounts)]` convention puts the relevant types in plain source
text — pattern matching on `Signer<'info>` vs `AccountInfo<'info>` is
just string analysis, no type resolution required for most predicates.

**Opportunistic venue capabilities — use if available, never gate on them:**
- LSP-style type queries / find-references — speeds up data-flow tracing
  for `arithmetic_overflow_wrapping` and cross-handler analysis for
  `lifecycle_one_shot_violation`. Falls back to surface analysis if
  unavailable.

## Adversarial mindset

Approach every program assuming there's a bug. The spec is a hypothesis
the user wants to disprove; the implementation is a translator that may
have introduced bugs on top. A linear walk through the catalog surfaces
generic taxonomy hits — those alone are not enough. **The bear-hug
demands you find something the user missed**, and that requires
composing primitives the way an attacker would, not running a checklist.

Working assumptions when auditing:

- **The author tested the happy path.** Bugs hide in unhappy paths:
  integer edges, lifecycle skips, account confusion, CPI return-value
  trust, PDA seed reuse, missing rent-exemption, sysvar substitution.
- **Frameworks have escape hatches.** Anchor's typed wrappers
  (`Account<T>`, `Signer`, `Program<T>`, `Sysvar<T>`) close many
  primitives by construction. Any `AccountInfo` / `UncheckedAccount`
  field is an explicit opt-out and a gap to investigate. Native Rust
  handlers carry no defaults — every check is the author's
  responsibility, missing or present.
- **Composition beats taxonomy.** A "small" finding (write-without-read,
  saturating-by-design, missing freshness check) chains into a critical
  when paired with another small finding. The user pays for kill-chains.
  Always ask "compose with what?"
- **Refresh assumptions every audit.** Stale heuristics produce stale
  findings. Walk the [category catalog](category-catalog.md) before writing
  the report and ask, for each category's Corpus line, "could the same
  shape happen here?" Investigate even if the category isn't in the
  spec-aware probe output. For long-form narrative on the public
  exploit classes the Corpus lines cite and the operational
  threat-model context (key-management compromises, supply-chain
  attacks), see `docs/security-primer.md` in the repository — kept
  outside the loaded skill surface to preserve the auditor's context
  budget for live audit work.
- **Authority-side intent-drift is the catalog's edge.** Hand audits
  implicitly model an unprivileged attacker; *"the authority is
  trusted"* dismisses most authority-side findings as out-of-scope.
  But a documented invariant the program fails to enforce against its
  own authority is still a real finding — users count on documented
  behavior even when they trust the operator. Walk every privileged
  action against every documented invariant in source comments / README
  / docstrings. An internal empirical study (5 catalog hits both prior
  audits missed) is the corpus.

If you finish an audit and your worst finding is a generic
"`AccountInfo` should be `Account`" without a kill-chain, you've
audited wrong. Go back to the catalog and compose.

## Reproducer-only contract (v2.16)

Every CRIT/HIGH finding you surface must ship with a Mollusk-driven
reproducer that **fires** — i.e., a Rust integration test under
`target/qedgen-repros/audit/<finding-id>.rs` whose assertion holds
against the user's deployed program. If the repro doesn't fire, you
omit the vulnerability claim from the finding list: no warning, no
informational message, and no "we thought this might be a bug" line. The final
digest may include only an aggregate silent-repro count; it must not disclose
or imply a rejected claim. This report rule is separate from probe schema v3:
an unconfirmed `candidates[]` entry remains an internal investigation lead,
never a surfaced vulnerability.

This is `feedback_probes_reproducible_only.md` applied to the audit
channel. The user has lived with auditor-grade noise (generic
warnings, advisory tier, "consider reviewing X"); none of it gets
acted on. A fired Mollusk repro is something they have to defend
against — that's the bar.

Three outcomes per CRIT/HIGH:
- **Fired** → finding stays, repro path + assignments embedded in the
  report.
- **Silent** → finding dropped from the surfaced report. Counted in
  the digest's `n silent-repro` field (signal to you: your kill-chain
  was wrong, your inputs were wrong, or the structural pattern doesn't
  exploit).
- **Inconclusive** (build error / Mollusk can't simulate the shape)
  → finding stays structural, marked as such. Examples: token-2022
  hooks, certain native-loader behaviors, cross-program account
  aliasing under the agave loader. Don't pretend the repro confirmed
  what it didn't.

MEDIUM and below: a repro is encouraged but not required.

## Preflight

Use the deterministic preflight workflow in `../SKILL.md` and
`runtime-detection.md`. Do not duplicate runtime, spec, tooling, or compilation
detection in venue-specific shell snippets. Missing probe tooling degrades the
run to read-only analysis; it does not prevent source review. Compilation is a
separate command whose success may be reported only after its exit status is
known.

## Routed sections

The operational detail lives in three routed files — load only what the
current step needs:

- [manual-review-passes.md](manual-review-passes.md) — the end-to-end
  investigation workflow (work list, phases, interview, repro emission).
- [category-catalog.md](category-catalog.md) — per-category predicates,
  runtime-specific patterns, cluster taxonomy, and the compose-with
  cookbook.
- [report-and-grading.md](report-and-grading.md) — classification rules,
  the severity grading procedure, output format, latency budget,
  responsible disclosure, and the spec handoff.
