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
shape. **TWO families now generated + `cargo kani` GREEN against the real code:**
- **C (Settings)** — `change_threshold` + `set_time_lock` (both proofs).
- **F-decrement (SpendingLimitV2)** — `decrement` (22 VCCs), via the full new
  feature stack below.

Five features shipped this pass unblocked F and set up Proposal:
`G13a` enum construction (`be8442c`), `G17` in-module placement +
`::`-path pragma values (`7e1d503`), optional invariant-assume (`7e1d503`),
`G14` Clock stub (`ecb22d6`). Also learned: **nested-field ensures already
work**. Proposal has one feature left (G15a, below).

### ✅ G13a — enum (sum-type) State-field construction  [SHIPPED be8442c, #177]
`state_ctor` bailed to `todo!()` on enum fields. Now emits symbolic variant
selection (`match kani::any::<usize>() % N { … }`) from the spec's sum types
(merged from `spec.sum_types` + `account_types`-with-variants). Unit + named-
payload variants. Validated: the real `Proposal` (6-variant `ProposalStatus`)
and the deeply-nested `SpendingLimitV2` (nested records + enum + Option) both
generate complete, correct ctors. **G13b (open):** tuple variants
(`PeriodV2::Custom(i64)`) need `of (T)` parser syntax + positional emission —
required only by F's `reset_if_needed` (F's `decrement` uses a concrete period).

### ✅ nested-field ensures — ALREADY SUPPORTED (not a gap)
`state.usage.remaining_in_period == old(…) - amount` lowers correctly: the
harness snapshots the top-level field (`let pre_usage = state.usage`) and
preserves the dotted access in the requires-assume and post-assert. So G15's
"method-postcondition arithmetic over nested fields" sub-item is already covered
for the snapshot/assert side.

### ✅ G17 — harness placement / type paths for private-module types  [SHIPPED 7e1d503, #180]
`pragma state_module = <path>` → the ctor names types BARE + the harness gets a
`use super::*` header and is placed INSIDE the defining module
(`#[cfg(kani)] #[path=…] mod`). Unblocked F (`SpendingLimitV2` is behind a
private `mod utils`, so `crate::<Type>` gave 9 "cannot find type" errors). Also
extended `pragma` values to accept `::`-paths. C + Proposal are re-exported to
root, so they keep the default `crate::`.

### ✅ G14 + optional-invariant  [SHIPPED ecb22d6 / 7e1d503, #178]
`pragma kani_stub_clock = <val>` emits `#[kani::stub(Clock::get, stub_clock_get)]`
per proof + the stub fn (run `-Z stubbing`) — for `Proposal::approve`/`cancel`.
`pragma state_invariant = none` skips the pre-state `assume(invariant())` — needed
for Proposal (no `invariant()` method) AND for F-decrement (its `invariant()`
panics under fully-symbolic input — the symbolic ctor is stricter than the scoped
hand-written harness). Validated at codegen on the Proposal harness.

### ✅ G15a — collection membership `contains(coll, elem)`  [SHIPPED 01b3117, #179]
`contains(coll, elem)` in requires/ensures → Rust `coll.contains(&elem)`, Lean
`elem ∈ coll`. AST `Expr::Contains` + MIR `ExprTree::Contains` threaded through
every exhaustive consumer + parser atom; `Vec` snapshots `.clone()` (non-Copy);
`Pubkey` params stay real `Pubkey`. Validated at codegen: the Proposal A5b
harness is fully generated (construction + membership requires/ensures + Clock
stub, only `approve()` agent-filled).

### ✅ G18 — Vec-membership proofs  [RESOLVED by #182 T1, #181 closed]
The A5b harness is codegen-complete + correct, but the PROOF fails: CBMC doesn't
bound `.contains` over `Vec<Pubkey>` after the real `approve()`'s `insert`/`clone`
(`Not unwinding loop … slice_contains … iteration N` at ANY unwind bound; ~39k
VCCs; an explicit `len() <=` assume didn't help). Solver-modeling limit (same
class as G12), NOT a codegen defect. C + F-decrement (scalar/arith) are green and
unaffected. **RESOLVED**: the wall was the 32-byte Pubkey memcmp forcing unwind >=34, not the
Vec length. #182 T1 (Pubkey Eq+Ord abstraction, unwind→5) dissolved it — A5b now
VERIFIES (2477 checks, non-vacuous). No collection remodel needed.

### 🧩 G15b — panic-freedom property class  [#179]
F's `reset_if_needed`: call the method, assert only that Kani finds no panic — no
value assertion. Needs a `panic_free`/`total` property class (emit the call, no
post-assert). Also needs G13b (tuple `PeriodV2::Custom`). Independent of G15a.

### 🧩 G16 (note) — D (account_tracking) is not a state-struct-mirror target
D constructs raw `AccountInfo` + byte-packed SPL token accounts + a `Balances`
tracker and checks conservation over them — a runtime-object harness, not an
`#[account]`-struct mirror. Likely a separate generator, not this shape. Unfiled
pending a decision on whether it's in scope.

## Solana Kani abstraction library (capability, #182)

> **STATUS (graduating from backlog, 2026-07-09; branch `feat/kani-prelude-182`):**
> the T1/T2/T4 abstraction bodies shipped as *codegen-emitted string literals
> re-inlined per harness* (`kani_impl/state_ctor.rs` + `kani_mir/prefix.rs`), and
> the T1 soundness proof had only ever run *once, in a throwaway audit workspace*
> — pinned nowhere. Extraction into a single-source, soundness-proven
> `kani_prelude/` (the Kani twin of `lean_solana/`) is underway.
>
> - **Chunk 1 — SHIPPED.** `kani_prelude/` crate: dependency-free, `#![cfg(kani)]`,
>   standalone workspace. Public API is byte-level (`wide_eq_32` / `wide_cmp_32` /
>   `checked_div_i64` + `mul_div_*`) so it names no Solana type — **Shape 1**
>   (user chose importable crate over vendored `#[path]` module) *without* the
>   anchor-lang version-unification hazard. `cargo kani`: 3/3 harnesses green
>   (pubkey eq/cmp ≡ derived over `[u8;32]`; div ≡ `i64::checked_div` bounded to
>   i8 — unbounded div/mul_div are nonlinear/divider BMC walls, deferred with the
>   contract argument documented).
> - **Chunk 2 — SHIPPED.** `project::write_kani_prelude` embeds (`include_str!`)
>   and materializes the crate as `qedgen_kani_prelude/` beside a program;
>   mirrors `write_lean_solana`. Unit-tested. Not yet hooked into codegen.
> - **Chunk 3 — SHIPPED.** `state_ctor::pubkey_eq_abstract_fn` / `div_abstract_fn`
>   now emit thin adapters over `qedgen_kani_prelude::{wide_eq_32,wide_cmp_32,
>   checked_div_i64}` (fully-qualified paths — no `use` needed; stub attrs
>   unchanged, they target the local adapter names). The over-approx
>   PDA/log/CPI/clock stubs stay inline. `run.rs` delivers the crate beside the
>   program + injects the path-dep, **gated on the emitted harness text
>   containing `qedgen_kani_prelude`** (not a spec predicate — the abstraction is
>   emitted only by the brownfield state-driven shape, NOT greenfield
>   symbolic-accounts, so a spec-level gate over-delivered). Brownfield manifest
>   injection reuses the existing `merge_cargo_toml`/idempotent text-insert.
> - **Chunk 4 — SHIPPED (acceptance).** The smart-account `kani/src/lib.rs` is a
>   hand-curated `c_proofs` module at unwind 40 that does NOT use T1, so a
>   destructive "regen" was the wrong test. Instead, verified end-to-end
>   NON-destructively (scratch dirs; smart-account repo untouched): (i) artifact
>   check — greenfield emits no crate ref / no delivery, brownfield emits the
>   adapter + delivers the crate + injects the dep; (ii) **real `cargo kani`** on
>   a crate-backed `#[kani::stub]` proof nested in a `[workspace]` host —
>   `crate_backed_stub_passes_at_unwind_2` green, proving the path-dep resolves,
>   `wide_eq_32` is callable under kani, and the stub is applied (the derived
>   32-byte `==` loop can't pass at unwind 2). Two bugs caught + fixed: the
>   over-delivery gate, and the delivered crate's `[workspace]` colliding with
>   the host ("multiple workspace roots") — prelude now carries no `[workspace]`,
>   root workspace `exclude`s `kani_prelude/`.
> - **Chunk 3b — SHIPPED.** Folded the `kani_mir/prefix.rs` spec-model `mul_div_*`
>   / `mul_bps_floor` copy onto the crate: `emit_math_helpers` now emits
>   `use qedgen_kani_prelude::{…}` instead of inline `fn` bodies. Delivery gate
>   factored to `run::deliver_prelude_if_referenced` (content-based) and wired at
>   all three gen sites — impl (`--kani-impl`) + both spec-model (`--kani`) paths.
>   `codegen_mir` / `proptest_gen_mir` keep their OWN inline copies (they run
>   outside kani). One snapshot regen'd (def→`use`); verified end-to-end (harness
>   imports the crate, crate delivered, dep injected). The spec-model `pubkey_eq`
>   over `[u8;32]` model fields stays inline — a separate unrolled helper, not a
>   memcmp wall.
>
> **DEFERRED (user, 2026-07-09):** the `kani.yml` CI job that would turn the
> soundness proofs into a *standing* pin — the shape is unsettled; revisit now
> that chunks 1–4 have shipped. Until then the proofs are runnable in-repo
> (`cd kani_prelude && cargo kani`) but not gated in CI, which has no Kani today.

Reusable `#[kani::stub]` abstractions for common Solana types Kani wastefully
bit-blasts, auto-emitted by the brownfield harness (like the Clock stub, G14).
This IS the existing Lean "Trust (axioms)" boundary (SPL Token, PDA, CPI,
Anchor) mirrored on the Kani side. Tiers (prevalence from the Squads target):
- **T1 opaque-token equality** — ✅ SHIPPED (`0c42ef2`): brownfield auto-emits
  `pk_eq_abstract` + `#[kani::stub]` for any Pubkey-touching harness; unwind 34→
  `vec_bound+4`; `pragma kani_abstract_pubkey = off` opts out. Kani-proven sound;
  both green C proofs re-verified at unwind 5. `[u8;32]`/`[u8;64]` extend it.
- **T2 trusted crypto** — PDA `find_program_address` (=sha256; 16 files), sha256/
  keccak/blake3, ed25519 verify. Axiomatize (uninterpreted + injectivity).
- **T3 trusted serde** — 📐 METHODOLOGY (not an auto-stub). Borsh round-trip is a
  confirmed bottleneck (times out at unwind 6 even bounded — memchr/memcmp), but a
  sound generic stub is impractical (round-trip identity is stateful; try_from_slice
  is generic/no-Arbitrary; multi-type event path). Fix = harness design: the
  replicate-the-effect style AVOIDS serde (C/F/A5b never hit it). Escape hatch:
  per-type deserialize stub → symbolic ctor, agent-wired. See #182.
- **T4 runtime/host** — ✅ SHIPPED (`496b5c8`): `pragma kani_stub_log` (sol_log/
  sol_log_data → no-op) + `pragma kani_stub_cpi` (invoke/invoke_signed → Ok(())).
  Opt-in; validated on micro-harnesses. Rent/other sysvars extend the Clock pattern.
- **T5 collections over opaque tokens** — `Vec<Pubkey>::contains`/`binary_search`
  (18 files). T1 kills inner cost; outer iteration needs a bounded model. **A5b
  (#181) sits here.** Prototype: T1 (Pubkey) — highest leverage, shrinks the A5b
  formula that OOM'd z3.

---

## Session: brownfield Anchor multisig (2026-07-08)

Migrated 3 hand-written brownfield impl-Kani harnesses (approve-threshold /
reject / cancel soundness) to the generated State-driven shape — stress-testing
the `is .Variant` + `len()` render paths and the non-`Copy` snapshot logic — then
attempted 3 round-2 advisory findings as FV targets. Four codegen bugs surfaced
and were fixed in-session (all with regression coverage); one placement gap and
one scope-boundary heuristic remain. Ranked most-leverage first.

### 🩹 G17b — in-module brownfield harness can't name types in a *private sibling* module  [FIXED]

The `pragma state_module` in-module placement (G17/#180) emits only `use super::*`
as the import header (`kani_impl/brownfield.rs:75-86`). That reaches the placement
module's own declared + `pub use` items — but NOT a private sibling module's types
nor the placement module's own private `use` imports. When the mirrored State
references a type declared in a *different* private module, the generated ctor
names it BARE and it fails to resolve; the agent had to hand-add explicit
`use crate::…::{…}` lines reachable by absolute path from within the enclosing
public module. Distinct from #180 (which solves "the mirrored struct itself is
behind a private module" via placement) — this is "the mirrored struct *references*
other types in another private module."
- **Evidence:** `kani_impl/brownfield.rs:75-86` (in-module branch emits `use super::*;`
  only); `kani_impl/state_ctor.rs:73-84` (`is_in_module` / `type_prefix` carry no
  per-type module path — the spec carries only type NAMES). `rg harness_use crates/`
  → empty (no escape hatch exists).
- **Root cause:** `brownfield.rs` has one fixed import header per placement mode and
  no per-referenced-type module-path info; the spec's State declares type *names*,
  not their defining modules.
- **Proposed:** (a) a `pragma harness_use = <path>,…` escape hatch that injects extra
  `use` lines into the harness header (cheap, unblocks now); and/or (b) resolve each
  referenced non-primitive type's defining module during `adapt` and emit the `use`
  set automatically.
- **Verdict:** FILE (friction/gap). Cross-links G17/#180. Leverage: any brownfield
  program whose account struct pulls field types from a second private module —
  common in real Anchor `state::*` trees.
- **Issue:** #183 (QEDGen/solana-skills)
- **Fixed:** option (a) shipped. `pragma harness_use = <path>` (repeatable, one `use`
  path per line — a `::*` glob or a single item; the parser's `path_value` now accepts
  a `*` segment). `ParsedSpec::pragma_values(key)` collects all; `brownfield.rs` emits
  each as `use <path>;` under one `#[allow(unused_imports)]`, after the placement
  header, in source order. Test: `brownfield_harness_use_pragma_injects_extra_imports`
  (`kani_impl/tests.rs`). Documented in `references/qedspec-dsl.md` §Pragmas. Option
  (b) (auto-resolve the defining module) left open — the spec has only type names, so
  (a) puts the one unknowable fact (the module path) in the author's hands.

### 🐞 B2 — `is .Variant` Rust lowering emitted non-compiling stub  [FIXED]

`Expr::IsVariant` (`spec/chumsky_adapter/rust.rs`) and `ExprTree::IsVariant`
(`codegen/rust_codegen_util/tree_render.rs`) both rendered
`matches!(x, /* ty */::V(..))` — a leading-`::` **comment** path (invalid Rust)
and an always-tuple `(..)` pattern (wrong for struct/unit variants). So `is .Variant`
in *any* Rust-target output (brownfield Kani, proptest, Anchor scaffold) failed to
compile. High severity: the dominant status-enum guard shape
(`state.status is .Approved`).
- **Evidence:** old `rust.rs` / `tree_render.rs` bodies `matches!({}, {}::{}(..))`
  with `"/* ty */"` literal; migrating the 3 vote-registration harnesses hit it.
- **Root cause:** the renderer had no enum-type / variant-shape info at emission time.
- **Fix (shipped):** `adts` registry (enum→variant→is-struct) on `TypeEnv`
  (`chumsky_adapter/mod.rs:102`) + `resolve_variant(hint, variant)`
  (`mod.rs:281`, hint from `path_type_name`, global unique-name fallback);
  `ExprTree::IsVariant` enriched with build-time `enum_ty` + `struct_variant`
  (`mir/expr_tree.rs`), populated in `chumsky_adapter/tree.rs`. Renders
  struct→`Enum::V { .. }`, unit→`Enum::V`. Lean path unaffected (routes through the
  per-variant `isV` helper). Regression:
  `brownfield_isvariant_and_len_render_and_clone_nonstate_copy_field`
  (`kani_impl/tests.rs`).
- **Verdict:** FIXED in-session; no new issue (complete + tested). Sibling of the
  enum-*construction* work G13a/#177.

### 🧩 G19 — `len(coll)` DSL builtin  [FIXED]

No collection-length builtin existed, so a threshold-over-Vec ensures
(`len(state.approved) >= threshold`) was unwritable. Added `Expr::Len` /
`ExprTree::Len`, threaded through every exhaustive consumer — parser atom
(`chumsky_parser/expr.rs:183`), `ast`, `canon`, `adapt`, `infer`→`Nat`,
Rust→`(coll.len() as u64)`, Lean→`(coll).length`, `tree`, `num_kind`, effect
bare-RHS, and the bound-guard walk — mirroring the `contains` builtin (G15a/#179).
- **Evidence:** `chumsky_parser/expr.rs:183` (`len_atom`); render sites in
  `rust_codegen_util/tree_render.rs` + `lean_gen_mir/tree_render.rs`. Covered by the
  same regression test as B2 (asserts `(post_votes.len() as u64) >= quorum`).
- **Verdict:** FIXED in-session; no new issue. Reusable across any Vec/collection spec.

### 🐞 B3 — brownfield snapshot MOVED non-`Copy` non-`Vec` state fields  [FIXED]

`kani_impl/harness.rs`'s snapshot RHS gate (`state_field_is_vec`) only matched a
`Vec ` prefix, so a `Clone`-not-`Copy` enum/record field (e.g. a `status` ADT) was
`let pre_status = state.status;` — a partial move that broke the subsequent
`&mut state` method call. The doc comment already *claimed* non-Copy fields must
clone, but the logic only covered `Vec`.
- **Evidence:** old `state_field_is_vec` (`t.trim_start().starts_with("Vec ")`);
  migrating a harness with an ADT `status` field failed to compile.
- **Root cause:** the Copy/Clone predicate under-approximated the non-Copy surface.
- **Fix (shipped):** `state_field_needs_clone` (`harness.rs:377`) + `is_copy_scalar_ty`
  (`harness.rs:391`) — clone everything except fixed-width ints / `Bool` / `Pubkey`
  / `Fin[N]`. Same regression test asserts `state.status.clone()` in both snapshots.
- **Verdict:** FIXED in-session; no new issue.

### 🐞 B4 — crate-level brownfield harness lacked a `use` for the bare enum name  [FIXED]

A crate-level (non-`state_module`) brownfield harness whose ensures used `is .Variant`
emitted no import for the bare enum name: `matches!(x, <Enum>::<V> { .. })` names the
enum BARE (the DSL type name) while the ctor uses `crate::` paths, and the header only
existed for the in-module branch. Result: `cannot find type <Enum>`.
- **Evidence:** old `brownfield.rs` emitted `use super::*` only inside the `in_module`
  branch; the `else` branch had no import.
- **Root cause:** the bare-name `matches!` render (B2) and the `crate::`-qualified ctor
  disagree on how the enum is named; the crate-level branch imported neither.
- **Fix (shipped):** `else` branch now emits `#[allow(unused_imports)]\nuse crate::*;`
  (`brownfield.rs:79-86`). Regression test asserts `use crate::*;` present.
- **Verdict:** FIXED in-session; no new issue.

### 📐 M3 — missing-invocation findings: a BMC harness proves the pure gate SOUND, not the bypass  [ENCODE]

All 3 round-2 advisory findings were *missing-invocation* bugs: an unwired guard +
`invoke_signed`; a mutate-without-`exit()` serialize drop; an async path skipping a
pure allowlist gate the sync path calls. QEDGen's symbolic-input BMC verifies
properties of **executed** code — it structurally cannot refute "a correct check is
never called on path X" (a call-graph fact, not a value property). Faithful harnesses
for these need unbuilt abstraction tiers (symbolic `AccountInfo` + `invoke_signed`
stub = #182 T4; Borsh round-trip = #182 T3). The one tractable finding had a PURE gate
(`&self, &payload`, no `AccountInfo`/CPI) → verified green as a *regression guarantee
for the fixed path*, not a refutation of the bypass.
- **Evidence:** the 3 findings' shapes above; the pure-gate finding is the only one
  that generated a complete harness (no T3/T4 dependency).
- **Encode (heuristic):** when a finding is a missing-call-site / absent-guard bug, a
  BMC harness proves only that the guard *itself* is SOUND on the path that calls it —
  it does NOT prove the bypass path is safe. Pin the abstraction tier each finding
  class needs before promising a repro. Cross-links #182 (tier map).
- **Verdict:** ENCODE (skill/scout playbook). No issue.

### 🧩 G20 — guard-enforcement (reject) harness mode  [FIXED]

The "must-fail / should-reject" property class kept surfacing (A5a duplicate-vote
rejection; the reject-half of the missing-invocation findings above): QEDGen could
prove what holds *after* a successful call (ensures-preservation) but not that the
code *rejects* a violated precondition. Shipped `pragma kani_reject = on` — for each
brownfield target handler with a `requires`/`when` guard, emit a
`verify_<handler>_rejects` proof that assumes the guard is VIOLATED
(`kani::assume(!(guard))`) and asserts the real handler returns `Err`
(`assert!(!ok, …)`). Same agent-fill (the real call) as the ensures harness; snapshots
only the guard's fields. No new DSL syntax — reuses `requires … else E`.
- **Evidence:** `kani_impl/harness.rs::emit_brownfield_reject_harness` (+ extracted
  `emit_impl_proof_attrs` / `emit_symbolic_state` shared with the ensures emitter);
  `brownfield.rs` gates on `pragma_value("kani_reject")`. Validated on A5a: the real
  `Proposal::approve` binary_search dedup — `cargo kani` SUCCESSFUL (reject + ensures).
- **Root cause / gap:** the ensures emitter was the only harness shape; a declared
  `requires` had no enforcement proof.
- **Verdict:** FILE→FIXED (gap). Partially operationalizes M3 (the "guard is SOUND"
  half is now a first-class proof). Tests: `brownfield_kani_reject_emits_guard_enforcement_harness`,
  `brownfield_without_kani_reject_pragma_omits_reject_harness`. Docs:
  `references/qedspec-dsl.md` §Pragmas.

### 🩹 F3 — release build needs a manual `cp target/release/qedgen bin/qedgen` before codegen reflects a fix  [backlog-only]

Codegen/interactive runs invoke `bin/qedgen`; a `cargo build --release` that forgets
the `cp` step (per CLAUDE.md "always copy to bin/") leaves `bin/qedgen` stale, so a
just-fixed codegen bug appears unfixed. Hit once this session (had to re-`cp` after an
edit). The snapshot harness already rebuilds (`tests/common/mod.rs`), but the manual
`bin/` copy has no such guard.
- **Evidence:** CLAUDE.md build step `cargo build --release && cp … bin/qedgen`;
  `rg "older than target" crates/` → no staleness check exists.
- **Proposed:** a single build entrypoint that always copies (a `just build` /
  `[alias]` in `.cargo/config.toml` / Makefile target) so `bin/` can't go stale — the
  robust fix. A binary self-comparing mtime to a sibling is fragile; prefer the alias.
- **Verdict:** dev-mode-only friction (end users install via the skill, never touch
  `bin/` vs `target/`). Backlog-only / doc-note — not a user-facing qedgen shape, so
  no issue. Flagged for a maintainer to fold into CLAUDE.md's build guidance.

### 🧩 G20b — reject-mode covers requires-only handlers  [FIXED]

`pragma kani_reject` initially only iterated the ensures-bearing `emit_targets`, so a
handler with a guard but no `ensures`/`effect` (a pure validator — exactly where guard
enforcement matters) got no reject proof. Now the reject loop iterates all handlers
with a guard, and an `extra_brownfield_mode` bypass (`kani_impl/mod.rs`) keeps the
emitter from bailing when the spec is guard-only. Test:
`brownfield_kani_reject_covers_requires_only_handler`.

### 🧩 G13b — tuple-variant construction (`of <Type>`)  [FIXED]

The real `PeriodV2::Custom(i64)` is a Rust TUPLE variant the DSL couldn't express
(only `of { named }` struct variants), so the State-driven Kani ctor couldn't build a
symbolic period covering Custom. `of <Type>` now parses a single-field tuple variant;
the positional field is named "0" (impossible for a real ident-named field →
collision-safe marker), and `emit_enum` renders `Enum::V(val)`. Verified `Custom of I64`
→ `PeriodV2::Custom(kani::any())`.
- **Scope / follow-up:** Kani construction only. `is .Variant` and the Lean ADT backend
  still assume unit/struct shapes; a tuple `is .Variant` needs a 3-way `VariantShape`
  refactor of the `adts` registry + `resolve_variant` + `ExprTree::IsVariant` (deferred —
  not needed for F's panic-free harness, and fails loudly rather than silently).

### 🧩 G15b — panic-freedom harness mode (`pragma kani_panic_free`)  [FIXED]

For `()`-returning methods whose only property is that they don't abort. Emits
`verify_<handler>_panic_free`: construct symbolic state, assume the handler's
`requires` guard, CALL the real method (agent-fill), no assertion — Kani's built-in
unwrap/overflow/div/index/panic checks do the verification. Validated on
`reset_if_needed`: `cargo kani` SUCCESSFUL for the standard periods.

### 🩹 G15c — `invariant()` panics on symbolic input, so panic-free harnesses can't assume it  [NEEDS-TRIAGE]

A panic-free proof of a method whose safety depends on the struct's `invariant()`
can't `kani::assume(state.invariant().is_ok())` when `invariant()` itself panics on
fully-symbolic input (e.g. `.unwrap()`s an `Option` field). Workaround used for F:
reconstruct the needed invariant clauses as explicit handler `requires`
(`0 <= last_reset <= now`). But the `PeriodV2::Custom(i64)` case needs "if period is
Custom(s) then s > 0".
- **Proposed:** (a) the tuple-variant `is .Variant` shape — SHIPPED (G13b, `64bf14b`),
  but it's only a boolean is-test, NOT the payload. Excluding `Custom(0)` needs variant
  **payload binding** (a `match state.period { Custom(s) => s > 0, _ => true }` or an
  `is .Custom(s)` binder) to write `(period is .Custom(s)) implies s > 0` — a distinct
  DSL feature (variant payload access) that is the real remaining unblock. And/or (b) a
  codegen mode that assumes each invariant *clause* as a precondition without calling
  the composite (panicking) `invariant()`.
- **Evidence:** `spending_limit_v2.rs` reset_if_needed + invariant; two Kani iterations
  (checked_sub overflow on negative last_reset → added `last_reset >= 0`; then GREEN for
  standard periods). See `formal_verification/VERIFICATION.md` F-reset note. The is-test
  alone can't reach the `Custom` payload, so the Custom>0 precondition is inexpressible.
- **Verdict:** FILE (gap). Leverage: any panic-free / precondition proof of a method
  gated by a rich invariant with unwraps, or any property over an enum variant's payload.
- **UPDATE — expressibility CLOSED, moved to a solver wall.** Variant payload binding
  SHIPPED (`57a91fd`): `match state.period with | Custom s => s > 0 | _ => true` in a
  requires renders `match pre_period { PeriodV2::Custom(s) => s > 0, _ => true }`
  (enum-resolved, shape-correct arms + `_` wildcard). F's Custom precondition is now
  fully expressible. But the Custom harness then divides `passed / reset_period` by a
  SYMBOLIC `i64`, which stalls BOTH CaDiCaL (SAT bit-blasting) AND z3 (`pragma
  kani_solver = z3`, `1219c00`) — z3 solved 5 checks in ~3s each then ground on the
  division check for 22+ min at 99% CPU. So the remaining gap is **solver
  tractability of symbolic `checked_div`**, not codegen. Next: abstract/bound the
  division (same pattern as the #181 Pubkey-memcmp wall — e.g. a `#[kani::stub]` on the
  divisor path, or bound the Custom period). Standard periods remain GREEN.
- **UPDATE 2 — division abstracted (G15e), residual is the multiply-back.** `pragma
  kani_abstract_div` SHIPPED (`9d24a89`) stubs `i64::checked_div` with an exact-contract
  symbolic quotient — no divider circuit. The F Custom harness then advances PAST the
  division check that stalled 22 min, but z3 next stalls ~8 min on the **multiply-back**
  `periods_passed * reset_period` (symbolic × symbolic i64). A multiply, unlike a
  divider circuit, has NO cheaper contract (a multiply's defining relation *is* a
  multiply), so the div-abstraction trick doesn't transfer. F Custom is thus a *chain*
  of nonlinear-symbolic-arithmetic walls: division cleared, multiply-back inherent.
  Remaining options: bound the Custom period's bit-width (narrows the multiply, weaker
  proof) or accept standard-periods-green + Custom-documented. Filed as G15e.

### 🧩 G15e — abstract `i64::checked_div` (`pragma kani_abstract_div`)  [FIXED]

A symbolic 64-bit divisor bit-blasts a sequential divider circuit that stalls both
CaDiCaL and z3. `pragma kani_abstract_div = on` stubs `i64::checked_div` with
`checked_div_abstract`: a fresh symbolic quotient pinned by division's EXACT contract
(`a = q·b + r`, `|r| < |b|`, `sign(r) = sign(a)`, in i128; preserves the `b==0` /
`MIN/-1` None cases). Exact (unique quotient) → sound both ways, like the #182
Pubkey/PDA stubs; removes the divider circuit. Validated: the F reset Custom harness
clears the division stall. Test: `brownfield_kani_abstract_div_emits_stub`. NOTE: only
addresses division — a symbolic multiply is a separate wall (see G15c UPDATE 2).

### 🧩 G15d — `pragma kani_solver` bakes `#[kani::solver(z3)]` into the harness  [FIXED]

A harness that divides/mods by a symbolic value blows up the default SAT backend;
z3/cvc5 reason about bit-vector division natively. `pragma kani_solver = <solver>`
emits `#[kani::solver(<solver>)]` after `#[kani::proof]` on every generated proof,
so the solver requirement is baked in + reproducible without a `cargo kani --solver`
flag (`1219c00`). Test: `brownfield_kani_solver_pragma_bakes_solver_attr`.

### 📐 M4 — E-A / E-B (round-2 policy findings): per-finding tractability, code-grounded  [NEEDS-TRIAGE]

Traced both remaining novel findings to the exact code with the current toolchain
(CPI/log/clock stubs, Pubkey/PDA abstraction, reject + panic-free harnesses, payload
binding, `kani_abstract_div`). Neither is a same-session harness; each needs a
specific, well-scoped new capability.

**E-A (HIGH) — ProgramInteraction hook force-signs as `HOOK_AUTHORITY`.**
`Hook::execute` (`program_interaction.rs:408`) marks any runtime hook account whose
key `== HOOK_AUTHORITY_PUBKEY` as a signer and `invoke_signed`s. The guard
`ProgramInteractionHookAuthorityCannotBePartOfHookAccounts` (`errors.rs:176`) is
defined but wired nowhere. Two angles:
- *Runtime angle* (drive `Hook::execute`): needs symbolic `AccountInfo[]` construction
  (#182 T4) — the CPI call is stubbable now, but building the `AccountInfo` array to
  pass in is the wall.
- *State angle* (the better one): the hooks ARE in the policy state (`pre_hook` /
  `post_hook: Option<Hook>`, each with `account_constraints: Vec<AccountConstraint>`,
  `AccountConstraintType::Pubkey(Vec<Pubkey>)`), and `invariant()` is PURE — so a
  guard-enforcement (reject) proof over `invariant()` could demonstrate the missing
  guard with no `AccountInfo`. Blockers: (a) the guard predicate — "`post_hook`'s
  account constraints include `HOOK_AUTHORITY`" — needs nested-structure predicate
  navigation the DSL lacks: `Option` access + `exists` over a `Vec` + enum-payload
  binding (`AccountConstraintType::Pubkey(v)`) + nested `contains`; (b) mirroring the
  deep policy state (Option<Hook>, Vec<enum-with-Vec-payload>, Vec<SpendingLimitV2>).
- **Proposed:** a nested-predicate DSL feature — `exists x in state.<vec>, <pred(x)>`
  with `Option` access and enum-payload binding — is the reusable unlock (covers any
  "some element of a collection violates P" property). Then E-A is a reject proof over
  the pure `invariant()`. This is the highest-leverage next DSL feature.
- **UPDATE — nested-predicate feature SHIPPED (blocker (a) cleared).** `exists|forall x
  in <coll>, pred` (`b1724c7`, `Expr`/`ExprTree::QuantIn` → `coll.iter().any|all`) +
  `match`/`is` on `Option` fields (`5b22050`, builtin `Some`/`None`) now compose to the
  exact E-A predicate: verified `match state.post_hook with | Some h => (exists k in
  h.keys, k == auth) | None => false` renders `match … { Option::Some(h) =>
  (h.keys.iter().any(|k| k == auth)), Option::None => false }` (enum-resolved, payload
  bound, field access, `_`/None). Remaining for E-A is blocker (b) only — MECHANICAL, no
  new DSL: (1) mirror the ~10-type policy state (`ProgramInteractionPolicy` →
  `Option<Hook>` → `Vec<AccountConstraint>` → `AccountConstraintType` enum tuple-payload
  → `Vec<Pubkey>`/`Vec<DataConstraint>`, plus `Vec<InstructionConstraint>`,
  `Vec<SpendingLimitV2>`, `DataConstraint`) via `state_struct` — the ctor handles
  Option/Vec/nested-record/enum/tuple-Vec-payload individually but is untested this deep;
  (2) reference the external `HOOK_AUTHORITY_PUBKEY` const in the predicate; (3) it's a
  reject harness whose FAILURE is the counterexample (the guard is missing). Left as a
  scoped follow-on — the reusable DSL unlock is done and validated to reach the predicate.

**E-B (MED) — `SettingsChange` doesn't persist.** `execute_payload`
(`settings_change.rs`) mutates an in-memory `Account<Settings>` (from a remaining
account) via `modify_with_action`, reallocs, logs — but never `settings.exit()` /
serializes back, so the change is dropped on return. The property ("the account DATA
BUFFER reflects the change") is inherently about Borsh serialize-on-exit — needs
account-data-buffer + serialize/deserialize modeling (#182 T3 serde), which is the
documented-intractable tier. No pure-fn angle (the bug is a missing side effect).
- **Proposed:** T3 serde modeling, OR accept the live anchor-ts repro as the evidence.

**Both:** the live anchor-ts repros are the correct, existing, sufficient evidence.
QEDGen's contribution would be the fixed-behavior regression spec — E-A via the
nested-predicate feature + pure `invariant()`; E-B via T3. Ranked: E-A's
nested-predicate DSL feature is worth building (broadly reusable); E-B waits on T3.
- **Verdict:** FILE (methodology + two scoped feature asks). Leverage: nested-predicate
  navigation unlocks a whole class of "collection element violates P" audit properties.

---

## Session: E-A guard-enforcement harness — nested-container Kani tractability (2026-07-09)

Continuing M4/E-A: **mirror the deep policy state and land the guard-enforcement
(reject-shaped) harness** over the pure `invariant()`. The state mirror + nested
predicate (`match state.<opt> with Some h => not (exists c in h.<vec>, match
c.<enum> with Pubkey pks => contains(pks, CONST) | _ => false) | None => true`)
GENERATE correctly through `state_struct` + `QuantIn` + Option/enum `match`. The
obstacle was purely **verifier resource**: the naive harness was **69,545 VCC(s)**
(1.55M program steps) — CBMC's SAT backend OOM'd and its SMT2 (z3) backend crashed.

### 🧩 R1 — Nested-container Kani reductions (SHIPPED, 4 codegen features)  [FIXED]

Root cause of the blowup: the impl-ensures harness DEEP-CLONES a non-`Copy`
nested container (`Option<Hook>` carrying `Vec<AccountConstraint>` carrying
`Vec<Pubkey>`) up to **three times** — a dead pre-snapshot, a live post-snapshot,
and again inside the `match` scrutinee — each copy generating a `drop_in_place` /
`RawVec` storm. Four mechanical, general codegen fixes (each with a regression
test in `kani_impl/tests.rs`), measured on the same harness:

1. **Snapshot pre/post split** (`collect_snapshot_fields_split`). A field read
   only via `post.<x>` gets NO dead `pre_<x>` clone (and vice-versa); only
   effect-participating fields snapshot both sides. Drops the dead pre-clone.
2. **`pragma kani_option_none = <field>`** (`state_ctor`). Builds an `Option<_>`
   field as `None` — no `Some` payload construction — pruning a symbolic
   sub-state the property never reads (companion to `kani_vec_empty`).
3. **Owned-snapshot `match &(...)`** (`chumsky_adapter/rust.rs`). A scrutinee that
   is a snapshot local (`pre.X`/`post.X` → owned `pre_X`/`post_X`) is matched by
   REFERENCE, not `.clone()` — its struct/collection payloads are read by-ref.
   Non-snapshot `&`-places (`c.field` under `.iter()`) keep `.clone()`.
4. **Post-snapshot move** (`emit_brownfield_handler_harness`). The post-snapshot
   MOVES the field out of `state` (last read; no CPI-assume splice follows), not
   `.clone()`.

Cumulative effect (each roughly halves the instance): **69,545 → 50,847 → 35,590
→ 20,322 VCC(s)** (9,097 after simplification), symex **217s → 20s**, program
steps **1.55M → 464k** — a **5× reduction**. All snapshot suites + 1128 unit
tests green. **Verdict: FIXED in source.**

### 📐 R2 — CBMC/Kani wall on SYMBOLIC nested `Vec` containers  [FILE — upstream]

Even at 9,097 VCC(s) the proof will not close, and the failure is **backend, not
size**:
- **z3 / SMT2:** `map::at: key not found` — a CBMC internal crash during
  SSA→SMT2 conversion. Reproduces at 9k AND 21k AND 29k VCC(s), so it is
  **construct-triggered, not size-triggered**: the `drop_in_place::<[T]>` /
  `RawVecInner::deallocate` machinery emitted for symbolic-length nested `Vec`s
  is what the SMT2 converter cannot lower.
- **CaDiCaL / SAT:** no crash, but bit-blasting the symbolic-`Vec`/`Option`
  combinatorics grinds indefinitely (>10 min, no verdict) once past the OOM
  threshold at ~29k.

So a property that navigates a **symbolic** heap-allocated nested container
(`Vec<Struct{ Vec<_> }>`) is currently beyond CBMC's practical reach on BOTH
backends — independent of QEDGen. The R1 reductions push the floor 5× lower but
don't cross it. **Mitigations for a future pass:** (a) a `kani_vec_concrete` /
fixed-shape ctor mode that builds the container with CONCRETE structure and only
the *leaf value under test* symbolic (collapses the `drop_in_place`/RawVec
symbolic machinery to a bounded, SMT2-convertible shape); (b) a concrete-witness
harness (specific malicious element) — trivially closes but is redundant with the
live client repro. **Verdict: FILE** — the reductions are the reusable win; the
symbolic-nested-`Vec` close waits on a concrete-shape ctor mode or upstream CBMC.

**E-A status:** state mirror ✅, nested predicate ✅, harness generates ✅ and is
5× smaller ✅; symbolic CLOSE blocked by R2 (CBMC, upstream). Evidence stands on
source analysis (`invariant()` structurally ignores hooks) + the live client
repro; QEDGen's artifact is the regression-shaped harness + the R1 reductions.

---

## Session: smart-account FV spec cleanup (2026-07-09)

Reorganizing the audit workspace's loose root-level `.qedspec` files into a
grouped `formal_verification/specs/` tree surfaced a stray artifact: a full
generated greenfield Anchor crate (`programs/`, `# ---- GENERATED BY QEDGEN ----`)
sitting in a **brownfield** audit dir, next to the real program it duplicates.

### 🩹 R3 — struct-mirror spec still scaffolds a greenfield crate under default codegen  [FIXED]

`--kani-impl-brownfield` skips the greenfield Rust scaffold, but the skip keyed
ONLY on that invocation flag (`run.rs` Codegen dispatch). A plain `qedgen codegen
--target anchor` (the no-flag default) or a bare `--kani-impl` on a spec that
declares `pragma state_struct = <RealStruct>` / `pragma state_module = <path>`
STILL ran `codegen_mir::generate` — synthesizing a throwaway Anchor crate for a
program that, by construction, already exists. That crate then carried
`#[qed(verified, spec = "…")]` stamps that dangle the moment the spec moves.
- **Evidence:** the audit's untracked `programs/` (`smartaccountsettingsmirror`)
  generated from `settings/mirror.qedspec`; noticed during the spec reorg.
- **Root cause:** brownfield-ness was a per-invocation flag, not derived from the
  spec — even though the code already documents `pragma state_struct` as "a
  brownfield `#[account]` struct" (`state_ctor.rs:333`), and `codegen_mir` never
  reads these pragmas (so the scaffold they'd produce is pure noise).
- **Fix (shipped):** `ParsedSpec::is_struct_mirror()` (single source of truth:
  `state_struct` ∨ `state_module` present) now also gates the scaffold-skip, so a
  mirror spec emits no crate under ANY invocation (default, `--kani-impl`,
  `--all`). Verified safe: no bundled fixture/example uses these pragmas, and the
  scaffold path ignores them → zero snapshot movement. Regression test
  `struct_mirror_codegen_skips_greenfield_scaffold` (codegen_smoke.rs) asserts
  both directions (pragma present → no scaffold; pragma dropped → scaffold
  returns). 1128+ unit tests + all snapshot suites green; fmt + clippy clean.
- **Verdict: FIXED in source.**

### 🧩 R4 — drop-suppression cracks the R2 CBMC wall (brownfield ManuallyDrop + by-ref)  [FIXED]

R2 concluded a property navigating a symbolic nested `Vec` container was beyond
CBMC on both backends. Re-running the shipped E-A harness (kani 0.67.0) confirmed
it: **20,322 VCCs → OOM in propositional reduction**, the blowup entirely in
`drop_in_place::<[AccountConstraint]>` / `RawVecInner::deallocate` — the symbolic
state's **destructor**, not the property. So the fix is to never emit that
teardown. Two changes to the brownfield handler emitter (`kani_impl/harness.rs`):

1. **`ManuallyDrop` the symbolic state** (`emit_symbolic_state`) — the generated
   `symbolic_<struct>()` is wrapped in `core::mem::ManuallyDrop::new(...)`, so its
   nested-`Vec` destructor is never generated. Read unchanged via `Deref`/`DerefMut`.
2. **By-reference post reads** (`rewrite_ensures_post_to_state`) — `post.<field>`
   lowers to `state.<field>` (a place, matched by ref) instead of a moved-out
   `post_<field>` snapshot, and the defensive `.clone()` on an inner `.iter()`
   match scrutinee (`match (c.kind).clone()` → `match &(c.kind)`) is stripped.
   No owned heap value is bound, so no `drop_in_place` is emitted anywhere.

Sound: skipping a destructor cannot change a property checked before it, and the
harness is `#[cfg(kani)]`-only. Both together take the E-A harness from **20,322
VCCs / OOM** to **2,401 → 528 after simplification, symex 1.7s, closes in ~5s** —
and the verdict flips from "no result" to a genuine **counterexample**: Kani finds
a valid `ProgramInteractionPolicy` whose hook constrains `HOOK_AUTHORITY`, i.e. the
guard-enforcement property is FALSE (Finding A, machine-checked; flips to PASS once
the guard is wired — the regression gate).
- **Evidence:** `hook_authority.qedspec` regenerated → `cargo kani` closes; before/after VCC + timing above.
- **Scope:** brownfield handler-ensures harnesses only. Kept the clone-form everywhere
  else (proptest, greenfield, `requires` guards) for the scalar-payload-binder case
  (`Custom(s) => s > 0`). Regression: `brownfield_drop_suppression_manually_drops_and_reads_by_ref`
  (nested `Option<Hook{Vec<Con{Kind}>}>` shape); 6 existing brownfield tests updated
  off the superseded post-snapshot-move assertion. All suites + fmt + clippy green.
- **Verdict: FIXED in source.** Supersedes the R1 post-snapshot-move optimization and
  the R2 "FILE — upstream" verdict for the brownfield path.
