# QEDGen toolchain backlog

Improvement opportunities surfaced while using QEDGen on real projects. Staging
ground for GitHub issues — each entry carries **evidence** (a concrete session
artifact), a **proposed fix**, and a **fix-now vs file** verdict. Owned by the
toolchain-scout agent (`.claude/agents/toolchain-scout.md`); anyone may append.

Standing rule: **codegen bugs get fixed in qedgen, not worked around** (user,
2026-07-08). Feature gaps and DX friction get filed here first.

Legend: 🐞 bug · 🧩 codegen/feature gap · 🩹 DX friction · 📐 methodology.

---

## Session: brownfield Anchor FV — audit target A (2026-07-07 → 08)

Source: a production Anchor program (~13k LOC) under audit. Verified a settings-
invariant preservation property (green) and a per-period spend-conservation
property (fired). Artifacts live in the audit workspace's `.qed/plan/` (private).

### 🐞 B1 — impl-Kani drops requires/ensures-only fields from the snapshot set  [FIXED]

`kani_impl/harness.rs::collect_snapshot_fields` was `modifies ∪ effect-LHS ∪
CPI-binders` — it omitted fields read only in `requires`/`ensures` (e.g.
`num_voters` in `threshold <= num_voters`), so the generated harness referenced
unbound `s.num_voters` / `post_num_voters`. Blocks **any** well-formedness spec.
- **Evidence:** first C harness gen (`tier0-derisk.md` §B); regenerated harness confirms fix.
- **Fix (shipped):** scan `requires`/`ensures` for `pre.`/`post.` fields; token-aware
  `s.`→`pre.` rewrite (`rewrite_state_var_to_pre`); regression test added; 1101 unit
  tests + snapshots green, 0 drift.

### 🧩 G1 — brownfield-Anchor Kani mode (state-struct harness, not Context harness)  [PHASE-1 SHIPPED]

`--kani-impl` emits `build_<handler>() -> crate::<Pascal>` + `accounts.handler(param)`
— the greenfield convention. Real Anchor doesn't match: handlers share one Accounts
struct, take `Context<T>` + `{Xxx}Args` structs, and are associated fns. Both C and D
were only tractable as a **state-struct unit harness** (construct the real state struct,
replicate the short state effect / call the real helper).
- **Evidence:** `tier0-derisk.md` §A (wiring measurement); C + D harnesses both use this shape.
- **Proposed:** a brownfield-Anchor emitter that generates the state-struct harness
  (symbolic state + real invariant()/helper call) instead of the Context harness.
- **Verdict:** FILE (feature). High leverage — it's the shape that actually works on brownfield.
- **Status:** Phase 1 + 2 shipped. `--kani-impl-brownfield` emits the state-struct
  harness; construction is now **generated from the qedspec State** (`state_ctor.rs`,
  `pragma state_struct = <Name>` + G9/G10 `Option`/`Vec` fields) — NOT the IDL. Only
  the effect + validity gate stays agent-fill. Superseded the IDL-driven approach
  (see G6/G7/G8 re-scope).
- **Issue:** #162 (QEDGen/solana-skills)

### 🧩 G2 — helper-target harness mode (not just entrypoint handlers)

impl-Kani is handler-scoped (iterates `spec.handlers`, calls `#[program]` entrypoints).
D's bug lives in a **post-CPI helper** (`evaluate_balance_changes`), unreachable by a
handler-scoped harness that abstracts CPI. Many Solana bugs live in shared helpers.
- **Evidence:** `finding-D-delegation.md` (had to hand-target the helper).
- **Proposed:** let a spec/harness target an internal fn or invariant helper.
- **Verdict:** FILE (feature). Generalizes beyond this program.
- **Issue:** #163 (QEDGen/solana-skills)

### 🧩 G3 — Kani brownfield scaffolding generator

The recurring boilerplate hand-written for C and D: colocate the harness **inside** the
program crate (standalone crates hit spl-token-2022 vs solana-program dep-hell), symbolic
`AccountInfo` via real `SplTokenAccount` + `Pack` (not hand offsets, not wire-format
`deserialize` which blows up to 18.5M SAT vars), `#[kani::stub]` for `Clock::get` +
`-Z stubbing`, unwind tuning.
- **Evidence:** `finding-D-delegation.md` "Kani mechanics learned"; kani_c/kani_d scratch.
- **Proposed:** `qedgen` emits this scaffolding for a brownfield target (or a `qedgen kani-scaffold`).
- **Verdict:** FILE (feature). Turns a multi-hour bring-up into minutes.
- **Issue:** #164 (QEDGen/solana-skills)

### 🧩 G4 — reusable Kani stub library for Solana sysvars

`Clock::get()` (and other sysvars) return errors off-chain, so any impl-Kani reaching
them needs a stub. We wrote `stub_clock_get` + `#[kani::stub(... ::Clock::get, ...)]`
by hand, and discovered it needs `Sysvar` in scope + `-Z stubbing`.
- **Evidence:** clock-stub de-risk (kani_c); `finding-D-delegation.md`.
- **Proposed:** ship a `qedgen`-provided kani stub module (Clock, Rent, other sysvars) +
  auto-add `-Z stubbing` when a harness needs it.
- **Verdict:** FILE (feature).
- **Issue:** #165 (QEDGen/solana-skills)

### 🩹 F1 — `qedgen check` couples to a stale Proofs.lean in Kani-only workflows

Running `qedgen check` on a new spec reported drift against the workspace's old
`Proofs.lean` (from a different spec), noise irrelevant to a Kani-only pass.
- **Evidence:** first `qedgen check` on the settings-invariant brownfield spec.
- **Proposed:** per-spec proof dirs, or a `--kani-only` / backend-scoped check mode.
- **Verdict:** FILE (DX).
- **Issue:** #166 (QEDGen/solana-skills)

### 🩹 F2 — auto-suggest the unwind bound

Users discover by trial that a 32-byte Pubkey `==` lowers to a `memcmp` needing unwind
≥33 (C failed at 4, passed at 40; D used 40). 
- **Evidence:** C first run (unwind failures); `tier0-derisk.md`.
- **Proposed:** qedgen computes a suggested `#[kani::unwind(N)]` from the harness
  (Pubkey/byte-array comparisons ⇒ ≥34) and stamps it in the generated file.
- **Verdict:** FILE (DX).
- **Issue:** #167 (QEDGen/solana-skills)
- **Status:** SHIPPED (v2.41.x). `kani_impl/harness.rs::suggested_unwind(handler, ensures, spec)`
  computes the bound from the harness: any snapshotted `Pubkey`-typed state field or `Pubkey`
  handler param (→ `[u8; 32]` memcmp) ⇒ `#[kani::unwind(34)]`; numeric-only harnesses ⇒
  `#[kani::unwind(4)]`, each with a trailing `//` reason. Wired into both the struct-framework
  (Anchor/Quasar greenfield) and brownfield emit paths, replacing the fixed `2` / `34`. Pinocchio
  keeps its own bound. Regression: `unwind_bound_tracks_pubkey_presence`.

### 📐 M1 — de-risk-smoke-first as a first-class step

Before investing in a full harness, a trivial smoke proof confirms the crate compiles &
verifies under Kani (the biggest brownfield unknown). Caught the standalone-crate dep-hell
early and confirmed the anchor+solana tree is Kani-tractable.
- **Evidence:** C smoke proof; D's two de-risks (parse + Clock stub).
- **Proposed:** encode in the qedgen skill / scout playbook.

### 📐 M2 — falsify-vs-preserve harness discipline

Preserve proofs (C) must be **mutation-tested** for non-vacuity (strict-`<` mutant refuted).
Falsify proofs (D) expect `VERIFICATION: FAILED` as the success signal.
- **Evidence:** C mutation test; D fired counterexample.
- **Proposed:** the skill/scout should require a non-vacuity check on every green preserve proof.

### 🧩 G5 — impl-Kani can't reach instruction-level authorization gates

impl-Kani verifies struct methods + internal helpers (state-struct / helper-target
shapes), but NOT the `validate()` / `#[access_control]` gates that read
`InterfaceAccount` / `Account` / `Signer` from a `Context<T>` — the status /
permission / time-lock / signer checks that ARE the "no unauthorized execution"
crown-jewel properties. The state-struct harness sidesteps accounts entirely; the
greenfield Context harness assumes a struct it can't construct for real Anchor.
- **Evidence:** the execute-gate property (succeeds ⇒ Approved + Execute permission
  + time-lock elapsed) is unreachable by both current shapes.
- **Root cause:** `codegen/kani_impl/` has no symbolic Anchor-`Context` construction path.
- **Proposed:** a Context/instruction harness mode — symbolic `InterfaceAccount`/
  `Account`/`Signer` (PDA-derived keys + `Pack`/Borsh-shaped `kani::any()` data) driving
  the real `validate()`/handler. Composes with #162 phase-2 (IDL layouts) + G4 (#165 sysvar stubs).
- **Verdict:** FILE (feature). High leverage — authorization is why a multisig exists.
- **Issue:** #169 (QEDGen/solana-skills)

> **G6/G7/G8 RE-SCOPED (2026-07):** these were prereqs for an **IDL-driven**
> constructor. That approach was abandoned — the IDL is the *lossy* layer (stale,
> Anchor-0.29 format, strips leading underscores). Construction now comes from the
> qedspec **State** (G1 phase 2, `state_ctor.rs`), which is faithful and checked.
> G6/G7/G8 no longer block construction; they'd only matter if we later
> auto-*derive* the State from the IDL. Left open, off the #162 critical path.

### 🧩 G6 — IDL-driven construction requires a fresh IDL  [RE-SCOPED — off critical path]

A stale committed IDL (field renamed/added since generation) makes the generated
struct-literal constructor reference non-existent fields → silent compile failure.
Observed: a target's IDL had `reserved1/reserved2` where the source is
`policy_seed: Option<u64>, _reserved2`.
- **Proposed:** drift-check the IDL vs the source `#[account]` structs at codegen
  time (hard error), or regenerate-on-build. "Complete qedspec has the IDL" = a *current* one.
- **Issue:** #170 (QEDGen/solana-skills)

### 🧩 G7 — IDL parser can't read Anchor-0.29 account struct bodies  [RE-SCOPED — off critical path]

`spec/idl.rs::Idl` reads `types` + instruction account *references*, but not the
top-level `accounts: [{name, type:{fields}}]` where Anchor 0.29 keeps account
struct bodies — so the layout an IDL-driven constructor needs is unreachable for 0.29.
- **Proposed:** add `accounts: Vec<IdlTypeDef>` (default `ty`); resolve fields from `accounts ∪ types`.
- **Issue:** #171 (QEDGen/solana-skills)

### 🧩 G8 — Anchor IDL is a lossy layout source  [RE-SCOPED — off critical path]

Even a fresh, parseable IDL strips leading underscores (`_reserved2` → `reserved2`)
and elides `#[account]`-only types, so a constructor built from it references
wrong field names. Root cause behind the State-driven pivot.
- **Verdict:** confirms construction must come from the qedspec State, not the IDL.
- **Issue:** #172 (QEDGen/solana-skills)

### ✅ G9 / G10 — DSL `Option<T>` + `Vec<record>` in State fields  [SHIPPED f46a451]

The record/ADT-variant field grammar rejected `Option T` and `Vec <Record>`, so a
State couldn't mirror a real `#[account]` struct — the blocker for State-driven
construction. Parser `param_ty` rule → `TypeRef::Param`; `map_type` renders
`Option<T>` / `Vec<T>` per-context.
- **Issues:** #173 (G9), #174 (G10) — closing on merge.

### ✅ G11 — declare the real state struct name (`pragma state_struct = <Name>`)  [SHIPPED 37304d8]

A brownfield `#[account]` struct's name (`Settings`, `SmartAccount`, …) isn't in the
spec: greenfield naming is `<Program>Account` and the bare `state {}` sugar defaults
to a synthetic `State`. `pragma state_struct = <Name>` names it; `state_ctor` builds
`crate::<Name>` from the canonical `state_fields`. The one thing only the user knows;
absent → the harness keeps its construction `todo!()`.
- **Issue:** #175 (QEDGen/solana-skills) — closing on merge.

### ✅ G12 — symbolic-LENGTH Vec construction OOMs CBMC  [SHIPPED 3d6412f]

State-driven construction emitted `Vec` fields as a symbolic-length build loop
(`let n = any(); assume(n <= 3); while i < n { v.push(any_elem) }`). Under
`#[kani::unwind(N)]` CBMC unwinds that loop AND the real `invariant()`'s own
iteration over the field to N, and models Vec growth/realloc — dominating (OOM)
the SAT problem even for a property that never reads the collection.
- **Evidence:** Squads `Settings`/`set_time_lock` (the #162-p2 PoC) — 54,916 VCCs
  → CBMC out of memory. `assume(n <= 1)` gave the IDENTICAL VCC count (the assume
  prunes solutions, not formula size): it's the length symbolicity, not the
  element count. Fixed-length `vec![elem]` → 12,731 VCCs, 11s, SUCCESSFUL against
  the real `Settings::invariant()`.
- **Fix:** emit fixed-length-K `vec![…]` of symbolic elements; K = `pragma
  kani_vec_bound` (default 1). Raise for a property that reads the collection.
- **Open follow-on:** the PoC's `set_time_lock` property is scalar-only, so K=1 is
  sound; for a property that reads deep into a large collection, the BMC bound
  under-covers silently. A lint (property references a `Vec` field ⇒ warn if
  `kani_vec_bound` is low) would surface the trade-off. Not yet filed.
- **Issue:** #176 (QEDGen/solana-skills) — closing on merge.

## Harness-migration boundary (Squads FV, #162-p2 follow-on)

Migrating the hand-written brownfield harnesses to the generated State-driven
shape: **C (Settings well-formedness) fully migrated** — `change_threshold` +
`set_time_lock`, generated construction (only the effect agent-filled), both
`cargo kani` GREEN against the real `Settings::invariant()`. The remaining
families are a *different verification style* the generated shape doesn't yet
cover; each surfaced a distinct gap (filed, not forced):

### 🧩 G13 — enum (sum-type) State-field construction  [#177]
`state_ctor` bails to `todo!()` on an enum field (only records resolve).
`Proposal.status: ProposalStatus` (6 variants) and `SpendingLimitV2.period:
PeriodV2` (4 unit + `Custom(i64)`) block construction. Fix: symbolic variant
selection from the spec's `ParsedSumType`. Highest-leverage — unblocks F + Proposal.

### 🧩 G14 — sysvar/Clock stub emission in generated impl-Kani  [#178]
`Clock::get()`-reading methods need `#[kani::stub(Clock::get, …)] -Z stubbing` +
a symbolic-Clock stub; the harness emits none. Hand-written in A4/A5/reject-cancel
(Proposal) and D.

### 🧩 G15 — properties beyond scalar-field ensures  [#179]
Collection membership (`signer ∈ approved`), panic-freedom (`reset_if_needed`),
and method-postcondition arithmetic (`remaining == old(remaining) - amount`) —
none expressible as the single scalar `ensures` the harness asserts. The method
CALL fits the AGENT-FILL (2/2) slot; the property language is the gap.

### 🧩 G16 (note) — D (account_tracking) is not a state-struct-mirror target
D constructs raw `AccountInfo` + byte-packed SPL token accounts + a `Balances`
tracker and checks conservation over them — a runtime-object harness, not an
`#[account]`-struct mirror. Likely a separate generator, not this shape. Unfiled
pending a decision on whether it's in scope.
