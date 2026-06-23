# qedsvm Discharge — Design Note (v3.0 target)

Status: **design** (no code yet; supersedes the forward-links in
`docs/prds/RELEASE-v2.28.md` and the v3.0 thread in
`project_v229_v230_sequencing`)
Author: agent-driven exploration, 2026-06-14
Scope: replace qedgen's two author-asserted trust surfaces — the bundled
Tier-1 CPI callee `ensures` axioms and the sBPF refinement-bridge `sorry`
stubs — with Lean proofs **discharged against the pinned program bytes** via
the `qedsvm` package.

This is not a release PRD. It is the architectural sketch that the v2.28
axiom docstrings (`crates/qedgen/data/proofs/spl/Token.lean:22-28`) and the
`qedbridge` `.refines` stubs both forward-reference. It states what must be
true — on the qedsvm side and the qedgen side — for "axiomatized against a
`binary_hash` pin" to become "proven against the bytes at that pin," and
phases the work so the first honest discharge ships without waiting on the
whole story.

---

## §0 — The one-paragraph picture

qedgen pins the *bytes* of a dependency (SPL Token, Metaplex, a user's
compiled sBPF program) with a `binary_hash`, and `verify --check-upstream`
re-dumps the deployed program and compares the hash. But nothing today
*proves* those bytes implement the contracts we state about them — the
contracts are `axiom`s (CPI callees) or `sorry` stubs (the sBPF bridge).
`qedsvm` gives Lean a model of sBPF execution plus a refinement layer
(`AsmRefinesFieldUpdate`) and a projection tactic (`qedsvm_discharge`) that,
*given a lift from decoded bytes to a field-list mutation*, closes the
contract equation. The v3.0 work is to build the pipeline that produces that
lift — decode the pinned ELF, run the handler under separation logic, reshape
the touched account into a field list — and to wire qedgen so the trust
surfaces consume it. The central simplification: the **bundled** callee
packages have a *fixed* hash, so they are proven **once** at package-build
time; only users' own programs need per-project discharge.

---

## §1 — The two trust surfaces today

### §1.1 Tier-1 CPI callee `ensures` axioms

`crates/qedgen/data/proofs/` ships two bundled proof packages:

- `spl/Token.lean` — `transfer` / `mint_to` / `burn`, 6 `ensures_axiom_*`
  total, plus `def binary_hash` (`:36-37`).
- `metaplex/Metadata.lean` — `sign_metadata` / `set_and_verify_collection` /
  `verify_collection`, 3 axioms, plus `def binary_hash` (`:26-27`).

Each contract is parametric over an opaque `State` and an accessor
(`Token.lean:54-56`):

```lean
axiom ensures_axiom_0 {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (from_balance : State → Nat) :
  (from_balance post) = (from_balance pre) - amount
```

A caller proof discharges its CPI obligation by *applying* this axiom.
`codegen/lean_gen_mir/cpi.rs` emits, per pinned call site, a per-ensures
theorem whose body is `exact <Iface>.<handler>.ensures_axiom_<i> <args>`
(with the caller's accessor `(·.field)` supplied for the abstract accessor);
`codegen/lean_sidecars.rs` copies the bundled package into the generated
project and wires the `require` / `import`. The package's only load-bearing
guarantee is the `binary_hash` content pin — `Token.lean:1-10` says so
plainly.

### §1.2 The sBPF refinement bridge

`lean_solana/QEDGen/Solana/Bridge.lean` is a `qedbridge <spec> where …` DSL
elaborator (active, built). It generates, per operation:

- `encodeState : State → Nat → Mem → Prop` and `decodeState` (memory ↔
  abstract `State`),
- a `.refines` theorem ("guards hold → exit 0 → final memory encodes the
  updated state") and a `.rejects` theorem ("guards fail → exit ≠ 0"), both
  over `executeFn progAt … FUEL`.

The `.refines` / `.rejects` **bodies are `sorry`**. The bridge scaffolding
exists; the proofs do not. Separately, `codegen/lean_gen_mir/sbpf.rs` emits
per-instruction guard theorems whose bodies are `wp_exec`/`sorry`
placeholders.

So: **two surfaces, same root cause** — we describe what the bytes do, but we
do not prove it from the bytes.

---

## §2 — What qedsvm v0.4.0 already provides (pinned `@ v0.4.0`)

From `lean_solana/lakefile.lean:16-17` (rev `8cf12c3`), the relevant pieces
already exist:

- **Field codec** (`SVM/SBPF/AccountCodec.lean`): `FieldVal`
  (`byte`/`u64`/`pubkey`/`blob`), `codecCoarse base : List (Nat × FieldVal) →
  Assertion`, and the keystone `account_agg` — *coarse* (spec-ready field
  atoms) `↔` *fine* (scattered asm-level atoms) generically over any layout,
  with no per-program axiom.
- **Refinement target** (`SVM/Solana/Abstract/Refinement.lean:150-158`):
  `AsmRefinesFieldUpdate (cr nSteps nCu entry exit rr base preFields
  postFields setupPre setupPost)` — a `cuTripleWithinMem` over the program
  region whose pre/post are `codecCoarse base {pre,post}Fields`. **Layout-
  general**: one predicate for any account shape. The legacy
  `AsmRefinesToken*` record-keyed forms are bridging input only.
- **Projection tactic** (`SVM/SBPF/Tactic/Discharge.lean:61-62`):
  ```lean
  macro "qedsvm_discharge" : tactic => `(tactic| simp [u64FieldAt])
  ```
  plus `u64FieldAt` + `u64FieldAt_found`. This closes the **accessor
  projection** — reading the mutated field out of the lift's `List (Nat ×
  FieldVal)` — *and only that step*.
- Execution + SL substrate: `executeFn` (fuel-bounded), `wp_exec`,
  `SVM.SBPF.{CPSSpec,SepLogic,Memory}`, `initState`.

### What v0.4.0 does NOT yet provide — the real gap

The discharge tactic's own header (`Discharge.lean:11-17`) is explicit: the
decode → `sl_block_auto` lift → `account_agg` reshape "is upstream and
produces an `AsmRefines…` obligation carrying the decoded field list." That
upstream lift is **not automated end to end yet**:

1. **Per-handler SL specs** for the bundled programs (SPL `transfer`, etc.) —
   the input the lift composes. Not shipped.
2. **The lift itself** ("qedlift's refinement codegen", referenced in
   `Refinement.lean:7`): ELF → per-instruction specs → `sl_block_auto` over
   the handler block → an `AsmRefinesFieldUpdate` instance. Scoped by
   **qedsvm#40** (whole-transition refinement: success + per-abort arms,
   multi-path SL composition, input-region layout lemmas, CPI-as-event-trace);
   discharge direction is **qedsvm#24**; **qedsvm#25** deletes the evolving
   `AsmRefinesToken*` records first.
3. **ELF-decode-at-proof-time** — a loader the tactic/lift can call to turn
   the cached bytes into an instruction list inside Lean.

**Consequence for planning:** qedgen *cannot* land discharge unilaterally.
`qedsvm_discharge` alone flips `axiom`→`theorem` only for the trivial
projection; the equation will not close without (1)–(3) upstream. The phasing
in §5 is gated on those.

---

## §3 — The seam: who owns what

Three packages, one discipline (consistent with `Refinement.lean:1-25` and
the spinout seam agreement):

| Layer | Owns | Examples |
|---|---|---|
| **qedsvm** (neutral) | sBPF semantics; the field codec; `AsmRefinesFieldUpdate`; `qedsvm_discharge`; the lift codegen; per-instruction SL specs; ELF decode | `FieldVal`, `account_agg`, `executeFn`, `sl_block_auto` |
| **lean_solana** (adapter) | tying qedsvm's neutral `List (Nat × FieldVal)` / accessors to `QEDGen.Solana.State`; the `qedbridge` DSL | `Bridge.lean`, `encodeState`/`decodeState` |
| **qedgen** (per-program / orchestration) | the `binary_hash` pin + lock; the bundled per-callee SL spec *references*; the ELF cache; the `axiom`→`theorem` emission; the cost gate; the trust-surface report | `data/proofs/`, `upstream_check.rs`, `lean_sidecars.rs` |

**Design rule (unchanged from the spinout):** qedsvm targets neutral
structures; qedgen emits per-program `FieldVal` layout + refinement
*statements*; lean_solana adapts to `QEDGen.Solana.State`. qedgen never
re-implements semantics — it *names the bytes* and *consumes the discharge*.

Open seam question (§10-Q2): do the **per-program** SL specs for well-known
programs (SPL/Metaplex) live in qedsvm (as "well-known program" modules — they
are not qedgen-specific) or in qedgen `data/proofs/`? Leaning qedsvm, with
qedgen referencing them; this keeps qedgen free of sBPF semantics.

---

## §4 — The discharge pipeline

For one Tier-1 callee ensures (`(acc post) = (acc pre) ± k`):

```
binary_hash  ──①──▶  ELF bytes (cache)  ──②──▶  decoded insns
                                                     │
                              per-handler SL spec ──③──▶ sl_block_auto
                                                     │
                                       SL post over byte regions
                                                     │
                                   account_agg ──④──▶ List (Nat × FieldVal)
                                                     │
              State := field-list, acc := u64FieldAt off ──⑤──▶
                                                     │
                                   qedsvm_discharge ──⑥──▶  (acc post) = (acc pre) ± k
```

Steps ①–② are qedgen + an ELF loader; ③–④ are qedsvm's lift (the #40 work);
⑤–⑥ are `qedsvm_discharge` today. The end state per axiom:

```lean
theorem ensures_axiom_0 … := by qedsvm_discharge "<binary_hash>" "transfer"
```

(The argument-carrying form of the tactic is itself a small upstream ask — the
v0.4.0 macro takes no args; it must learn to look up the ELF + SL spec by
`binary_hash`/handler.)

### The key simplification — bundled vs per-project discharge

Two modes, and they are *not* the same cost shape:

- **Bundled-package discharge (Tier-1 callees) — once, ahead of time.** The
  bundled `data/proofs/spl/Token.lean` is pinned to a *fixed* `binary_hash`.
  Its discharge is a **one-time artifact**, produced when we build the
  bundled package, not when a user builds their project. User projects keep
  doing exactly what they do today — `require` the package and `exact
  Token.transfer.ensures_axiom_0 …` — except the symbol they apply is now a
  `theorem`, not an `axiom`. *No change to generated user projects, no
  per-project cost.* This is Phase 1 and it is the cheapest, highest-trust-
  delta win.
- **Per-project discharge (user sBPF programs) — at verify time.** A user's
  own program has its own bytes; its `.refines`/`.rejects` (§1.2) must be
  discharged against *their* ELF, so the proof runs in *their* `lake build`
  under an opt-in `verify --discharge` gate (§8). This is Phase 2/3.

This split means Phase 1 ships value without ever putting SL composition on a
user's critical path.

---

## §5 — Phasing

**Phase 0 — qedsvm prerequisites (upstream, blocking).** At least one bundled
handler's full chain (③–④ for SPL `transfer`) works in qedsvm: a `transfer`
SL spec, the lift to `AsmRefinesFieldUpdate`, ELF-decode-at-proof-time, and a
`binary_hash`-keyed `qedsvm_discharge "<hash>" "<handler>"`. Tracked by
qedsvm#40/#24/#25. qedgen work below is scaffolding-only until this lands for
one handler.

**Phase 1 — Tier-1 CPI callee discharge (the headline).** Discharge the
bundled packages once:
- ELF-into-cache hook (§8).
- `data/proofs/spl/Token.lean`: flip `transfer`'s two `axiom`s to `theorem …
  := by qedsvm_discharge …`; keep `mint_to`/`burn` as axioms until their SL
  specs land (mixed package is honest and incremental).
- Then `mint_to`, `burn`, then Metaplex.
- Update the `#print axioms` trust-surface report (`verify --lean`) so
  discharged handlers drop off it — the *visible* proof that the surface
  shrank.
Exit criterion: a consumer proof over a verified `transfer` reports **no**
`Token.transfer.ensures_axiom_*` under `#print axioms`.

**Phase 2 — sBPF refinement bridge.** Replace `Bridge.lean`'s `.refines` /
`.rejects` `sorry` bodies with real `cuTripleWithinMem` + `AsmRefinesField
Update` proofs for the bundled sBPF examples (`examples/sbpf/*`), discharged
per-project under the opt-in gate. Retire the `executeFn`-only stub framing in
favor of the SL/lift path.

**Phase 3 — verified-callee composition + user programs.** Extend discharge
to user-authored callees and user sBPF programs; add **per-`binary_hash` proof
indexing** (SPL v4.0.3 vs v4.0.4 get separate proven packages); turn
`verified_with ["proptest"]` into a real `verified_with ["qedsvm@<commit>"]`
claim.

---

## §6 — qedgen-side work items

1. **ELF cache** — a content-addressed store under the qedgen cache dir
   (`QEDGEN_VALIDATION_WORKSPACE` sibling), keyed `binary_hash → ELF bytes`.
   `verify/upstream_check.rs` already dumps the bytes via `solana program
   dump` and hashes them (`:152-173`); today it discards them after the
   compare. Add a stash step so the bytes the tactic needs are present without
   a second fetch.
2. **Bundled per-callee SL spec wiring** — reference the qedsvm `transfer`/…
   SL spec from `data/proofs/spl/Token.lean` (exact mechanism depends on
   §10-Q2). Keep `binary_hash` the single pin that ties spec ↔ SL ↔ ELF.
3. **`axiom`→`theorem` emission** — in the bundled packages (Phase 1, hand-
   edited once per handler) and, for Phase 2/3, in `lean_sidecars.rs` /
   `Bridge.lean` codegen, emit the `theorem … := by qedsvm_discharge …` form
   when discharge is available + requested; fall back to `axiom`/`sorry`
   otherwise (graceful, mixed).
4. **Per-`binary_hash` proof indexing** — `data/proofs/spl/` grows a
   per-version layout so two pinned SPL versions don't collide.
5. **Cost gate** — `verify --discharge` (opt-in); default `verify
   --check-upstream` stays the cheap hash-pin path (§8).
6. **Trust-surface report** — extend the existing `#print axioms` scan in
   `verify --lean` to *expect* the discharged symbols to be gone, and to list
   what remains (the honesty boundary, §9).

---

## §7 — qedsvm-side prerequisites (must land first)

- Per-handler SL specs for SPL Token (`transfer`/`mint_to`/`burn`) and
  Metaplex handlers.
- The **lift** codegen: ELF + SL spec → `AsmRefinesFieldUpdate` instance
  (qedsvm#40 — success path first, then per-abort arms / multi-path /
  input-region layout / CPI-as-event-trace).
- ELF-decode-at-proof-time (a `Runner.runElf`-style loader callable from the
  tactic), and the **argument-carrying** `qedsvm_discharge "<hash>"
  "<handler>"` macro.
- qedsvm#25 (delete `AsmRefinesToken*` records) so new lifts target
  `AsmRefinesFieldUpdate` uniformly.

qedgen tracks these as a hard dependency; Phase 1 cannot exit until SPL
`transfer` is dischargeable upstream.

---

## §8 — Cost, caching, and the `--check-upstream` ELF hook

- **Default stays cheap.** `verify --check-upstream` keeps doing the
  hash-pin compare (seconds). It additionally *stashes* the dumped ELF in the
  cache (§6.1) so a later discharge needs no re-fetch.
- **Discharge is opt-in.** SL composition over a full handler is plausibly
  minutes; gate it behind `verify --discharge` (and never on a user's default
  `lake build`). For bundled callees (Phase 1) the cost is paid **once** at
  package-build time and shipped as `.olean`, so users never pay it.
- **Reproducibility.** Bundled-package discharge needs the pinned ELF at
  *package*-build time. Either vendor the bytes (large) or cache-with-fetch
  keyed by the checked-in `binary_hash` (preferred — same hash already in the
  `.qedspec` `upstream { }` block and the `.lean` `binary_hash`).

---

## §9 — What stays unverified even after discharge (honesty boundary)

Discharge proves the **bytes** honor the contract. It does **not** prove:

1. **Provenance** — that the pinned bytes *are* the published source. qedsvm
   verifies bytes, not where they came from. Closing this needs a source-tag
   clone + reproducible build + hash compare in `verify --check-upstream`
   (separate from discharge).
2. **Caller CPI builder ↔ interface** — that the caller's Rust actually
   invokes the callee with the accounts/args the interface declares. This is
   an audit-side lint, not a Lean obligation.
3. **The qedsvm TCB** — hand-written sBPF semantics, agave-pinned crypto
   crates, the ELF loader. Smaller and shared, but still trusted.

The `#print axioms` report (§6.6) must keep surfacing (1) and (3) honestly:
discharge moves a handler from "author-asserted axiom" to "proven modulo the
qedsvm TCB + the byte/source provenance gap" — a real and large trust delta,
but not "trustless."

---

## §10 — Open questions / risks

- **Q1 — discharge granularity.** Confirmed direction: bundled callees once
  at package-build time; user programs per-project under the gate. Risk: the
  bundled `.olean` must be rebuilt whenever the SPL `binary_hash` is bumped —
  acceptable (it is a release event), but the build must fail loudly if the
  cached ELF and the `binary_hash` disagree.
- **Q2 — where per-program SL specs live** (qedsvm vs qedgen `data/proofs/`).
  Leaning qedsvm (neutral, well-known programs). Decide with the qedsvm
  maintainer before Phase 1 wiring.
- **Q3 — ELF availability for bundled programs.** Cache-with-fetch vs vendor.
  Cache preferred; needs a deterministic fetch step in the package build.
- **Q4 — the three-way pin.** `binary_hash` appears in the `.qedspec`
  `upstream { }`, the bundled `.lean` `def binary_hash`, and (new) the cached
  ELF. The resolver already enforces spec↔lean equality at lock time; extend
  it to the cache key.
- **Risk — upstream timeline.** The entire thread is gated on qedsvm#40. If
  that slips, Phase 1 cannot ship; qedgen should not build speculative
  scaffolding far ahead of the first dischargeable handler.
- **Risk — abort arms.** The bundled ensures are success-path only. Real
  handlers abort; the lift's per-abort-arm story (qedsvm#40) must exist before
  we claim a handler is "verified" rather than "verified on the success path."

---

## §11 — Terminology note: the "Stance 3" collision

"Stance" is overloaded in the existing docs and must not be used unqualified
in this thread:

- `docs/design/spec-composition.md` uses **Stance 1/2/3** for *CPI
  composition strategy* — trust ensures / compose proofs / dynamic test-
  harness verification.
- The v2.28 axiom work + `project_stance3_qedsvm_discharge` use **Stance
  1/2/3** for the *axiom trust level* — author-asserted / theorem-wrapper /
  qedsvm-discharged.

These are different axes. This document avoids the label entirely and says
**"discharge"** for the byte-level proof. Recommend retiring "Stance 3" as a
project shorthand to prevent the two meanings from colliding in future notes.

---

## Appendix — primary sources (verify before acting; cited 2026-06-14)

- Axiom packages: `crates/qedgen/data/proofs/spl/Token.lean`,
  `…/metaplex/Metadata.lean`.
- CPI emission: `crates/qedgen/src/codegen/lean_gen_mir/cpi.rs`;
  `…/codegen/lean_sidecars.rs`.
- Bridge DSL: `lean_solana/QEDGen/Solana/Bridge.lean`.
- Upstream pin: `crates/qedgen/src/verify/upstream_check.rs`.
- qedsvm (v0.4.0, rev `8cf12c3`):
  `lean_solana/.lake/packages/qedsvm/SVM/SBPF/AccountCodec.lean`,
  `…/SVM/Solana/Abstract/Refinement.lean`,
  `…/SVM/SBPF/Tactic/Discharge.lean`.
- Prior framing: `docs/prds/RELEASE-v2.28.md`,
  `docs/design/spec-composition.md`.
