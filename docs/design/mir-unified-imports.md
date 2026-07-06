# MIR shape for cross-program references — unified imports

**Status:** Design note for Phase 1c-7 (CPI theorem emission). Pre-implementation; written 2026-05-25 to resolve the design blocker called out at `qedgen-mir-sketch.md:357-364`.

**Position:** Strengthens [[feedback-unified-imports-semantics]] with the MIR-layer corollary. Where the existing rule says "import resolves whole qedspecs, not just interface stubs," this note says **imports are the canonical primitive at MIR too** — `Mir.imports` is the sole lifted structure for cross-program references, and inline `interface` blocks are a degraded shape of it (Tier 0, no upstream pin). No parallel surfaces, no synchronization views.

**Why this matters beyond CPI:** the MIR exercise isn't just porting `lean_gen.rs` into a typed IR. It's an audit pass that exposes design debt accumulated across v2.5 → v2.29 of CPI/interface/import work — three concepts that grew side-by-side and ended up storing the same imported source's data in two parallel `ParsedSpec` fields that no single codegen reader joins back together. Phase 1c-7 is the right time to collapse them, because the MIR lowering is the natural cut-point.

## Current shape — the duplication is structural

Two parallel collections on `ParsedSpec`:

| Field | Defined | Carries |
|---|---|---|
| `pub interfaces: Vec<ParsedInterface>` | `check.rs:1163` | Call contracts (`handlers`, `ensures`, `requires`, `program_id`, `upstream` pin) |
| `pub imported_namespaces: BTreeMap<String, ImportedNamespace>` | `check.rs:1277` | Type declarations (`account_types`, `records`) |

**The merge loop fans every imported source into BOTH.** At `check.rs:1887-1919`:

```rust
// 1. Push the call-contract half into parsed.interfaces
parsed.interfaces.push(merged);

// 2. Conditionally push the type-decl half into parsed.imported_namespaces
//    (skipped when account_types is empty — the SPL Token / System Program
//    / Metaplex bundled-stub case)
if !imported.account_types.is_empty() {
    parsed.imported_namespaces.insert(local_ns_name, ImportedNamespace { ... });
}
```

**Downstream readers only ever see half.** No single codegen reader joins them back together:

| Reader | Reads from | Purpose |
|---|---|---|
| `codegen.rs:763, 1277, 1288, 1473, 4647, 5248` | `imported_namespaces` | Rust mirror at `src/imported/<ns>.rs`; account-type ref resolution |
| `lean_gen.rs:510, 1920-2150, 5882` | `interfaces` | CPI theorem emission; pinned-axiom application; sibling `<Iface>.lean` modules |
| `check.rs:5882, 9169, 9653, 9895-9972` | `interfaces` | Lint surface; test assertions |

The consequence: when a user imports a full qedspec, the **types** flow to one codegen and the **call contracts** flow to a different codegen, and neither knows the other half exists for the same import. That's load-bearing for v2.27 Track A (state-aware ensures with `state_binders`) where a CPI theorem needs both — and the current `lean_gen.rs` path works around it by re-walking `spec.interfaces` in isolation and accepting that abstract-state field types come from a separate place.

## The MIR-layer collapse

Under [[feedback-unified-imports-semantics]] applied at MIR:

```rust
pub struct Mir {
    // ... existing fields ...

    /// Cross-program references — the sole lifted structure for
    /// everything an `import` resolves to AND every inline `interface
    /// { … }` block in the consumer's own spec. Inline blocks are
    /// the degraded shape (Tier 0, no upstream pin), keyed by the
    /// interface name itself per §"Open questions" #2 resolution.
    pub imports: BTreeMap<NamespaceAlias, ImportedSpecMir>,
}

/// One imported source — both types and call contracts come from the
/// same artifact, warranted by the same `binary_hash` pin.
#[derive(Debug, Clone)]
pub struct ImportedSpecMir {
    /// Local alias used by `call <alias>.handler(...)` and
    /// `<alias>.<Type>` references. Falls back to bound name when
    /// no `as` clause is declared.
    pub alias: Symbol,
    /// Where the imported source came from — a built-in key
    /// (`"spl"`, `"system"`), a file path, or the `Inline` marker
    /// for inline `interface` blocks (no source, no warrant).
    pub origin: ImportOrigin,
    /// Account-type declarations exported by the imported spec.
    /// Re-emitted as Rust mirrors at `src/imported/<alias>.rs`.
    /// Empty for Tier-0 interface-only stubs.
    pub account_types: Vec<AccountTypeDecl>,
    /// Record types referenced by the imported account types.
    pub records: Vec<RecordTypeDecl>,
    /// Interface (call-contract) declarations the imported spec
    /// exports. Each carries handlers + ensures + requires + the
    /// abstract state-field vocabulary (v2.27 Phase 0).
    pub interfaces: BTreeMap<Symbol, InterfaceDecl>,
    /// `upstream { binary_hash = ... }` pin warranting THE WHOLE
    /// IMPORTED ARTIFACT. The pin justifies trusting both
    /// `interfaces` ensures AND `account_types` layouts — these are
    /// the same artifact, not two contracts.
    pub upstream: Option<UpstreamPin>,
}

#[derive(Debug, Clone)]
pub enum ImportOrigin {
    /// Built-in stdlib key resolved through the bundled qedspec at
    /// `crates/qedgen/data/interfaces/<key>.qedspec`.
    Builtin(Symbol),
    /// File-path import from the consumer's `qed.toml`.
    File(std::path::PathBuf),
    /// Inline `interface Foo { … }` block declared in the consumer's
    /// own spec. The interface's name IS the namespace alias under
    /// this origin — same data shape as an external import, just
    /// without an `upstream` pin (Tier 0 by construction). No "self-
    /// import" indirection; the caller is declaring a contract surface
    /// without warrant.
    Inline,
}
```

`Stmt::Cpi` gains its per-call-site projection:

```rust
Cpi {
    /// References the alias in `Mir.imports`.
    target: NamespaceRef,
    /// Which interface within that namespace.
    interface: Symbol,
    /// Which handler within that interface.
    method: Symbol,
    args: Vec<CallArg>,
    /// v2.27 Track A — caller-supplied projections from the callee's
    /// abstract state vocabulary onto the caller's concrete State
    /// fields. The callee namespace makes the abstract vocabulary
    /// unambiguous; no side-table needed.
    state_binders: Vec<StateBinder>,
    result_binding: Option<Symbol>,
}

#[derive(Debug, Clone)]
pub struct StateBinder {
    /// Callee abstract field (e.g., `pool_balance` from the imported
    /// `state { pool_balance : Nat, ... }` block).
    pub callee_field: Symbol,
    /// Caller concrete projection (a `Path` into the caller's State).
    pub caller_projection: Path,
}
```

## Design debt being paid down

This is the audit lens — what the MIR exercise lets us simplify that we couldn't simplify in place under v2.x's "additive features only" cadence:

1. **The parallel `ParsedSpec.interfaces` + `ParsedSpec.imported_namespaces` collections collapse to one** at the MIR layer. `ParsedSpec` retains them as the parse-layer truth (no v2.x break), but every downstream MIR-aware reader walks `Mir.imports` instead. Eventually the parse-layer split fades too.

2. **The conditional fan-out at `check.rs:1912` becomes a single unconditional insert.** Today's parse-time merge writes call contracts to `parsed.interfaces` unconditionally but writes type declarations to `parsed.imported_namespaces` only when `!imported.account_types.is_empty()`. The SPL Token / System Program / Metaplex bundled stubs fall through the conditional — their account types are empty — and as a result they only appear in `parsed.interfaces`. Under the unified shape, every imported source must appear in `imported_namespaces` so the MIR lowering's single canonical input is complete. Drop the conditional. Tier-0 stubs become entries with `account_types: vec![]` — present and queryable, just empty. The "does this import have types?" question stops being interesting at the parse layer too; it's a property of the entry, not a precondition for the entry's existence.

3. **The bundled stdlib stops being a special case at the codegen layer.** SPL Token / System Program / Metaplex already ship as qedspec files at `crates/qedgen/data/interfaces/`. The parse layer resolves them through the same path as user imports. But the codegen layer still treats them as "interface stubs without types" — under unification, they're just imports whose `account_types` happens to be empty, and the same emit code handles them.

4. **Inline `interface { ... }` blocks stop being a parser fork.** Today they take a different path through the merge loop (they're already on `ParsedSpec.interfaces` and never reach `imported_namespaces`). Under the unified MIR, they lower as `ImportOrigin::Inline` entries in `Mir.imports` — same shape, same downstream code path, no fork.

5. **`state_binders` get a natural home.** v2.27 Track A added per-call-site abstract→concrete state projections; today they're a field on `ParsedHandlerCall` consumed by `lean_gen.rs` directly. Under the unified shape, they're a field on `Stmt::Cpi` and the codegen reads MIR through the same access pattern as every other Stmt.

6. **Sibling Lean module naming becomes principled.** Today, the CPI emitter writes `<Iface>.lean` next to `Spec.lean` for pinned interfaces (`Token.lean`, `System.lean`, `Metadata.lean`). Under unification, naming gets a layer of qualification — `<namespace>.<interface>.lean` or `imports/<namespace>/<interface>.lean` — which prevents user-import collisions. (Cosmetic for the bundled stdlib; matters when two user imports both declare an interface named `Vault`.)

7. **The "Tier 0/1/2" mental model from [[project-cpi-composition]] becomes derivable, not declared.** Tier 0 = `ImportedSpecMir` with empty `interfaces[].ensures`. Tier 1 = `ImportedSpecMir` with non-empty ensures and `Some(upstream)`. Tier 2 = same as Tier 1 plus a bundled proof package at `crates/qedgen/data/proofs/`. No need for a separate tier classifier on the data structure — the tier is a query over the shape.

## Migration sequence

Seven concrete steps. The order matters because the rest of MIR doesn't touch interfaces today (it's an empty stub), so the cost is localized to the parse-layer adjustments + the new MIR code + the CPI emitter port. User authorized in-port breakage 2026-05-25; steps 0 and 1 take advantage of that.

0. **Drop the conditional at `check.rs:1912`** so every imported source registers in `parsed.imported_namespaces` regardless of whether `account_types` is empty. Tier-0 bundled stubs (SPL Token, System Program, Metaplex) become entries with `account_types: vec![]`. This makes `imported_namespaces` the single canonical parse-layer truth for "every imported source" — without it, the MIR lowering can't find bundled stubs and the CPI emitter regresses immediately.

1. **Move the "skip empty account_types" gate from parse-time to codegen.rs.** Today the conditional doubles as a Rust-mirror suppressor (codegen.rs:1277 + 5248 iterate `imported_namespaces` and would generate empty `src/imported/<ns>.rs` files for bundled stubs once Step 0 lands). Add an explicit `if ns.account_types.is_empty() { continue; }` at each mirror-generation site. The decision moves to the consumer — parse-layer says "every import is recorded," codegen says "don't generate mirrors for empty type sets." Cleaner separation of concerns; no behavior change for users who already had non-empty imports.

2. **Add `ImportedSpecMir` + `ImportOrigin` + `StateBinder` types in `mir.rs`** (no readers yet, no behavior change). Wire through doc strings cross-referencing this note.

3. **Remove the stub `Mir.interfaces` field + `lower_interfaces` fn.** Per §"Open questions" #1 resolution: the field vanishes. The CPI emitter walks `Mir.imports` directly. This ripples through every `Mir { ... }` construction site (`lower()`, `mir_types_construct`, the two `render_emits_*` helpers tests) — search for `interfaces: InterfaceRegistry` and drop the line. The `InterfaceRegistry` / `InterfaceDecl` / `InterfaceMethod` types stay (used by `ImportedSpecMir.interfaces`); only the top-level `Mir.interfaces` field goes.

4. **Implement `lower_imports(parsed) -> BTreeMap<Symbol, ImportedSpecMir>`** reading `parsed.imported_namespaces` as primary input. Synthesize inline `interface` blocks (entries in `parsed.interfaces` whose `name` doesn't match any `imported_namespaces` key) as `ImportOrigin::Inline` entries — the interface name IS the namespace alias, no synthetic key. The `upstream` pin lifts from `ParsedInterface.upstream` for external imports; `None` for inline (Tier 0 by construction).

5. **Add `state_binders: Vec<StateBinder>` to `Stmt::Cpi`** and thread through `lower_handler`. The parse-time data is already on `ParsedHandlerCall.state_binders`; this is a structural rename + a type tightening (`Path` instead of free string for the caller projection).

6. **Port `lean_gen::render_cpi_theorems` → `lean_gen_mir::emit_cpi_theorems`** reading the new shape. The transfer-envelope half (driven by `Stmt::TokenTransfer`) is independent and can ship first if §8 needs to fork into two slices. The call-site ensures-as-axiom half walks `Mir.imports` for interface resolution and reads `Stmt::Cpi.state_binders` for substitution.

**Estimated effort:** 3–4 days (up from 2–3 since the parse-layer steps add a half-day of test churn). The new code is bounded; the risk is the existing emit fns that construct `Mir { ... }` literals — every one needs the `interfaces: InterfaceRegistry::default()` line dropped, then re-added during the cross-fade if any backward-compat path still references the old field. Plan to do steps 3–4 as a single commit so the test suite is never wedged between shapes.

## Open questions

1. **Does `Mir.interfaces` stay as a flat lookup view, or vanish entirely?** — **RESOLVED 2026-05-25: vanish.**
   - **Why not a view:** the explicit goal is collapsing "same data in two places." A view re-creates that pattern — just centralized at one sync point instead of distributed. References back into `Mir.imports` introduce lifetime friction that costs more in code than the linear scan ever saved.
   - **Why the scan is fine:** specs have ≤ ~5 imports, ≤ ~3 interfaces per import. `imports.values().flat_map(|imp| imp.interfaces.values())` is a 15-entry walk per CPI emission. Performance is a non-issue at this scale.
   - **Decision:** no `Mir.interfaces`. The CPI emitter walks `Mir.imports` directly. The migration step is in §"Migration sequence" #3.

2. **Inline `interface` lowering — synthetic self-import or special case?** — **RESOLVED 2026-05-25: same shape as external imports under `ImportOrigin::Inline`; no synthetic key.**
   - **Why not "self-import":** that framing was misleading. An external import points at a source with a `binary_hash` pin (warrant). An inline interface has no source, no pin — it's the caller declaring a contract surface without warrant. That IS the Tier 0 shape per [[project-cpi-composition]]. Calling it a "self-import" suggests an indirection that doesn't exist.
   - **Why not a parallel field:** `Mir.inline_interfaces` would re-create the exact debt this note pays down.
   - **Decision:** inline `interface Foo { ... }` lowers as `Mir.imports["Foo"] = ImportedSpecMir { alias: "Foo", origin: ImportOrigin::Inline, interfaces: { "Foo" => ... }, upstream: None, account_types: vec![], records: vec![] }`. The interface name IS the namespace alias. Today's surface (`interface Token { ... }` + `call Token.transfer(...)`) already uses one identifier for both roles; the MIR shape just makes that explicit.

3. **`StateBinder.caller_projection` — `Path` or `Expr`?**
   - v2.27 Track A allows the binder RHS to be any expression (`state.pool.balance - state.pool.fees` is conceivable). Today it's restricted to a single dotted state path.
   - `Path` is honest about today's surface; `Expr` is honest about what the typed-codomain extension could grow into.
   - **Tentative:** `Path`. Tighten now; widen later if a fixture demands it. The conservative choice respects the [[feedback-keep-declarative]] preference.

4. **Do bundled stdlib qedspecs become user-facing examples?**
   - They're internal today (under `crates/qedgen/data/`). Under unification, they're "just qedspecs" — there's no reason a user couldn't author one of their own and have it ship the same way (Tier-2 third-party libraries).
   - **Tentative:** out of scope for this note. The unification doesn't open that door; it just stops closing it artificially.

## Non-goals — what NOT to fold into Phase 1c-7

- **v2.27 Track A's typed state-field codomain.** Already shipped; the new `Stmt::Cpi.state_binders` lifts the existing data, doesn't redesign it.
- **`cpi_substitute` MIR→MIR pass.** Per `qedgen-mir-sketch.md` §"Cross-cutting passes," this is Phase 3 work. The Phase 1c-7 emitter still calls `cpi_substitute::substitute_callee_ensures_lean` directly on opaque strings, same as legacy.
- **Lean sibling module renaming.** Cosmetic; touches the codegen output filename layout. Defer to a follow-up that owns the user-facing rename + migration.
- **Removing `ParsedSpec.interfaces` / `ParsedSpec.imported_namespaces`.** Parse-layer break. Defer to v3.0; the MIR layer collapse is non-breaking because `ParsedSpec` keeps its current shape.
- **A standalone `qedgen verify --check-upstream` rework.** Today this command walks `ParsedSpec.interfaces`; under unification it should walk `Mir.imports`. The port follows naturally from the new shape but isn't on the Phase 1c-7 critical path.

## Validation plan

How we'll know the new shape works:

1. **Existing bundled-stdlib fixtures regenerate identically** with `QEDGEN_USE_MIR=1`:
   - `examples/rust/escrow/escrow.qedspec` (imports `Token` from `"spl"`)
   - `examples/rust/lending/lending.qedspec` (imports `Token` from `"spl"`)
   - `examples/rust/bundled-stdlib-demo/pool.qedspec` (imports both `Token` and `System`)

2. **Inline `interface` fixtures regenerate identically.** Need to identify one in the corpus or add a minimal one. Candidates: the issue-8 pool fixture declares an inline `interface` block; verify.

3. **v2.27 Track A `state_binders` fixture regenerates identically.** Need to identify the canonical Track A test case — likely lives under `examples/rust/bundled-stdlib-demo/` since that's where Track A landed.

4. **New unit tests in `lean_gen_mir::tests`:**
   - `render_emits_cpi_transfer_envelope` — the `Stmt::TokenTransfer` half against an escrow-shaped fixture.
   - `render_emits_cpi_ensures_axiom_pinned` — the Tier-1/2 axiom application half against a `binary_hash`-pinned fixture.
   - `render_emits_cpi_ensures_sorry_unpinned` — the Tier-0 fallback shape.
   - `render_inline_interface_lowers_with_alias_as_name` — confirms inline `interface Foo { ... }` lowers to `Mir.imports["Foo"]` with `ImportOrigin::Inline` and the interface name doubling as the alias.

5. **Parse-layer change confirmation.** After dropping the `check.rs:1912` conditional:
   - `parsed.imported_namespaces` contains entries for every imported source, including SPL Token / System Program / Metaplex with `account_types: vec![]`.
   - Existing check.rs tests at lines 9169 / 9653 / 9895 / 9972 still pass (they assert on `parsed.interfaces`, not `imported_namespaces`).
   - No new `src/imported/<ns>.rs` files generated for empty-account_types entries (verified by walking `codegen.rs` Rust-mirror sites after Step 1's gate move).

6. **No lake-build regression on any bundled example.** The script `scripts/check-lake-build.sh --strict` is the gate; pre-Phase-1c-7 baseline locked.

## Cross-references

- [[feedback-unified-imports-semantics]] — the parent rule this note implements at the MIR layer.
- [[project-cpi-composition]] — Tier 0/1/2 mental model the unified shape makes derivable.
- [[feedback-keep-declarative]] — informs the `Path` vs `Expr` choice on `StateBinder`.
- `docs/design/qedgen-mir-sketch.md` §"Next-session handoff" — Phase 1c-7 suggested-first-move; this note answers the "design pass required" callout at §"What NOT to do without revisiting."
- `docs/design/spec-composition.md` — Tier 1/2/3 interface composition (the spec-layer doc the MIR shape implements).
- `crates/qedgen/src/check.rs:1887-1919` — the parse-time merge loop that fans imports into two buckets (the structural debt).
- `crates/qedgen/src/check.rs:1912` — the exact conditional Step 0 of the migration drops.
- `crates/qedgen/src/codegen.rs:1277, 5248` — the Rust-mirror sites Step 1 adds an explicit empty-skip gate to.
- `crates/qedgen/src/import_resolver.rs` — built-in key resolution (`"spl"`, `"system"`) and the `parse_imported_sources` entry that already produces a `ParsedSpec` per import.
- `crates/qedgen/data/interfaces/{spl_token,system,metaplex}.qedspec` — the bundled stdlib, already qedspec-shaped.
- `crates/qedgen/src/lean_gen.rs:1931-2150` — `render_cpi_theorems`, the emitter being ported.
