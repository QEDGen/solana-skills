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
- **Status:** Phase 1 shipped — `--kani-impl-brownfield` emits the state-struct harness (2 `todo!()` agent-fill: struct construction + effect). Phase 2 (IDL-driven construction) open.
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
