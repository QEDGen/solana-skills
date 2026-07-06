# qedgen MIR — design sketch

> **Historical design doc (the v2.29–v2.30 MIR migration).** The `QEDGEN_LEGACY_*` escape hatches and parallel legacy renderers described below existed only during the migration soak; **v2.32 deleted them** — the four MIR codegens are now the sole path. For current behavior see [`references/cli.md`](../../references/cli.md). Kept for the migration rationale.

**Status:** Phase 1 + 2 (Lean), Phase 3 (Kani), Phase 4 (Anchor/Quasar), Phase 5 (proptest) all complete. **All four primary codegens are MIR-default and snapshot-gated.** Escape hatches `QEDGEN_LEGACY_{LEAN,KANI,CODEGEN,PROPTEST}=1` existed through the soak and were removed in v2.32. Snapshot suites: `mir_snapshot` (Lean, 6 pilots), `kani_snapshot` (6), `codegen_snapshot` (6, multi-file dump), `proptest_snapshot` (6). Two known carve-outs: `generate_guards` (636L, Anchor) and the full proptest emit body stay delegated to legacy pending the typed-Stmt MIR refactor (v3.0); the dispatch surfaces are MIR-default so future cleanups land without touching callers.

**Last revised:** 2026-05-25 (Phase 5 proptest scaffold + snapshot + dispatch flip — MIR carry-through complete across all four primary codegens).

**Companion docs** (read these first if you want measured evidence behind the claims here):

- [`codegen-baseline.md`](codegen-baseline.md) — LoC + case-count snapshot of the four primary emit modules.
- [`codegen-divergence.md`](codegen-divergence.md) — enumerated cross-codegen divergence classes with measured evidence.
- [`intrinsic-fixture-map.md`](intrinsic-fixture-map.md) — every spec feature × every fixture, mapped to candidate MIR shapes.
- [`cross-cutting-transforms.md`](cross-cutting-transforms.md) — which existing modules are genuine MIR→MIR pass candidates vs. already-shared vs. out-of-scope.

## Motivation

qedgen has four primary codegens (Lean, Anchor/Quasar via `codegen.rs::Target`, Kani classic + impl-targeted, proptest) plus two test emitters. Each lives in its own module and emits directly from `ParsedSpec`. Cross-cutting concerns — lifecycle gating, effect-op dispatch, abort semantics, account-block lowering, CPI substitution — are re-implemented per codegen. This is the source of most qedgen bugs: a new spec feature is N+ edits across emit modules, and missing one yields silent divergence rather than a build failure.

The divergence inventory shows the concrete pattern: `ParsedEffectBranches` (issue #42's conditional-effects shape) is consumed only by `lean_gen.rs` — Anchor, Kani, proptest have **zero** references. The same shape recurs for variant promotion (codegen.rs + lean_gen.rs only), CPI ensures substitution (Lean + Kani only), and others.

**MIR's goal: reduce codegen bugs.** Make cross-codegen divergence structurally impossible by replacing the shared-by-convention `ParsedSpec` dispatch with a typed `Stmt` IR every codegen has to `match` exhaustively. Per [[feedback-mir-is-bug-reduction]], LoC reduction is a side-effect; the metric is bug-class elimination.

## Position in the pipeline

Today:

```
.qedspec --> chumsky_parser --> ParsedSpec --> lean_gen.rs    --> Spec.lean
                                            \-> codegen.rs    --> programs/.../src/lib.rs (Anchor + Quasar via Target)
                                            \-> kani.rs       --> kani/harnesses.rs
                                            \-> kani_impl.rs  --> kani/impl_harnesses.rs
                                            \-> proptest_gen  --> tests/properties.rs
```

Cross-cutting transforms (`cpi_substitute.rs`, parts of `check.rs`) operate at the `ParsedSpec` level and feed each codegen, which then re-dispatches independently.

Proposed:

```
.qedspec --> chumsky_parser --> ParsedSpec --> [lower] --> MIR --> lean_codegen     --> Spec.lean
                                                              \-> anchor_codegen    --> programs/.../src/lib.rs
                                                              \-> kani_codegen      --> kani/harnesses.rs
                                                              \-> proptest_codegen  --> tests/properties.rs
```

Codegens consume MIR; none parse `.qedspec` or touch the chumsky AST. MIR→MIR passes (see `cross-cutting-transforms.md`) run between lowering and codegen.

## Key design constraints

### Expressions are opaque strings

This is the single most important constraint. The parser already lowers expressions per target (`ParsedRequires` carries `lean_expr`, `rust_expr`, `rust_expr_pod`, `rust_expr_binary` — four pre-rendered string forms per clause). Re-modelling expressions as a typed tree inside MIR would either (a) re-parse the pre-rendered strings (wrong direction) or (b) reach back to `crate::ast::Node<crate::ast::Expr>`, which only `ParsedRequires.ast_body` preserves (`ParsedEnsures` and `ParsedAbort` don't carry it).

So MIR is **structurally typed at the statement level, opaque at the expression level**:

```rust
pub struct Expr {
    pub lean: String,
    pub rust: String,
    pub rust_pod: String,
    pub rust_binary: String,
    pub source_span: Option<Span>,
}

pub struct Predicate(pub Expr);
```

Each codegen picks the field it needs (`expr.lean` / `expr.rust` / `expr.rust_binary`). The MIR's value comes from desugaring **structure and dispatch**, not from re-modelling expressions.

This caveat is load-bearing for the divergence inventory: classes A1, A2, A3, A4, B1 (per `codegen-divergence.md`) close under structural typing of statements. Classes A5 (quantifier rendering) and C3 (operator-precedence in concatenation) **persist** — they're expression-rendering issues opaque strings don't address. Mitigation for C3 is defensive parens at lowering time, which is coding discipline, not IR.

### qedgen-local scope; no qedsvm coupling

Per [[feedback-mir-is-bug-reduction]] and the conversation that produced this sketch:

- The `runMir` Lean-side operational semantics is **parked**. It was the Lean object every codegen would target; without it, codegens interpret MIR independently and lifestyle predicates live in qedsvm's `Svm/Solana/` library. Adding `runMir` later is purely additive.
- No `applyOp ≡ runMir` equivalence lemma. No `encodeState` / `decodeState` schema. No cross-repo migration of MIR-adjacent Lean primitives into qedsvm.
- ~~qedsvm stays vendored at `lean_solana/QEDGen/Solana/SBPF/` until qedsvm tags a stable release.~~ Done: qedsvm v0.3.0 tagged a stable surface and the vendored tree was deleted — `lean_solana` now does `require qedsvm` and everything imports `SVM.SBPF.*` (solana-skills#86).
- qedbridge codegen (Phase 5 in the original proposal) is also deferred until qedsvm stabilizes. The MIR's design doesn't depend on it.

## Shape

```rust
pub struct Mir {
    pub name: Symbol,
    pub state: StateAdt,           // variants + fields, with field types
    pub accounts: AccountTable,    // PDAs, owners, writability, init, authority, token-type
    pub errors: ErrorEnum,
    pub interfaces: InterfaceRegistry,  // imported namespaces / interface stubs
    pub handlers: Vec<HandlerMir>,
    pub invariants: Vec<InvariantMir>,
}

pub struct StateAdt {
    pub variants: Vec<StateVariant>,
}

pub struct StateVariant {
    pub tag: VariantTag,
    pub fields: Vec<(Symbol, Ty)>,
}

pub struct HandlerMir {
    pub name: Symbol,
    pub discriminant: Option<Bytes>,
    pub params: Vec<(Symbol, Ty)>,
    pub accounts: Vec<AccountBinding>,
    pub auth: Option<AccountOrField>,             // signer requirement (from `auth` clause)
    pub transition: Option<(VariantTag, VariantTag)>,  // lifecycle arrow
    pub pre: Vec<Predicate>,                       // requires clauses (already with schema-includes expanded)
    pub body: Block,
    pub post: Vec<Predicate>,                      // ensures clauses
    pub emits: Vec<EventRef>,                      // auxiliary; out of body
}

pub struct Block(pub Vec<Stmt>);

pub enum Stmt {
    // ---- Solana intrinsics validated by ≥5 fixtures ----

    /// Authorization-or-abort. Canonical `requires X else Err` shape; 96 uses
    /// across 15 of 21 main fixtures. Closes divergence class A3.
    RequireOrAbort { pred: Predicate, err: ErrorRef },

    /// SPL Token Transfer. 7 fixtures (5 via `call Token.transfer`, 2 via
    /// `transfers {}` block). Closes divergence classes A2 (Kani/proptest
    /// gap) and A4 (CPI ensures coordination).
    TokenTransfer {
        from: AccountRef,
        to: AccountRef,
        amount: Expr,
        authority: AccountRef,
    },

    /// Lifecycle promotion to a new variant, carrying payload. Only 1 main
    /// fixture but several regression fixtures; closes class A2 for the
    /// Kani/proptest variant-promotion gap.
    VariantPromote {
        from_tag: VariantTag,
        to_tag: VariantTag,
        payload: Vec<(Symbol, Expr)>,
    },

    // ---- Effect-op kinds (validated by 10 fixtures + per-effect-error v2.24 work) ----

    /// `field := value`. Escape hatch for arbitrary effect RHS.
    Assign { path: Path, rhs: Expr },

    /// `field +=` with checked overflow → ErrorRef. Default arithmetic shape
    /// post-v2.24. Closes class B1.
    CheckedAdd { path: Path, delta: Expr, err: ErrorRef },
    CheckedSub { path: Path, delta: Expr, err: ErrorRef },

    /// `field +=!` — wrapping arithmetic, no error. v2.24 marker.
    WrapAdd { path: Path, delta: Expr },
    WrapSub { path: Path, delta: Expr },

    /// `field +=?` — saturating arithmetic, no error. v2.24 marker.
    SatAdd { path: Path, delta: Expr },
    SatSub { path: Path, delta: Expr },

    // ---- Control flow (closes class A1: ParsedEffectBranches divergence) ----

    /// Conditional effect block (issue #42). Currently only Lean codegen
    /// handles it; MIR makes it first-class.
    Branch {
        scrutinee: Predicate,  // or `match` on a value — see open Qs
        then_block: Block,
        else_block: Option<Block>,
    },

    /// Terminal abort. Used as the canonical post-`Branch` exit for fail paths.
    Abort(ErrorRef),

    // ---- Generic escape hatches ----

    /// Generic CPI to a non-Token interface. One occurrence in fixtures
    /// (`call Target.h` in a regression). Reserved for forward compatibility.
    Cpi { target: InterfaceRef, method: MethodRef, args: Vec<Expr> },

    /// Local binding inside a handler body. Rare in current fixtures but
    /// needed if any future surface adds explicit `let`.
    Let { name: Symbol, ty: Ty, value: Expr },
}

// Opaque expression carrier — no internal structure.
pub struct Expr {
    pub lean: String,
    pub rust: String,
    pub rust_pod: String,
    pub rust_binary: String,
    pub source_span: Option<Span>,
}

pub struct Predicate(pub Expr);

pub enum Ty { U8, U16, U32, U64, U128, I64, I128, Bool, Pubkey, Custom(Symbol) }

pub struct AccountTable {
    pub pdas: Vec<PdaDeclaration>,        // top-level `pda <name> [seeds]`
    pub bindings: BTreeMap<Symbol, AccountBindingShape>,
}

pub struct AccountBindingShape {
    pub writable: bool,
    pub readonly: bool,                    // redundant with !writable but stored for clarity
    pub init: bool,
    pub kind: AccountKind,                 // Signer / Token / Mint / Program / Pda / Plain
    pub authority: Option<AccountRef>,
    pub pda_ref: Option<PdaRef>,
}

pub struct PdaDeclaration {
    pub name: Symbol,
    pub seeds: Vec<Expr>,
}

pub enum AccountKind { Signer, Token, Mint, Program, Pda, Plain }

pub enum AccountRef {
    ByBinding(Symbol),                     // refers to an entry in AccountTable.bindings
    BySelf,                                 // 'self' / the handler's primary state
}

pub enum AccountOrField {
    Account(AccountRef),
    AccountField { account: AccountRef, field: Symbol },  // dotted-auth v2.29.1
}
```

Total statement kinds: **11** (4 primary intrinsics + 7 effect/control variants + escape hatches). Half the original proposal's 20-node list. Everything dropped (SystemTransfer / AccountInit / AccountClose / TokenMint / TokenBurn / TokenApprove / SysvarRead / DiscriminantMatch / SignerCheck-as-Stmt / LamportAssert / TokenExt / ProgramSpecific) lacks fixture evidence — see `intrinsic-fixture-map.md`.

## Design rules

1. **A `Stmt` kind earns inclusion by eliminating a divergence class.** Not by reaching a codegen-count quorum. Each kind above is traceable to a class in `codegen-divergence.md` or a fixture cluster in `intrinsic-fixture-map.md`. If a new spec feature can be lowered into existing `Stmt` kinds without re-introducing a divergence class, don't add a new kind.

2. **MIR is desugared, not optimized.** Surface sugar (`+=!`, `else Err`, schema-includes, dotted-auth, `transfers {…}` blocks) lowers to explicit primitive nodes during parser→MIR. Optimizations (const fold, dead-handler elimination) are not in scope for the initial port; revisit later if measurement shows them worthwhile.

3. **MIR is structurally typed at the statement level, opaque at the expression level.** Statement kinds are checked exhaustively; expressions ride as pre-rendered target strings. See "Expressions are opaque strings" above.

4. **MIR is finite and small.** Target: ~11 statement kinds, ~10 type constructors. If the IR grows beyond ~15 `Stmt` kinds, either the surface DSL is over-extended or the intrinsic set drifted from fixture evidence — investigate before adding.

5. **No control flow beyond `Branch` and `Abort`.** No loops, no early return, no exceptions. Solana handlers don't need them.

6. **AccountTable is foundational.** Account-block features (writable / init / authority / type / pda) account for the largest cross-codegen LoC weight (114 + 100 + 74 + 24 + 27 = 339 fixture references). Designing `AccountTable` is Phase 0 work, not deferred to Phase 3.

## Non-goals

- **Not a verification IR.** Proofs are stated against `Svm/Solana/` predicates in qedsvm, not against MIR. MIR is what spec semantics are *expressed* in, not what theorems are stated against.
- **Not a bytecode IR.** That's qedsvm's `Insn` + `CodeReq`.
- **Not an MLIR-style dialect framework.** One IR, no dialect registry, no extensibility hooks.
- **Not a Rust HIR-style typed AST.** The surface AST stays in `chumsky_parser.rs`; MIR is desugared and target-neutral.
- **Not a refinement-theorem emitter.** The original proposal positioned qedbridge codegen as the motivating consumer that justifies MIR's structural typing. That consumer is parked until qedsvm stabilizes; MIR's value case rests on bug-reduction across the four existing codegens.

## Cross-cutting passes (MIR → MIR)

Per `cross-cutting-transforms.md`, five genuine MIR→MIR pass candidates:

| Pass | LoC est. | What it does |
| --- | ---: | --- |
| `cpi_substitute` | ~150 (lift+adapt) | Precompute substituted callee ensures at MIR construction time; expose as `Stmt::TokenTransfer.callee_ensures: Vec<Predicate>` / `Stmt::Cpi.callee_ensures`. Closes A4. |
| `lifecycle_lower` | ~? | Synthesize an entry `Stmt::RequireOrAbort` checking `s.tag == from_tag` from the handler's `transition` field. Closes B1 (was 60–76 refs per codegen). |
| `variant_promote_check` | ~? | Validate that `Stmt::VariantPromote.payload` covers all fields of `to_tag`. Closes A2. |
| `effect_op_validate` | ~? | Ensure `Stmt::CheckedAdd` / `WrapAdd` / `SatAdd` carry the right error refs and target paths exist. Closes B3-shape correctness. |
| `account_consistency` | ~? | Validate that account references in statements match declared `AccountTable` entries. |

These run between parser→MIR lowering and codegen. They're **mandatory** for correctness, not optional optimizations. Optional passes (const_fold, dead_handler) are deferred.

## Migration path

Realistic per the `cross-cutting-transforms.md` analysis. The original proposal's 5–7 week estimate underestimated by ~2×; honest scope is **8–13 weeks** for the qedgen-local port.

| Phase | Scope | Estimate |
| --- | --- | --- |
| 0 | Define MIR types (Stmt + AccountTable + HandlerMir + Mir) + `lower(parsed: &ParsedSpec) -> Mir` function for the pilot scope. Validate against the TokenTransfer-using fixtures. **AccountTable is the major design artifact here** — it's foundational and has the most cross-codegen surface. | ~2 wks |
| 1 | Port Lean codegen for the pilot scope (TokenTransfer + RequireOrAbort + Assign + CheckedAdd/Sub + lifecycle gating via `HandlerMir.transition`). Keep `lean_gen.rs` as a fallback behind a flag. Acceptance: every TokenTransfer-using fixture produces byte-identical or cosmetic-diff-only Lean output. | ~2 wks |
| 2 | Port Anchor codegen (`codegen.rs::Target::Anchor` + `Target::Quasar`) for pilot scope. Second target validates the abstraction. Acceptance: every fixture's Anchor output is byte-identical or cosmetic-diff-only. | ~2 wks |
| 3 | Move MIR→MIR passes: `cpi_substitute`, `lifecycle_lower`, `variant_promote_check`, `effect_op_validate`, `account_consistency`. Reuse between Lean and Anchor validates the pass infrastructure. | ~1 wk |
| 4 | Port Kani (classic + impl) and proptest. proptest gains the `cpi_substitute` output for free, closing divergence A4 by construction. | ~2 wks |
| 5 | Add `Stmt::Branch` + `Stmt::VariantPromote` + `Stmt::WrapAdd` etc. to all codegens. The ones not exercised by the pilot land here. Bug-bundle replay tests (#39/#40/#41/#43 + the ParsedEffectBranches gap) become acceptance gates. | ~1–2 wks |
| 6 | Remove `ParsedSpec`-era fallback paths from all codegens. Delete dead code as cleanup. | ~3 days |

Total: ~10–13 wks of qedgen-local work. No qedsvm coupling, no qedbridge codegen — those come back when qedsvm tags stable.

## Risks

- **Over-abstraction.** Mitigated by the bug-reduction framing — every node kind traces to a divergence class. If we add a kind that doesn't close a class, drop it.
- **Lowering loses information.** Mitigated by keeping `ParsedSpec → Mir` lossless w.r.t. semantics. Source spans flow through opaquely on `Expr.source_span`.
- **Phase 3 underestimated.** The original proposal's 3–5-day estimate was off by ~5×. Real cross-cutting-transform work is ~12 days of port + ~12 days of codegen-side `match Stmt` rewrites. This sketch budgets Phase 3 at 1 wk plus the codegen-side work absorbed into Phases 1–2.
- **AccountTable design is the riskiest single artifact.** It carries the most cross-codegen surface (339 fixture references). Wrong shape forces revision across all four codegens. Mitigation: prototype against Anchor's `#[derive(Accounts)]` emission first (codegen.rs has 15 variant-promotion refs + 100+ account-attr refs to use as the validation surface).
- **Expression-rendering bugs (class C3) are not closed by MIR.** Opaque strings preserve operator-precedence concatenation hazards. Mitigation: at MIR construction time, wrap every `Expr.rust`/`Expr.rust_binary` in defensive outer parens before storage. Coding discipline at the lowering layer, not the IR layer.

## Open questions

1. **`Stmt::Branch` scrutinee shape.** Today's `ParsedEffectBranches` carries a `match`-on-value scrutinee. MIR's `Branch.scrutinee: Predicate` only models boolean tests; a `Stmt::Match { scrutinee: Expr, arms: Vec<(Pattern, Block)> }` may be needed for the issue #42 corpus. Resolve in Phase 0 against `crates/qedgen/tests/fixtures/regressions/issue-42-conditional/fee_router.qedspec`.

2. **`InterfaceRegistry` shape.** Unified imports (v2.29 Slice F–I) populate `ParsedSpec.imported_namespaces`. MIR's `Mir.interfaces` either mirrors this directly or holds a different shape optimized for `Stmt::Cpi` callee-ensures lookup. Decide once `cpi_substitute` is ported (Phase 3).

3. **`Predicate` normalization.** Today each clause stores 4 rendered forms (`lean_expr`, `rust_expr`, `rust_expr_pod`, `rust_expr_binary`). MIR mirrors this. Should we add a `kani_expr` field for Kani-specific lowering, or keep `rust_expr_binary` as Kani's canonical input? Probably the latter — adding a fifth render field requires a parser change, which v2.30 should avoid.

4. **Source-location threading.** Every node carries an `Option<Span>` opaquely. Spans flow from chumsky's positions; renderers can ignore them. No further design needed in Phase 0 — confirm in implementation.

5. **`Mir.invariants` shape.** Issue #67's `rule` vs `invariant` distinction is parallel work. Until #67 lands, treat `invariants` as a `Vec<Predicate>` over `(pre, post): (&State, &State)`. Re-shape when #67's parser changes land.

## Implementation status (mir branch)

Tracking what's shipped on the `mir` branch vs. what's still planned. Commits referenced are short SHAs on `mir`.

### Phase 0 — typed IR + lowering — **shipped** (`ab4bdbe`)

- `crates/qedgen/src/mir.rs` (~870 LoC) — full type definitions per the §"Shape" above: `Mir`, `HandlerMir`, `Stmt` (12 kinds), `Expr` (opaque-string carrier), `AccountTable`, `AccountBindingShape`, plus references / types / errors / events / interfaces.
- `mir::lower(parsed: &ParsedSpec) -> Mir` for the pilot scope: handler bodies lowered through `RequireOrAbort`, `TokenTransfer`, `Assign`, `CheckedAdd/Sub`, `WrapAdd/Sub`, `SatAdd/Sub`, `Cpi`, `Emit`, `Abort`. `Branch` and `VariantPromote` recognize their source shape but emit stubs (Phase 5).
- `HandlerMir.transition` lifecycle threaded from pre/post-status.
- `AccountTable` populated from top-level `pda` declarations + per-handler account bindings.
- 10 lowering tests including 5 fixture-driven runs (escrow, escrow-split [multi-file], lending, multisig, bundled-stdlib-demo).

### Phase 1b — lean_gen_mir scaffold + flag — **shipped** (`f670404`)

- `crates/qedgen/src/lean_gen_mir.rs` mirrors `lean_gen::{generate, render}` entry-point shape.
- `QEDGEN_USE_MIR=1` env var (Phase 1b–Phase 2) routed `qedgen codegen --lean` through the new path; post Phase 2 MIR is the default and `QEDGEN_LEGACY_LEAN=1` is the escape hatch back to `lean_gen`.
- Shape-detection dispatch (sBPF / indexed / multi-account / single-account) matches legacy; non-pilot branches emit marker stubs.

### Phase 1c — Lean emission for pilot scope — **closed for this session (14/16 + adjacents)**

Sub-slices shipped:

| § | Emitter | Slice | Status |
|---|---|---|---|
| 1 | `emit_header` (imports) | 1b | ✅ |
| 2 | `emit_namespace_open/close` | 1b | ✅ |
| 3 | `emit_uninterpreted_helpers` + `emit_ref_impls` | 1c-4 | ✅ |
| 4 | `emit_constants` | 1c-4 | ✅ |
| 5 | `emit_lifecycle_marker` (Status inductive) | 1b | ✅ |
| 6 | `emit_state_struct` (cross-variant union) | 1c-1 | ✅ |
| 7 | `emit_transitions` (per-handler) | 1c-1 | ✅ |
| 8 | CPI theorems | — | **deferred** (needs `Mir.interfaces` populated) |
| 9 | `emit_invariants` | 1c-3 | ✅ |
| 10 | `emit_operation_inductive` + applyOp | 1c-1 | ✅ |
| 11 | `emit_properties` + preservation | 1c-5 | ✅ |
| 12 | `emit_aborts_if` (legacy + requires-else) | 1c-2 | ✅ |
| 13 | `emit_ensures` | 1c-2 | ✅ |
| 14 | `emit_frame_conditions` | 1c-3 | ✅ |
| 15 | `emit_covers` + `emit_liveness` + `emit_environments` + `emit_overflow` | 1c-6 | ✅ (statements only — proof scripts deferred) |
| 16 | namespace close | 1b | ✅ |

**15 of 16 sections emit content.** End-to-end smoke-confirmed on `examples/rust/{escrow,lending,multisig,bundled-stdlib-demo}/*.qedspec` plus the since-removed perp-dex risk-engine example with `QEDGEN_USE_MIR=1`. 15 lean_gen_mir tests + 10 mir tests pass. Full bin suite at 970 passing.

Commit trail on `mir` branch:
- `ab4bdbe` Phase 0 — typed IR + lowering
- `f670404` Phase 1b — scaffold + `QEDGEN_USE_MIR=1` flag
- `60a8a38` Phase 1c-1 — transitions + Operation + applyOp
- `b9be609` Phase 1c-2 — aborts + ensures
- `01578c6` Phase 1c-3 — invariants + frame
- `c403089` docs — sketch progress catchup
- `42bdb06` Phase 1c-4 — constants + helpers + ref_impls
- `040a8b4` Phase 1c-5 — properties + preservation
- (next commit) Phase 1c-6 — cover / liveness / environments / overflow

### Deferred — return in a dedicated Phase 1d session

- **§8 CPI theorems** — `render_cpi_theorems` in legacy lean_gen.rs:`grep -n "^fn render_cpi_theorems"`. Requires populating `Mir.interfaces` from `ParsedSpec.imported_namespaces` + the bundled stdlib registry (SPL Token / System Program / Metaplex). Intersects with Phase 3's `cpi_substitute` MIR→MIR pass. ~1–2 days.
- **§15 proof scripts** — Phase 1c-6 emits cover / liveness / overflow theorems with `:= sorry` / `:= by sorry` bodies. Legacy lean_gen.rs has three auto-proof helpers — `cover_trace_proof` (witness construction over state-field defaults), `liveness_proof_script` (lifecycle-graph walk via `find_liveness_path` + `subst h_apply; rfl`), `overflow_proof_script` (`unfold + split + omega`) — that close many trivial cases. Environment theorems already auto-discharge via `unfold + dsimp + exact h_inv` when mutated fields don't appear in the property body. ~half to one day total when needed.
- **Multi-variant ADT path (`render_single_account_adt`)** — currently lean_gen.rs takes this branch for `escrow` (Uninitialized | Open | Closed); the MIR path emits the flat-state form, which diverges from legacy. Byte-equivalence for escrow requires implementing the inductive-State emission. ~2–3 days. Largest single deferred item.
- **Preservation proof scripts** — Phase 1c-5 emits property preservation theorems as `:= sorry`. legacy lean_gen.rs has a `preservation_proof_script` helper that discharges via `if_neg` / `dsimp + omega` projection. ~half day.
- **`rewrite_subscripts_lean` pass for ref_impls** — Phase 1c-4 emits ref_impl bodies verbatim; legacy applies a `m[i]` → `(m i)` rewrite for Map-typed params. Triggers when a fixture uses ref_impls with Map subscripts — no pilot fixture does today. ~half day when needed.

### Phase 1d — snapshot equivalence — **shipped**

Snapshot tests live at `crates/qedgen/tests/mir_snapshot.rs` with
per-fixture `Spec.lean` snapshots under
`crates/qedgen/tests/snapshots/`. Each test regenerates the MIR
output (`QEDGEN_USE_MIR=1 qedgen codegen --lean`) into an isolated
`git init`'d tempdir and asserts byte-equality against the snapshot.
Drift fails the test with a unified diff; intentional updates run
through `UPDATE_SNAPSHOTS=1 cargo test --test mir_snapshot`.

The snapshots lock the MIR output (not vs legacy). MIR ↔ legacy
parity per fixture is documented in
`crates/qedgen/tests/snapshots/README.md`:

| Fixture | Path | MIR ⇆ legacy |
|---|---|---|
| `bundled-stdlib-demo` | ADT | byte-identical |
| `cross-program-vault` | ADT | byte-identical |
| `escrow-split` | ADT | byte-identical (vs fresh-legacy regen) after §15 `cover_trace_proof` port |
| `escrow` | flat | byte-identical (vs fresh-legacy regen) after Phase 1c-10 flat-path alignment |
| `lending` | multi-account | byte-identical (vs fresh-legacy regen) after Phase 2 multi-account renderer |
| `multisig` | indexed | byte-identical after Phase 1e indexed-state lowering |

ADT-path byte-equivalence is the Phase 1c-8 deliverable; escrow flat-
path byte-equivalence is the Phase 1c-10 deliverable; multisig
indexed-state is the Phase 1e deliverable; lending multi-account is
the Phase 2 deliverable. **Every pilot fixture is now byte-equivalent
to the legacy renderer**, gated by `cargo test --test mir_snapshot`.

### Honest scoping

**Lean.** Byte-equivalence reached for all six pilot fixtures across
all four state shapes (ADT, flat single-account, indexed, multi-
account). MIR is the default Lean codegen path post v2.30 Phase 2.

**Kani.** Phase 3a–3f shipped. **MIR is the default Kani-codegen
path** post Phase 3f flip (`QEDGEN_LEGACY_KANI=1` escape hatch).
All 6 pilot fixtures full-file byte-identical to legacy (escrow
268 / escrow-split 179 / bundled-stdlib-demo 155 / multisig 1324
/ the since-removed perp-dex risk engine 2365 / lending 511 lines). Snapshot tests gate every
pilot via `tests/kani_snapshot.rs` (mirrors `tests/mir_snapshot.rs`
for Lean). Pilot-scope guard sends sBPF (`pragma sbpf`) to legacy
unconditionally — MIR's `is_sbpf` is still a Phase-0 stub; records
(`type T { … }`) are MIR-supported via shared
`rust_codegen_util::emit_record_structs`, so no records carve-out
needed (unlike the Lean side).

**Anchor.** Phase 4a–4i shipped. **MIR is the default
Anchor/Quasar codegen path** post Phase 4i flip
(`QEDGEN_LEGACY_CODEGEN=1` escape hatch). 9 of 10 sub-generators
MIR-direct: 4b cargo_toml + math + events, 4c errors +
ref_impls, 4d imported_mirror, 4e lib, 4f state, 4h instructions.
4g `generate_guards` (636L) intentionally stays delegated to
legacy — its per-handler `requires` / `effects` / `auth` / `status`
emission is deeply coupled to `ParsedHandler` fields with no
clean structural seam; a meaningful MIR port requires lifting
requires + effects into typed `Stmt` nodes first (a separate
v3.0-class refactor). Snapshot tests in `tests/codegen_snapshot.rs`
gate every pilot via concatenated `programs/` tree dumps with
`--- <relpath> ---` headers between files.

**Proptest.** Phase 5 shipped. MIR is the default proptest codegen
path (`QEDGEN_LEGACY_PROPTEST=1` escape hatch). Same pure-
delegation scaffold pattern as Anchor's `generate_guards` deferral:
`proptest_gen_mir.rs::generate(&Mir, &ParsedSpec, &Path, &Path)`
delegates the full emit to legacy `proptest_gen::generate`
(per-handler `arb_state` / preservation / invariant / guard /
overflow / sequence harnesses are tightly coupled to ParsedHandler
fields — per-slot binders, abstract binders, effect lowering,
`old(...)` resolution — without a clean structural seam). Snapshot
tests in `tests/proptest_snapshot.rs` gate every pilot;
sub-emitter ports are a v3.0 cleanup.

## Next-session handoff

For the next session picking up this work:

**Branch & toolchain:**
- Branch: `mir` (8 commits ahead of `main`; `main` is v2.29.2).
- Local: `.cargo/config.toml` carries `rustflags = ["-C", "symbol-mangling-version=v0"]` for the macOS linker workaround. See [[reference-macos-linker-workaround]].

**Smoke commands:**
- `cargo test -p qedgen-solana-skills --bins lean_gen_mir::tests` — Lean MIR-codegen unit tests.
- `cargo test -p qedgen-solana-skills --bins kani_mir::tests` — Kani MIR-codegen unit tests.
- `cargo test -p qedgen-solana-skills --bins mir::tests` — MIR lowering tests.
- `cargo test -p qedgen-solana-skills --test mir_snapshot` — Phase 1d Lean snapshot equivalence over every pilot fixture.
- `cargo test -p qedgen-solana-skills --test kani_snapshot` — Phase 3f Kani snapshot equivalence over every pilot fixture. Use `UPDATE_SNAPSHOTS=1` to refresh after an intentional codegen change.
- `cargo fmt --check` + `cargo clippy -p qedgen-solana-skills -- -D warnings` — CI gates.
- `qedgen codegen --spec examples/rust/bundled-stdlib-demo/pool.qedspec --lean --kani` — run MIR (the default for both) end-to-end. Prefix with `QEDGEN_LEGACY_LEAN=1` / `QEDGEN_LEGACY_KANI=1` for the respective legacy renderers. Restore after eyeballing: `git checkout -- examples/rust/bundled-stdlib-demo/`.

**Where the pieces live:**
- `crates/qedgen/src/mir.rs` — typed IR + lowering. Search anchors: `pub struct Mir`, `pub enum Stmt`, `pub fn lower`.
- `crates/qedgen/src/lean_gen_mir.rs` — Lean emission. Section emitters are `emit_*` fns; the order in `render_single_account` mirrors `lean_gen.rs::render_single_account` (line 1177).
- `crates/qedgen/src/kani_mir.rs` — Kani emission. Section emitters mirror `kani.rs::emit_kani_account_section` order: structural → guard → abort → property → ensures → invariant → effect → overflow. Multi-account dispatch (`emit_multi_account_sections`) wraps each `account_type` in `mod <lowercase> { use super::*; ... }` driven by `scope_parsed_to_account`.
- `crates/qedgen/src/main.rs` — dispatch gates. Lean (`--lean`): pilot-scope guard sends sBPF + record-bearing specs to legacy; pilot specs route through MIR; `QEDGEN_LEGACY_LEAN=1` forces legacy. Kani (`--kani`): pilot-scope guard sends sBPF to legacy (records are MIR-supported); `QEDGEN_LEGACY_KANI=1` forces legacy.

**Suggested first move in the next session:**
1. **MIR carry-through for the remaining non-Lean codegens.** Kani
   (`kani.rs`, 2,437 LoC) and Lean (`lean_gen.rs`, 8,661 LoC) are
   both MIR-default and snapshot-gated. The remaining work is
   Anchor (`codegen.rs`, 7,572 LoC — handler shape impact) and
   proptest (`proptest_gen.rs`, 2,110 LoC — per-slot lowering
   impact). Same scaffold-then-section-walk pattern as Kani Phase
   3a-3f: create `<X>_mir.rs` mirroring the legacy entry shape,
   add an opt-in env var, port section-by-section, add a snapshot
   harness, flip the default, add the pilot-scope guard.
2. **Close the MIR pilot-scope carve-outs.** The dispatch guard
   currently sends two shape classes to legacy: (a) sBPF — needs
   `pragmas` lifted into MIR and `is_sbpf` un-stubbed (then a
   `render_sbpf` MIR port); (b) record-bearing specs (the
   perp-dex-risk-engine class) — needs `Mir.records` lift + per-field `structure T` +
   `instance : Inhabited T` emission + bare-field assign wrapping
   (`{ acct with active := 1 }` instead of MIR's current bare
   `(1)`). The legacy fallback covers correctness; closing the
   carve-outs lets the guard be removed.
3. **Retire `render_single_account_adt` ↔ `render_multi_account`
   split where possible.** Phase 2's per-account scoped-Mir +
   token-rename approach (`scope_mir_to_account` + `rename_state_idents`
   in `lean_gen_mir.rs`) is the proven pattern; the ADT path could
   eventually pivot the same way to share emitters with the flat
   path. Low-priority cleanup — defer to v3.0.

**What Phase 2 closed** (this session, 2026-05-25):
- `Mir.account_states: Vec<AccountStateMir>` carries every
  declared `type <Account>` block as a parallel state lift.
  Single-account specs keep `account_states.len() == 1`;
  `Mir.state` still points at the primary so the existing single-
  account renderers (`render_single_account`,
  `render_single_account_adt`, `render_indexed_state`) keep
  emitting the same output. `HandlerMir.on_account: Option<Symbol>`
  records the qualified pre-state account name (e.g. `Loan` from
  `: Loan.Empty -> Loan.Active`).
- `lean_gen_mir::render_multi_account` mirrors
  `lean_gen::render_multi_account` byte-for-byte for the lending
  fixture. Implementation strategy: per-account *scoped Mir*
  (`scope_mir_to_account`) reuses the existing single-account
  section emitters; `rename_state_idents` rewrites bare
  `State` / `Status` / `Operation` / `applyOp` / `applyOps`
  identifiers to their per-account form (`PoolState`,
  `LoanOperation`, `applyLoanOp`, …) before the block lands in
  the main buffer.
- Multi-account specifics handled in dedicated helpers:
  - `emit_invariants_as_comments` — variant-typed binder
    invariants emit structured `-- INVARIANT OBLIGATION` comments
    (lowering deferred to v3.0; mirrors
    `lean_gen::render_invariants_as_comments`).
  - `emit_properties_multi` + `group_properties_by_account` —
    properties group by which account's fields they touch; pass-2
    overflow theorems thread the right `h_inv_<prop>` hypothesis.
  - `emit_covers_multi` — section header always written when any
    covers exist; cross-account traces become skip-comments.
  - `emit_liveness_multi` — resolves the per-liveness account from
    `via_ops[0].on_account` so `liveness_loan_settles` correctly
    binds to `LoanState` + `applyLoanOps` + the legacy auto-
    discharge script.
  - `emit_environments_multi` + `emit_environments_no_header` —
    per-property-group binding + bare-field-name rewrite
    (`constraint interest_rate > 0` →
    `(h_c0 : new_interest_rate > 0)`).
- Unit tests `render_emits_invariant_theorems`,
  `render_emits_cover_theorems`, `render_emits_liveness_theorems`,
  `render_emits_properties_with_preservation` updated to assert
  the correct multi-account shape (they previously asserted on
  the pre-Phase-2 broken single-account collapse output).

Result: `cargo test --test mir_snapshot snapshot_lending` passes
with the snapshot byte-identical to a fresh legacy regen (cksum
match). Every pilot fixture now byte-equivalent across MIR ↔
legacy.

**What Phase 1e closed** (this session, 2026-05-25):
- `Ty::Map { capacity, value }` capacity field is now `Symbol`
  (`String`) instead of `u32`, so `Map[MAX_MEMBERS] Pubkey`
  parses correctly (previously fell through to
  `Ty::Custom("Map[MAX_MEMBERS] Pubkey")` because the matcher
  required a numeric literal). Unblocks `is_indexed` detection
  for fixtures using const-name capacities.
- `render_indexed_state` MIR renderer ported (replaces the
  Phase-1-stretch stub). Emits the legacy `import
  Mathlib.Algebra.BigOperators.Fin` / `QEDGen.Solana.Account` /
  `QEDGenMathlib.IndexedState` triple; `open QEDGen.Solana.IndexedState`;
  `abbrev AccountIdx : Type := Fin <bound>` (via
  `pick_account_idx_bound_mir`); `inductive Status`; flat-State
  struct projecting the active variant's fields with `Map N T`
  rendering; transitions with `Fin N` param promotion +
  parenthesized requires + `Function.update`-collapsed
  indexed effects; `inductive Operation` (no `deriving`); `def
  applyOp`; property predicate `def`s only (no preservation
  theorems — `Proofs.lean` carries those).
- Helpers added: `rewrite_subscripts_lean`, `parse_indexed_lhs`,
  `infer_idx_promotions_mir`, `scan_indexed_in_expr`,
  `effect_value_to_lean_mir`, `pick_account_idx_bound_mir`,
  `collect_map_roots`, `emit_indexed_transition`,
  `emit_indexed_operation_inductive`, `render_ty_indexed`. All
  mirror their `lean_gen.rs` counterparts byte-for-byte.

Result: `multisig` snapshot byte-identical to fresh-legacy
regen. The MIR path now covers every pilot fixture except
`lending`, whose legacy multi-account renderer is Phase 2.

**What Phase 1c-11 closed** (prior session, 2026-05-25):
- `overflow_proof_script` ported to MIR (`emit_overflow_inner`):
  flat-state overflow theorems now discharge via `unfold + split
  + cases + refine ⟨h_valid.*, ?_⟩ + simp [valid_*, Valid.valid_*,
  Valid.*_MAX]; omega`, byte-identical to legacy.
- `preservation_proof_script` ported to MIR
  (`preservation_proof_script` helper): per-handler
  `<prop>_preserved_by_<op>` sub-lemmas now discharge by
  `unfold <Trans>; split at h` + `(touches-prop-field ? unfold
  <prop>; dsimp; omega : exact h_inv)` + `contradiction`. Matches
  legacy verbatim modulo state-type names (multi-account legacy
  uses `PoolState`/`LoanState`; MIR flat-state uses `State`).
- Master `<prop>_invariant` theorem now auto-proves by `cases op
  with` (`master_invariant_proof_script` helper) — delegates to
  `<prop>_preserved_by_<op>` for handlers in `preserved_by`;
  inline proof for the rest (trivial `subst` + `exact h_inv` when
  no field overlap, else `simp [applyOp]` + nested
  unfold/split/dsimp/omega). Naming still `_invariant` vs
  legacy's `_inductive` — see "Suggested first move" item 3.
- Tests: `mir_snapshot` refreshed for `lending` + `multisig`
  (the only fixtures with `property` blocks); `escrow`,
  `escrow-split`, `bundled-stdlib-demo`, `cross-program-vault`
  unaffected.

Result: every flat-state overflow + preservation `:= sorry`
becomes a real tactic discharge. Remaining flat-vs-legacy
divergences are structural (Phase 2 multi-account split, Mathlib
indexed-state imports, master theorem name) and tracked above.

**What Phase 1c-10 closed** (prior session, 2026-05-25):
- `inductive Status` deriving order + bare-variant shape
- `structure State` deriving order (`Repr, DecidableEq, BEq`)
- Transition body conjuncts: signer-equality (`signer = s.<who>`
  when `who` is a state field), lifecycle gate (`s.status = .<pre>`),
  auto under/overflow guards, requires-clause filtering for
  handler-account pubkey refs
- Conditional auth-alias suppression (only when `who` is NOT a
  state field, otherwise the conjunct already pins it)
- `else none` single-line form
- Requires-based abort theorem auto-proof (`if_neg`-with-projection
  via `abort_requires_proof`)
- Liveness statement shape (`∃ ops, ... ∧ ∀ s', ... →` when
  `find_liveness_path` returns Some) + auto-proof script
  (`liveness_proof_script`)

Result: `escrow` flat-path snapshot byte-identical vs fresh-legacy
regen.

**What NOT to do without revisiting:**
- Don't try to refactor the flat-path emitter into a "deriving
  preference" parameter shared with the ADT path — the ADT and flat
  emitters have legitimately different goals (variant pattern-match
  vs flat-struct guards) and the byte-shape mismatch isn't just
  formatting drift. Port the legacy emitter behavior section-by-
  section like Phase 1c-8 did.
- Don't add a parallel `Mir.interfaces` lift alongside
  `Mir.imports` — the unified shape resolved in
  [`mir-unified-imports.md`](mir-unified-imports.md) makes
  `Mir.imports` canonical. Re-introducing the split would re-
  create the exact debt this MIR exercise pays down.

## What the companion docs validate

| Claim in this sketch | Evidence |
| --- | --- |
| Four primary codegens, not five | `codegen-baseline.md` "5 codegens framing is slightly inflated" |
| ParsedEffectBranches divergence is real (Lean-only) | `codegen-baseline.md` table + `codegen-divergence.md` A1 |
| Variant-promotion gap in Kani/proptest | `codegen-divergence.md` A2 |
| Abort semantics divergence | `codegen-divergence.md` A3 |
| Lifecycle gating is the highest-weight cross-cutting concern | `codegen-baseline.md` (60–76 refs/codegen) + `codegen-divergence.md` B2 |
| TokenTransfer is the only meaningful CPI shape | `intrinsic-fixture-map.md` (8 of 9 CPI occurrences) |
| `RequireOrAbort` is the most-used non-arithmetic node | `intrinsic-fixture-map.md` (15 fixtures, 96 uses) |
| AccountTable carries the largest cross-codegen surface | `intrinsic-fixture-map.md` "Implications" §2 |
| Half the proposal's intrinsic list lacks fixture evidence | `intrinsic-fixture-map.md` "What's in the proposal but not in fixtures" table |
| Phase 3 estimate was off by ~5× | `cross-cutting-transforms.md` "Phase ordering implication" |

## Cross-references

- `docs/design/mir-unified-imports.md` — Phase 1c-7 design note. `Mir.imports` collapses the parallel `ParsedSpec.interfaces` + `ParsedSpec.imported_namespaces` surfaces into one canonical lifted structure; sequencing + open questions + validation plan.
- `docs/design/spec-composition.md` — Tier 1/2/3 interface composition (relates to `Mir.imports[*].interfaces`).
- `docs/design/pre-post-property-lowering.md` — current pre/post handling at the ParsedSpec level; lowering moves into parser→MIR.
- `crates/qedgen/src/lean_gen.rs` — current Lean codegen; Phase 1 rewrites this on top of MIR.
- `crates/qedgen/src/codegen.rs` — current Anchor/Quasar codegen; Phase 2 rewrites.
- `crates/qedgen/src/cpi_substitute.rs` — current CPI substitution; Phase 3 ports to MIR construction time.
- Issue #66 (the original proposal) — this sketch is the qedgen-local refinement of #66 after measurement.
- Issue #67 (`.qedspec` evolution: rules vs invariants, ghost vars, hooks, quantifiers, havoc) — items 1, 2, 4, 5 land above MIR (parser changes only); item 4 (hooks) is gated on `Stmt`-boundary instrumentation which MIR makes possible.
