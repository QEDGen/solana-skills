# QEDGen v2.10 — release notes

**Tag:** `v2.10.0` (pending)
**Branch:** `v2.10-bear-hug`
**Theme:** the bear-hug — examples that compile, defense layers that hold, lints that catch the next time.

v2.9 closed the brownfield wedge. v2.10 was meant to ship `qedgen probe`
+ the auditor subagent. During pre-release the auditor ran on the
showcase examples and found that the codegen-output examples didn't
compile from a fresh regen, and once they did, they had real CRITs
and HIGHs. The release scope expanded twice: first to absorb a
**codegen quality pass** (R1–R24) so examples build green, then to
absorb a **post-codegen security audit** (R25–R28) and the
**spec-authoring lints** (5 new `qedgen check` rules) that catch the
recurring spec-shape gaps.

The result: a release where the showcase examples are *the floor* of
quality, not the gap. The auditor surfaces what the spec misses
entirely; the lints surface what the spec under-models; codegen
emits the defense layers automatically.

## What's in

### Codegen quality pass (R1–R24)

Three Quasar-target codegen examples (`multisig`, `lending`,
`percolator`) went from 88–90 cargo errors each on a fresh regen to
**0 errors each**. 24 root-cause fixes shipped in 7 commits — every
one resolves a class of codegen-output problem, not just the one
example that surfaced it.

Highlights:

- **R10** — `<'info>` lifetimes on Quasar `#[derive(Accounts)]` structs;
  fields are `&'info` references per quasar_lang's canonical pattern.
  Closes the `AccountCount`/`ParseAccounts` cascade.
- **R12** — `guards.rs` now binds `s.field` to `ctx.<state_account>.field`
  with word-bounded substitution so `accounts[i].fee_credits.get()`
  doesn't get corrupted to `fee_creditctx.vault.get()`.
- **R13** — target-aware seeds: Quasar bare idents (auto-handled by
  the macro) instead of `.key().as_ref()`; suppress `seeds = […]` on
  Quasar non-init handlers when seeds reference state fields (paired
  with R28 below).
- **R17** — Quasar's `#[account]` macro auto-wraps integer state fields
  in Pod companions. `mechanize_effect`'s `set` adds `.into()`,
  `wrapping_*` unwinds to `.get().wrapping_*().into()`.
- **R20/R24** — user record types emitted as `#[repr(C)]` structs with
  Pod-flavored fields on Quasar; `expr_to_rust` threads
  `RustOpts { pod_aware, env }` so Pod field accesses get `.get()`
  postfix and mixed-kind binops add `as i128` casts.
- **R22** — `Fin[N]` handler params lower to `u32` on Quasar so the
  `#[instruction]` macro auto-Pods them to `PodU32` (avoiding the
  `WriteBytes` / alignment-1 conflict on `usize`).

Full root-cause table: `.claude/projects/-Users-abishek-code-leanstral-solana-skill/memory/project_v2_10_codegen_pass.md`.

### Post-codegen security audit closure (R25–R28)

The auditor subagent (shipping in this release) ran on the freshly-
regenerated examples and found 2 CRITs + 6 HIGHs. Five closed by
codegen, two by spec edits.

- **R25** — `auth X` lowers to `has_one = X` when `X` matches a state
  field. Closes percolator's CRIT (every handler reachable by any
  signer) and multisig::remove_member HIGH in one emit.
- **R26** — runtime lifecycle Status enforcement. State structs carry
  a `pub status: u8` field; guards.rs emits `if status != Pre as u8 {
  return Err(InvalidLifecycle) }` on entry and `status = Post as u8`
  after the requires. Closes multisig::propose's proposal-erasure
  CRIT (calling `propose` from `HasProposal` would have zeroed the
  vote tally).
- **R27** — runtime token-vault authority verification. When the spec
  declares `pool_vault : token, authority pool`, guards.rs emits
  `if *ctx.pool_vault.owner() != *ctx.pool.address() { return
  Err(Unauthorized) }`. Closes the lending pool_vault HIGH.
- **R28** — runtime PDA verification on R13-suppressed handlers using
  `quasar_lang::pda::verify_program_address` with the stored bump.
  Closes the wrong-PDA-passing surface across the multisig non-init
  handlers and lending repay/liquidate.

Spec edits the user (you) made on top of R25–R28 closed the last two
audit findings:

- multisig — `members : Map[32] Pubkey` + `voted : Map[32] U8` on
  `State.Active`; approve/reject add `requires
  state.members[member_index] == approver` and `requires
  state.voted[member_index] == 0`.
- lending::liquidate — `requires state.amount > state.collateral
  else AccountHealthy`.

### Spec-authoring lints (5 new `qedgen check` rules)

Five new lints that catch the audit's recurring spec-shape gaps **at
spec-authoring time**, sub-second, on save. Each maps 1:1 to an audit
finding so routine gaps don't have to wait for a post-codegen audit.

- `[unbound_auth]` — `auth X` without a state-field anchor (would
  have caught the percolator CRIT).
- `[unguarded_indexed_mutation]` — handler takes index parameter and
  mutates `state.<map>[i]` without binding `i` to the signer
  (multisig approve/reject).
- `[scalar_counter_no_dedup]` — counter increment with no per-actor
  tracking field; tightened to fire only when the bound is itself a
  state field (so const-bounded TVL caps don't false-positive).
- `[unguarded_terminal_transition]` — handler reaches a terminal
  lifecycle state with no `requires` (lending::liquidate); skipped
  when R25's `auth → has_one` already binds the auth.
- `[unconditional_value_transfer]` — handler transfers from
  program-owned authority with no caller-binding `requires` (lending
  pool_vault); same R25-skip exception.

Verified on the audit's pre-fix specs — every original audit finding
that the lint covers fires; on the post-fix specs all four examples
are clean.

PRD: `docs/prds/SPEC-AUTHORING-LINTS-v2.10.md`.

### `qedgen probe` + auditor subagent

The original v2.10 scope. Shipped end-to-end:

- **`qedgen probe --json`** — emits per-handler findings in
  spec-aware mode (predicate runtime against `.qedspec`) and the
  work-list envelope in spec-less brownfield mode (`--bootstrap
  --root <p>`). Categories: `missing_signer`, `arbitrary_cpi`,
  `arithmetic_overflow_wrapping`, `lifecycle_one_shot_violation`.
- **`qedgen-auditor` skill** — the harness-native auditor subagent.
  Read+Grep+Bash+Write tool surface; works on Anchor / native /
  sBPF / qedgen-codegen runtimes; spec-aware AND spec-less.
  Vulnerability-first digest, full report at
  `.qed/findings/audit-<ts>.md`, suppressions to
  `.qed/probe-suppress.toml`. Runs in any agent-skills harness
  (Claude Code, Codex, Cursor, Windsurf).

### `bootstrap` mode for `qedgen check`

`qedgen check --bootstrap --root <p>` walks a brownfield project
without requiring a `.qedspec` upfront — scaffolds the work list the
auditor then investigates.

### Closes

- escrow issues #17 + #18 (committed pre-codegen-pass; teaches the
  auditor to catch them next time).
- All 8 audit findings on the showcase examples (5 by codegen, 2 by
  spec edits, 1 by user adopting `permissionless` markers on
  intentionally-open handlers).

## What's not in

- **Pinocchio target** — still reserves the CLI surface; codegen
  branch slips to v2.11+.
- **Lean auto-prove for Map-using specs** — the multisig spec now
  uses `Map[N] Pubkey` for the member list, which routes lean_gen
  through `render_indexed_state` (no theorem bodies; proofs live in
  user-owned `Proofs.lean`). Six lean_gen tests rebased to a frozen
  scalar-only fixture; extending the indexed-state renderer to
  emit auto-proven theorems is a v2.11 follow-up.
- **R5 skip-if-exists UX** — generating a new `pub mod` declaration
  in lib.rs requires `rm`-ing the user-owned file. Workaround
  documented; UX overhaul deferred.

## Numbers

- 11 codegen / lint / spec commits on `v2.10-bear-hug`.
- 28 root causes resolved (R1–R28) — 24 codegen, 4 security.
- 439 qedgen unit tests pass; 4 new fixture-based regression tests
  for the spec-authoring lints.
- Multisig: 88 errors → 0. Lending: 88 → 0. Percolator: 90 → 0.
- 2 CRITs, 6 HIGHs, 3 MEDs, 2 INFOs from the audit — all closed
  (5 by codegen, 2 by spec edits, 1 by `permissionless` markers).

## Defense layers per Quasar handler (post-v2.10)

```rust
// 1. R26 — lifecycle pre-check
if ctx.vault.status != Status::HasProposal as u8 {
    return Err(InvalidLifecycle);
}

// 2. R28 — PDA verification (state-field seeds)
{
    let __seeds = &[b"vault", ctx.vault.creator.as_ref(),
                    &[ctx.vault.bump]];
    verify_program_address(__seeds, &crate::ID,
                            ctx.vault.address())?;
}

// 3. R27 — token authority binding (when applicable)
if *ctx.pool_vault.owner() != *ctx.pool.address() {
    return Err(Unauthorized);
}

// 4. user-spec requires (with R12 `s.field` → `ctx.<acct>.field` bind)
if !(ctx.vault.members[(member_index) as usize]
     == (*ctx.approver.to_account_view().address())) {
    return Err(NotAMember);
}
```

Plus on the `#[derive(Accounts)]` struct itself (R25):

```rust
#[account(mut, has_one = creator)]
pub vault: &'info mut Account<MultisigAccount>,
```

## Pre-tag gates

- [x] `cargo fmt --check`
- [x] `cargo clippy -- -D warnings`
- [x] `cargo test` — 439 pass, 0 fail
- [x] `bash scripts/check-readme-drift.sh`
- [x] All 3 codegen-output examples compile cleanly from fresh regen
- [x] Audit clean on all 4 specs (multisig, lending, percolator,
      escrow): zero CRIT/HIGH from the spec-authoring lints
- [x] `.qedspec` files frozen and audit-fixed for the showcase
      examples
- [ ] `lake build` on bundled Lean projects — multisig uses
      `render_indexed_state` (no theorem bodies); other examples
      retain auto-proven Spec.lean
- [ ] `qedgen check --frozen` against bundled examples
- [ ] Doc/code drift sweep
- [x] Version bumped: `crates/qedgen/Cargo.toml` 2.9.0 → 2.10.0
- [x] `feedback_no_anchor_v2_mentions.md` policy: no naming external
      sources in user-facing docs

## Linked memories / docs

- `project_v2_10_codegen_pass.md` — full codegen pass status table
- `.qed/findings/audit-20260427-v210.md` — audit closure report
- `docs/prds/SPEC-AUTHORING-LINTS-v2.10.md` — lint design + audit mapping
- `feedback_audit_bear_hug.md` — bear-hug requires examples that work
- `feedback_audit_as_subagent.md` — auditor as harness-native subagent
- `feedback_minor_release_completeness.md` — bias ambition over cadence

## Upgrade notes

`#[qed]` attribute hash format unchanged — existing programs
re-validate without spec edits. Three behavior changes worth
noting:

1. **Quasar-target programs** that R6/R7 previously couldn't compile
   now do; if you have a hand-edited workaround crate, regenerate
   from the spec to pick up R10/R17/R20/R22/R24 fixes.
2. **`qedgen check` may emit 5 new lint families** on existing specs.
   `unbound_auth` has the highest false-positive risk (intentional
   single-signer admin handlers without a state-side anchor); use
   `permissionless` if the open access is by design.
3. **State structs gain a `pub status: u8` field** when the spec
   declares lifecycle states. Existing on-chain accounts retain
   their stored layout — the field is appended, so reading legacy
   data needs a one-time `status := <variant>` migration call.
   v2.11 will ship a migration shim.
