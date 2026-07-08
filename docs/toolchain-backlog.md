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
