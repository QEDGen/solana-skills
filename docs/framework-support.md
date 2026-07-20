# Framework support matrix

What each pipeline surface supports per framework/target, verified against the
code gates (not aspirations). Update this table when a per-target gate changes;
each row names the module that owns the gate so claims stay checkable.

Five *distinct* framework notions exist — they are not one enum:

| Notion | Where | Values |
|---|---|---|
| Greenfield `Target` | `cli.rs` | anchor, quasar, pinocchio |
| Probe `--runtime` override | `cli.rs` (`RuntimeOverride`) | + native, sbpf |
| Brownfield audit `Runtime` | `probe/mod.rs` | + qedgen-codegen, unknown |
| Adapter `ProgramFramework` | `adapt/program_model.rs` | anchor, pinocchio, native |
| Ratchet `Framework` | `verify/ratchet.rs` | anchor, quasar |

sBPF assembly is selected by `pragma sbpf` in the spec, not by a `Target`.

## The matrix

✅ full · ⚠️ partial (noted) · ❌ none · n/a not meaningful

| Surface (owning module) | Anchor | Quasar | Pinocchio | Native | sBPF asm |
|---|---|---|---|---|---|
| IDL → spec scaffold (`spec/idl.rs` + `idl2spec`) — *deprecated* | ✅ pre-0.30 + 0.30 | ❌ | ✅ Codama IR (#197) | ❌ | ❌ |
| IDL → Tier-0 interface (`interface_gen`) | ✅ | ❌ | ✅ Codama IR (#197) | ❌ | ❌ |
| IDL → brownfield fuzz (`probe/crucible_brownfield`) | ✅ 0.30 | ✅ | ⚠️ needs on-disk Codama/0.30 IDL | ❌ deferred | ❌ parked |
| Brownfield adapt → spec skeleton (`adapt/`) — *deprecated* | ✅ args + accounts + errors | ❌ no adapter | ⚠️ handlers-only skeleton | ⚠️ loose (no conventions) | ❌ |
| Greenfield Rust scaffold (`codegen_mir`) | ✅ | ⚠️ generic CPI → `todo!()` | ⚠️ generic CPI → `todo!()`; imported mirrors error | n/a | n/a |
| Kani spec-model (`kani_mir`) | ✅ | ✅ | ✅ | n/a | skip by design |
| impl-Kani (`kani_impl`) | ✅ greenfield + state-struct (#162) + Context (#169) | ⚠️ greenfield shape only | ⚠️ own `#[repr(C)]` shape; some ix-data field types TODO | ❌ | ❌ |
| proptest (`proptest_gen_mir`) | ✅ | ✅ | ✅ | n/a | skip by design |
| Lean (`lean_gen_mir`) | ✅ | ✅ | ✅ | n/a | ✅ dedicated sBPF path |
| Probe: runtime-agnostic scanners (`run_helpers`) | ✅ (#196) | ✅ (#196) | ✅ | ✅ (#196) | ❌ bootstrap only |
| Probe: IDL-enrichment overlay (`probe/idl_overlay`) | ✅ enrich + narrow (#235); unbuilt → `derivable_idl` (#238) | ✅ enrich + narrow (#235); unbuilt → `derivable_idl` (#238) | ✅ enrich + handler fill | ⚠️ enrich only (declarative flags) | ❌ |
| Probe: runtime-specific findings (`probe/`) | ❌ agent-layer (SKILL.md) | ❌ agent-layer | ✅ richest (`pinocchio_probe`) | ⚠️ Shank dispatcher discovery only | ❌ |
| Miri divergence repros (`verify/miri_verify`) | ❌ | ❌ | ✅ | ❌ | n/a |
| Ratchet / readiness (`verify/ratchet`) | ✅ | ✅ | ❌ no ratchet crate | ❌ | ❌ |

## Codegen ownership contract: CPIs, PDA creation, and events

“Complete” means codegen emits the whole operation required by the spec. If
account resolution fails, a handler is unsupported, or signer seeds would have
to be guessed, the scaffold emits a reasoned agent-fill site and a `todo!()`.
It never emits a plausible unsigned CPI for a PDA authority.

| Operation | Anchor | Quasar | Pinocchio |
|---|---|---|---|
| Lifecycle-created state PDA (`Uninitialized`/`Empty` → active) | ✅ account macro owns `init`, payer, space, seeds, bump | ✅ account macro owns `init`, payer, seeds, bump | ⚠️ agent fill: complete signed System allocation/assignment |
| `transfers { ... }` sugar | ⚠️ agent fill: CPI accounts + authority | ⚠️ agent fill: CPI accounts + authority | ⚠️ agent fill: CPI accounts + authority |
| Direct canonical SPL Token `call`, transaction signer authority | ✅ transfer, mint, burn, initialize, close | ⚠️ transfer, mint, burn, close; initialize is agent fill | ✅ transfer, mint, burn, initialize, close |
| Direct System transfer, transaction signer authority | ✅ | ✅ | ✅ |
| Direct System create/assign, non-PDA signer | ✅ generic invocation | ⚠️ agent fill | ⚠️ agent fill |
| Generic interface `call`, transaction signer authority | ✅ discriminator + args + account metas | ⚠️ agent fill | ⚠️ agent fill |
| Any direct `call` whose signer slot binds a program PDA | ✅ builder shapes (SPL Token, System transfer) sign via `new_with_signer` when seeds are the account's declared `pda [...]`; ⚠️ agent fill otherwise (generic invoke, unassemblable seeds) | ⚠️ agent fill: complete CPI with signer seeds | ⚠️ agent fill: complete CPI with signer seeds |
| Events | ⚠️ agent fill: payload binding + framework emission | ⚠️ agent fill: payload binding + framework emission | ⚠️ agent fill: payload binding + framework emission |

The executable boundary lives in `codegen_shared::cpi::CpiPlan`:
`Complete(code)` and `AgentFill(reason)` are the only outcomes. The PDA-signer
check is a post-condition on the emitted artifact: a call whose signer slots
bind caller PDAs is `Complete` only if the emitted code signs for them
(`new_with_signer` / `invoke_signed`), so an ordinary unsigned `invoke` can
never ship for a PDA-authorized call. Pinocchio likewise turns lifecycle PDA
creation, transfer sugar, events, and unsupported calls into a hard handler
`todo!()` rather than returning `Ok(())` after a breadcrumb.

The two *deprecated* rows (`qedgen spec --idl`, `qedgen adapt --program`)
remain functional in v2.x with a runtime warning and are removed in v3.0.
The brownfield front door is spec elicitation: `qedgen probe --program <c>
--emit-spec-candidates --audit-dir .qed/audit/<ts>` (writes the same spec
skeleton as a byproduct plus `hypotheses.json`) → decisions recorded to
`<audit-dir>/answers.json` → `qedgen ratify --audit-dir <dir>`. The IDL now
enters as a probe evidence source (signer flags, `has_one` relations, status
enums) via the IDL-enrichment overlay row.

## Reading the Pinocchio column

Pinocchio is a first-class *audit* target (richest probe path, Miri repros,
Codama-gated fuzz) and a full *greenfield* target, and — since #197 — its
Codama IDL enters the same front doors as Anchor's (`qedgen interface --idl`,
the probe IDL-enrichment overlay, and the deprecated `qedgen spec --idl`).
Remaining real gaps:

- **Brownfield spec depth** — the deprecated `pinocchio_to_spec` skeleton
  infers handlers only; probe elicitation (probe → answers → ratify) is the
  current path, and the IDL overlay is the richer evidence source when an
  IDL exists.
- **Generic CPI mechanization** in the greenfield scaffold (SPL/System are
  mechanized; anything else is a `todo!()` breadcrumb).
- **impl-Kani ix-data field types** — the `#[repr(C)]` profile covers the
  common numeric shapes; exotic field types leave bytes symbolic with a TODO.
- **No ratchet** — mainnet-readiness gating is Anchor/Quasar only.

## Reading the Quasar column

Quasar is greenfield + ratchet + fuzz. It never had a (now-deprecated)
brownfield adapter and has only the greenfield impl-Kani shape — a
pre-existing Quasar program is audited and spec-elicited through the probe
(agnostic scanners + IDL overlay + probe → answers → ratify) but not
state-struct harnessed.

## sBPF assembly

Verified through the Lean path exclusively (`asm2lean`, `qedsvm`); every
Rust-shaped artifact (Kani, proptest, Crucible, scaffold) is skipped by
design — generated Rust harnesses are meaningless for assembly
(`feedback_sbpf_no_kani_proptest`). Client-side tests own runtime checks.
