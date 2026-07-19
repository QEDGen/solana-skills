# QEDGen v2.46.0 — the periphery hardened: rename recovery, journey-tested lanes, mechanical gates

**Status:** released. **Scope:** 19 merged PRs since v2.45.0 (#264–#268, #275–#287, #291–#292).
**Theme:** a triage sweep of the July issue wave, driven by one observation:
the codegen core is snapshot-armored, so bugs cluster in the periphery — CLI
plumbing, accounting, bash/jq validators, runbooks, and documented lanes that
no test had ever driven end to end. This release clears every open issue from
that wave (the only issues left open predate it) and lands the prevention
program so the same classes can't silently return.

## 1. Rename recovery for user-owned files — `--force` and `--merge-accounts` (#253, #288)

`src/lib.rs` and `src/instructions/*.rs` are user-owned: codegen skips them
when they exist, which is right for preserving handler fills — and wrong
after a spec-level rename, where the regenerated siblings (guards.rs,
state.rs, harnesses) pick up new names while the skipped scaffold keeps the
old ones and the crate stops compiling. All three options from the #253
dogfooding report shipped:

- **Warn on stale skip** (#287): the skip site compares the file's embedded
  `#[qed(spec_hash = …)]` stamps against the current spec's per-handler
  hashes; a mismatch escalates the skip note to a WARNING naming the cause
  and the recovery flags.
- **`codegen --force`** (#292, all targets): regenerates the user-owned set
  wholesale — the rename workflow where regen + re-fill beats hand-merging.
  Gated by a **git-recoverability guard** that runs before any artifact is
  written: every file the flag would overwrite must be tracked and
  unmodified, otherwise codegen aborts listing the dirty files. A `--force`
  can never destroy fills that git can't recover.
- **`codegen --merge-accounts`** (#292, Anchor): surgical recovery — the
  Cargo.toml section-merge doctrine applied to Rust items. Regenerates each
  current handler's `#[derive(Accounts)]` struct inside the user-owned
  `lib.rs` in place, preserving the `#[program]` mod, handler fills, and
  imports. New handlers append; structs with no matching handler are
  reported but never deleted; a same-named struct without
  `#[derive(Accounts)]` is user territory and left alone.

The rename→recover lane is pinned end to end by a journey test
(scaffold → fill → commit → rename → warn → merge preserves fills → force
refuses dirty, regenerates after commit).

## 2. Severity accounting was dropping errors (#260, #270)

`check`'s summary tally was written when `Severity` had two variants;
`Error` was added later and the open-coded `.filter(== variant)` counting
compiled while silently dropping it — for ten releases, E-lints appeared in
the finding list but not in the summary line or the exit code. Fixed by the
closed-enum doctrine applied to small enums: `SeverityCounts::of` counts
through an exhaustive `match`, so the next new variant is a compile error at
every accounting site. The same audit found and fixed the identical bug in
`UnifiedReport::issue_count` (E-lints passed the `--code`/`--kani` exit
gate). CLAUDE.md now states the small-enum rule.

## 3. Bootstrap→ratify and probe lanes work as documented (#248, #249, #251, #289, #290)

The documented spec-less elicitation handoff — `probe --bootstrap
--emit-spec-candidates --audit-dir` → `answers.json` → `ratify` — had never
been driven end to end: the bootstrap branch silently dropped the audit dir
and ratify wrote the spec to `<root>/.qed/.qed.qedspec`. Both fixed (#268);
default paths resolve from the manifest's recorded `target.program_root`.
Follow-through residuals: `probe --program .` (the natural invocation from
inside a program root) now canonicalizes before anything derives a name —
no more `spec Program` skeletons or `program.qedspec` (#289); resolved
codegen output paths no longer render a literal `/./` (#290); `probe`
accepts the `--json` flag its siblings taught users to type (#251).

## 4. Diagnostics: absolute paths and construct-named parse errors (#254, #262, #279)

- Unknown-manifest-dep errors name the searched `qed.toml` absolutely
  (no more `(looking in )`), and chumsky parse failures prefix the nearest
  enclosing construct instead of raw char-class expectations (#286).
- Relative codegen output paths — including every clap default — resolve
  against the **spec's directory**, not the invoker's cwd: `codegen --spec
  <elsewhere>/x.qedspec` never scatters artifacts under cwd (#285).
- Missing prerequisites preflight as one combined report instead of
  failing one at a time (#285).

## 5. Prevention program: gates as scripts, lanes as tests (#269, #271–#274)

- **`scripts/release-gate.sh`** (#276): the example `--frozen` baseline
  (per-example expectation table) + zero-`sorry` sweep as a script asserting
  by exit code, run on every PR — the release checklist's §7/§8 stopped
  being prose.
- **Journey tests** (#282): the SKILL.md quickstart and scaffold-to-spec
  lanes execute their exact documented command sequences against staged
  fixtures; #248/#249 shipped because no such test existed.
- **Cold-start smoke** (#283): a 5-scenario `install.sh` matrix +
  installed-binary quickstart in CI (`cold-start.yml`), catching the
  released-binary × HEAD-spec skew class.
- **Auditor validators hardened** (#266, #281): type-wrong domain-artifact
  fields fail cleanly with a named field instead of crashing jq; a
  shape-wrong fixture per validator + shellcheck run in CI.
- **Version-skew guard** (#265): `install.sh` refuses to keep a stale
  `bin/qedgen` silently.
- **Scout rule** (#280): a documented workaround is an unfiled bug — the
  toolchain scout now sweeps docs for them.

## Compatibility

No breaking changes. New CLI surface: `codegen --force`,
`codegen --merge-accounts`, `probe --json`. All existing specs, scaffolds,
and workflows continue to work unchanged; `--merge-accounts` is Anchor-only
and errors early elsewhere.
