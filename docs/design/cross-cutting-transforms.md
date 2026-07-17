# Cross-cutting transforms — catalog

**Captured:** 2026-05-24, `mir` branch off v2.29.2.

**Purpose.** Issue #66 names `cpi_substitute.rs`, `consolidate.rs`, and schema-include expansion as candidate MIR→MIR passes. The reality is more nuanced — some are already-shared pre-codegen passes (no port needed), some are per-codegen passes (genuine MIR→MIR candidates), some operate on emitted artifacts (out of MIR scope). This file classifies every cross-cutting transform in qedgen so Phase 3 (the "move passes to MIR→MIR" phase) knows what's actually in scope.

For each pass: **location**, **inputs**, **outputs**, **codegens that consume it**, **classification**.

---

## Classification scheme

- **PRE-CODEGEN (shared)** — runs once on `ParsedSpec` before any codegen reads it. Already shared across codegens; **no port needed**. MIR can either consume the already-expanded form or run the same logic at MIR construction time.
- **PER-CODEGEN (genuine MIR→MIR candidate)** — invoked separately by each codegen (or by a subset of codegens). MIR can centralize as a `mir.rs::passes::*` function consumed once.
- **POST-CODEGEN (out of MIR scope)** — operates on emitted source artifacts. Not a MIR→MIR concern.
- **AUXILIARY (separate command path)** — used by a CLI command other than `codegen`/`check` (`qedgen reconcile`, `qedgen ratify`, `qedgen spec`). Not in the codegen pipeline; out of MIR scope.

---

## A. PRE-CODEGEN (shared) — no port needed

### A1. Schema-include expansion

**Location.** `chumsky_adapter.rs:3125-3142` (post-parse pass).
**Inputs.** `ParsedSpec.schemas` (top-level `schema <name> { requires X else Err … }` blocks) + per-handler `schema_includes: Vec<String>` references.
**Outputs.** Mutates each handler in place, appending the referenced schema's `requires` clauses to the handler's own `requires` list.
**Consumed by.** All codegens read the expanded `ParsedHandler.requires` — they don't see the original include directives.
**Classification.** **PRE-CODEGEN.** Operates above the codegen layer; runs once during parse. No work to port — MIR's parser→MIR lowering reads the already-expanded `ParsedHandler` and the includes are gone.

### A2. Dotted-auth desugaring

**Location.** `chumsky_adapter.rs:3144+` (`desugar_dotted_auth`).
**Inputs.** `ParsedHandler.who = Some("admin_config.admin_key")` (v2.29.1 dotted form).
**Outputs.** Synthesizes a `requires admin_config.admin_key == signer.pubkey else Unauthorized` clause; rewrites `handler.who` to the signer account name.
**Consumed by.** All codegens see the desugared form.
**Classification.** **PRE-CODEGEN.** Same shape as A1 — runs during parse, no per-codegen work.

### A3. Import resolution

**Location.** `import_resolver.rs` (1,562 LoC).
**Inputs.** `Manifest` (parsed `qed.toml`) + `ParsedSpec.imports`.
**Outputs.** Fetched/cached source bytes per imported spec; populates `ParsedSpec.imported_namespaces` (v2.29 Slice F).
**Consumed by.** All codegens; type/name resolution reads `imported_namespaces`.
**Classification.** **PRE-CODEGEN.** Runs once during the `qedgen check`/`codegen` pipeline before any codegen executes. Out of MIR's per-spec lowering scope — the resolved `ParsedSpec` flows into MIR construction with imports already merged.

### A4. CPI envelope substitution (the cpi_substitute.rs module itself)

**Location.** `cpi_substitute.rs` (483 LoC).
**Inputs.** Per-call-site `ParsedCall` (callee args) + `ParsedStateBinder` (state field bindings) + callee's `ParsedInterface.handlers[i].ensures`.
**Outputs.** Substituted `String` expressions per ensures clause — both `lean_expr` (consumed by Lean) and `rust_expr_binary` (consumed by Kani).
**Consumed by.** `lean_gen.rs::render_cpi_theorems` (11 refs), `kani.rs::emit_assume_calls` (1 ref), `kani_impl.rs::emit_assume_calls` (6 refs). `codegen.rs` and `proptest_gen.rs` do **not** consume it (see divergence A4 in `codegen-divergence.md`).
**Classification.** **PER-CODEGEN (genuine MIR→MIR candidate).** Currently invoked separately by each consuming codegen with slightly different parameter shapes (Lean uses `lean_expr`, Kani uses `rust_expr_binary`). In MIR, `Stmt::Cpi` carries the call shape and substituted ensures can be precomputed once at MIR construction time — every consumer reads the same `Cpi.callee_ensures: Vec<Predicate>` field. **proptest gains free** by consuming the same field that Lean does (closing the A4 divergence).

**Effort estimate.** Moderate — substitution logic itself is mature (regex word-boundary replace); the port is ~150 LoC of relocation + adding the consumed-by-proptest path. ~2-3 days.

---

## B. PER-CODEGEN (genuine MIR→MIR candidates)

### B1. Lifecycle gating

**Location.** Distributed across `lean_gen.rs` (76 refs), `kani.rs` (68 refs), `codegen.rs` (65 refs), `proptest_gen.rs` (60 refs).
**Inputs.** `ParsedHandler.from_variant`, `ParsedHandler.to_variant`, `ParsedSpec.state.variants`.
**Outputs.** Per-codegen: lean theorem preconditions (`s.status = StateActive`), Anchor `require!` preambles, Kani `kani::assume`, proptest `prop_assume`.
**Consumed by.** All four primary emit modules independently.
**Classification.** **PER-CODEGEN (genuine MIR→MIR candidate).** Highest divergence-class severity per the divergence inventory (B2). MIR moves this to a `HandlerMir.transition: Option<(VariantTag, VariantTag)>` field + a `Stmt::LifecycleGate { allowed: Vec<VariantTag> }` synthesized by parser→MIR lowering. Each codegen has one canonical lowering for the gate node.

**Effort estimate.** Largest single MIR→MIR port. ~5-7 days because the dispatch is spread across the four codegens, each with idiosyncratic styles. Likely lands in Phase 2 alongside lifecycle-using fixtures.

### B2. Variant-promotion logic

**Location.** `codegen.rs` (15 refs, dominant), `lean_gen.rs` (3 refs).
**Inputs.** `ParsedHandler.effects` where one effect targets `state` with a `.Variant {...}` RHS; `ParsedSpec.state.variants` for destructure pattern.
**Outputs.** Anchor: destructure-and-rebuild Rust (v2.29 Slice C). Lean: corresponding theorem cases.
**Consumed by.** `codegen.rs` (full lowering) + `lean_gen.rs` (theorem cases). `kani.rs` and `proptest_gen.rs` do not consume it (divergence A2 in `codegen-divergence.md`).
**Classification.** **PER-CODEGEN (genuine MIR→MIR candidate).** MIR lifts to `Stmt::VariantPromote { from_tag, to_tag, payload_bindings }`. Closes A2 by giving Kani and proptest a first-class node to consume.

**Effort estimate.** ~2-3 days. The destructure-and-rebuild logic itself stays in codegen.rs; what moves is the *trigger* (was: implicit pattern-match on effect shape; becomes: explicit `Stmt::VariantPromote` consumption).

### B3. Effect-op string-literal dispatch

**Location.** Per the baseline doc — 33/17/6/2 string-literal matches across `lean_gen.rs`/`codegen.rs`/`kani.rs`/`proptest_gen.rs`.
**Inputs.** `ParsedHandler.effects: Vec<(field, op_kind, value)>` where `op_kind` is `"set"` / `"add"` / `"add_sat"` / `"add_wrap"` / `"sub"` / `"sub_sat"` / `"sub_wrap"` / etc.
**Outputs.** Per-codegen lowering: lean `field := s.field + value`, Anchor `s.field = s.field.checked_add(value).ok_or(Err)?`, Kani `let new = s.field.checked_add(value).unwrap();`, proptest model update.
**Consumed by.** All four codegens.
**Classification.** **PER-CODEGEN (genuine MIR→MIR candidate).** MIR lifts effect ops to typed `Stmt` kinds: `Stmt::Assign`, `Stmt::CheckedAdd`, `Stmt::CheckedSub`, `Stmt::SatAdd`, `Stmt::SatSub`, `Stmt::WrapAdd`, `Stmt::WrapSub`. One canonical lowering per node per codegen. Closes B1 by construction.

**Effort estimate.** ~3 days per codegen × 4 = ~12 days, but with most of the per-codegen work being mechanical (translate each existing string-literal arm to a `Stmt` arm). Phase 1 covers Lean + Anchor; Phase 3 covers Kani + proptest.

### B4. Account-block lowering (writable / pda / init / authority / type)

**Location.** `codegen.rs` (Anchor `#[derive(Accounts)]` emission, hundreds of lines), `lean_gen.rs` (account context for theorem state), `kani.rs` + `kani_impl.rs` (symbolic account binding), `proptest_gen.rs` (generator-side account model).
**Inputs.** `ParsedHandler.accounts: Vec<ParsedAccount>` carrying `writable`, `readonly`, `init`, `pda`, `authority`, `type token` annotations.
**Outputs.** Per-codegen lowering of the accounts context.
**Consumed by.** All codegens.
**Classification.** **PER-CODEGEN (genuine MIR→MIR candidate).** Account-block features are the highest-volume fixture-feature (114 + 100 + 74 + 24 + 27 references per the intrinsic-fixture map). MIR's `Mir.accounts: AccountTable` + per-handler `Vec<AccountBinding>` carries the structure uniformly. Per the intrinsic-fixture map's implication (3), this is probably where the most cross-codegen LoC sits — Phase 0 should design `AccountTable` first.

**Effort estimate.** ~5-7 days for the AccountTable design + porting Anchor's `#[derive(Accounts)]` emission. Other codegens' account-side surface is smaller. Phase 1 work, alongside the TokenTransfer pilot.

---

## C. POST-CODEGEN (out of MIR scope)

### C1. Proof-project consolidation

**Location.** `consolidate.rs` (208 LoC).
**Inputs.** Multiple emitted Lean proof project directories.
**Outputs.** One merged project with a unified lakefile.
**Consumed by.** `qedgen consolidate` CLI command.
**Classification.** **POST-CODEGEN.** Operates on emitted artifacts after codegen has run. Out of MIR scope; stays in place. Issue #66's listing of consolidate.rs as a candidate transform is a misreading — it's not a per-spec pass.

---

## D. AUXILIARY (separate command path)

### D1. Anchor IDL → `.qedspec` stub generation

**Location.** `interface_gen.rs` (245 LoC).
**Inputs.** Anchor IDL JSON.
**Outputs.** A shape-only `interface Name { ... }` qedspec block.
**Consumed by.** `qedgen spec --idl ...` CLI command (deprecated, v3.0 removal — the IDL is now a probe evidence source; spec elicitation runs probe → answers → ratify). Not invoked by `codegen`.
**Classification.** **AUXILIARY.** Out of MIR scope; runs at a separate CLI command path.

### D2. Reconcile / drift detection

**Location.** `reconcile.rs` (732 LoC) + `spec_hash.rs` (830 LoC).
**Inputs.** `#[qed(verified, spec_hash = "...")]` attribute occurrences in user source + corresponding spec sections.
**Outputs.** Drift report.
**Consumed by.** `qedgen reconcile` CLI command + the `qedgen-macros` proc-macro at compile time.
**Classification.** **AUXILIARY.** Out of MIR scope; operates on the spec-vs-source pair, not on lowered IR. Spec hashing in particular runs over `.qedspec` source text directly per `spec_hash.rs`'s contract with `qedgen-macros::content_hash`.

### D3. Audit-to-spec ratification

**Location.** `ratify.rs`.
**Inputs.** `interview.md`, `clusters.json`, `skeleton.qedspec` from a prior `qedgen probe`.
**Outputs.** Ratified `.qedspec` + scoping/findings docs.
**Consumed by.** `qedgen ratify` CLI command.
**Classification.** **AUXILIARY.** Out of MIR scope.

---

## Summary — MIR→MIR pass scope is narrower than #66 implies

| Class | Count | Total LoC | In MIR scope? |
|---|---:|---:|---|
| PRE-CODEGEN (A1–A3) | 3 | ~1,600 | No port — already shared, lives in parser/check |
| **PER-CODEGEN** (A4, B1, B2, B3, B4) | **5** | **~1,500 in shared modules + heavy distribution across codegens** | **Yes — these are the actual MIR→MIR candidates** |
| POST-CODEGEN (C1) | 1 | 208 | No — operates on emitted artifacts |
| AUXILIARY (D1–D3) | 3 | ~1,800 | No — separate CLI command paths |

Five genuine MIR→MIR candidates: **CPI substitution** (A4), **lifecycle gating** (B1), **variant promotion** (B2), **effect-op dispatch** (B3), **account-block lowering** (B4). The LoC weight is *distributed* across codegens rather than concentrated in shared modules — porting these is the heart of MIR's value, not relocating `cpi_substitute.rs`.

## Phase ordering implication

The MIR proposal's Phase 3 ("move `cpi_substitute` + account-inference to MIR→MIR passes, 3-5 days") understates by 3-4× because:

- The actual MIR→MIR work is **B1 + B2 + B3 + B4** (lifecycle + variant + effect-op + accounts), not just cpi_substitute.
- Each per-codegen `match` arm has to be rewritten to consume `Stmt`/`Mir` types instead of `(field, op_kind, value)` strings.
- AccountTable design (B4) is foundational — it touches every codegen's account-side surface and probably wants to land **before** TokenTransfer pilot (Phase 1), not in Phase 3.

Realistic per-pass estimates:
- A4 (cpi_substitute relocation + proptest adoption): 2-3 days
- B1 (lifecycle gating to `HandlerMir.transition`): 5-7 days
- B2 (variant promotion to `Stmt::VariantPromote`): 2-3 days
- B3 (effect-op string → typed `Stmt` kinds): 3 days × 4 codegens = ~12 days (split Phase 1/Phase 3)
- B4 (account-block lowering to `AccountTable`): 5-7 days

**Aggregate:** 27-32 days of cross-cutting-transform porting alone. Plus the codegen-side `match Stmt` rewrites that consume each pass's output. Original Phase 3 estimate (3-5 days) was an order of magnitude off.

This isn't an argument against MIR — it's an argument for being honest about Phase 3's scope when scheduling.
