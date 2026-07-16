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
| IDL → spec scaffold (`spec/idl.rs` + `idl2spec`) | ✅ pre-0.30 + 0.30 | ❌ | ✅ Codama IR (#197) | ❌ | ❌ |
| IDL → Tier-0 interface (`interface_gen`) | ✅ | ❌ | ✅ Codama IR (#197) | ❌ | ❌ |
| IDL → brownfield fuzz (`probe/crucible_brownfield`) | ✅ 0.30 | ✅ | ⚠️ needs on-disk Codama/0.30 IDL | ❌ deferred | ❌ parked |
| Brownfield adapt → spec skeleton (`adapt/`) | ✅ args + accounts + errors | ❌ no adapter | ⚠️ handlers-only skeleton | ⚠️ loose (no conventions) | ❌ |
| Greenfield Rust scaffold (`codegen_mir`) | ✅ | ⚠️ generic CPI → `todo!()` | ⚠️ generic CPI → `todo!()`; imported mirrors error | n/a | n/a |
| Kani spec-model (`kani_mir`) | ✅ | ✅ | ✅ | n/a | skip by design |
| impl-Kani (`kani_impl`) | ✅ greenfield + state-struct (#162) + Context (#169) | ⚠️ greenfield shape only | ⚠️ own `#[repr(C)]` shape; some ix-data field types TODO | ❌ | ❌ |
| proptest (`proptest_gen_mir`) | ✅ | ✅ | ✅ | n/a | skip by design |
| Lean (`lean_gen_mir`) | ✅ | ✅ | ✅ | n/a | ✅ dedicated sBPF path |
| Probe: runtime-agnostic scanners (`run_helpers`) | ✅ (#196) | ✅ (#196) | ✅ | ✅ (#196) | ❌ bootstrap only |
| Probe: IDL-enrichment overlay (`probe/idl_overlay`) | ✅ enrich + narrow (#235) | ✅ enrich + narrow (#235) | ✅ enrich + handler fill | ⚠️ enrich only (declarative flags) | ❌ |
| Probe: runtime-specific findings (`probe/`) | ❌ agent-layer (SKILL.md) | ❌ agent-layer | ✅ richest (`pinocchio_probe`) | ⚠️ Shank dispatcher discovery only | ❌ |
| Miri divergence repros (`verify/miri_verify`) | ❌ | ❌ | ✅ | ❌ | n/a |
| Ratchet / readiness (`verify/ratchet`) | ✅ | ✅ | ❌ no ratchet crate | ❌ | ❌ |

## Reading the Pinocchio column

Pinocchio is a first-class *audit* target (richest probe path, Miri repros,
Codama-gated fuzz) and a full *greenfield* target, and — since #197 — its
Codama IDL enters the same front door as Anchor's (`qedgen spec --idl`,
`qedgen interface --idl`). Remaining real gaps:

- **Brownfield spec depth** — `pinocchio_to_spec` infers handlers only;
  account lists / param types / error enums are agent-completed (the Codama
  path is the richer alternative when an IDL exists).
- **Generic CPI mechanization** in the greenfield scaffold (SPL/System are
  mechanized; anything else is a `todo!()` breadcrumb).
- **impl-Kani ix-data field types** — the `#[repr(C)]` profile covers the
  common numeric shapes; exotic field types leave bytes symbolic with a TODO.
- **No ratchet** — mainnet-readiness gating is Anchor/Quasar only.

## Reading the Quasar column

Quasar is greenfield + ratchet + fuzz. It has **no brownfield adapter** and
only the greenfield impl-Kani shape — a pre-existing Quasar program can be
audited (probe/agnostic scanners) but not spec-skeletoned or state-struct
harnessed.

## sBPF assembly

Verified through the Lean path exclusively (`asm2lean`, `qedsvm`); every
Rust-shaped artifact (Kani, proptest, Crucible, scaffold) is skipped by
design — generated Rust harnesses are meaningless for assembly
(`feedback_sbpf_no_kani_proptest`). Client-side tests own runtime checks.
