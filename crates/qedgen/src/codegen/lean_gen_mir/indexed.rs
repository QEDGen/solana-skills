use super::*;
use crate::obligations::{ObligationKind, ObligationRecorder, UnsupportedReason};

/// `(root_field, idx)` → `Vec<(inner_field, op_kind, value)>` — groups
/// multiple writes to the same `Map` slot into one `Function.update`.
/// The value is the typed MIR `Expr` (#151 Slice 2): rendering happens at
/// emission with the tree, not from the pre-rendered string.
pub(super) type IndexedEffectsByRoot<'a> =
    std::collections::BTreeMap<(String, String), Vec<(String, String, &'a crate::mir::Expr)>>;

/// Map a scalar DSL type string to its Lean type (record fields carry
/// string types, not the typed `Ty`).
pub(super) fn map_scalar_type(t: &str) -> String {
    match t.trim() {
        "U8" | "U16" | "U32" | "U64" | "U128" => "Nat".to_string(),
        "I8" | "I16" | "I32" | "I64" | "I128" => "Int".to_string(),
        "Bool" => "Bool".to_string(),
        "Pubkey" => "Pubkey".to_string(),
        other => other.to_string(),
    }
}

/// Default value for a record field's `Inhabited` instance.
pub(super) fn default_value_for(t: &str) -> &'static str {
    match t.trim() {
        "U8" | "U16" | "U32" | "U64" | "U128" => "0",
        "I8" | "I16" | "I32" | "I64" | "I128" => "0",
        "Bool" => "false",
        _ => "default",
    }
}

/// Indexed-state Lean renderer — fires when any state field is
/// `Ty::Map { .. }`. Needs Mathlib's `Fin`/`Function.update` machinery,
/// so the output shape diverges from the flat / ADT-State renderers:
///
///   * imports `Mathlib.Algebra.BigOperators.Fin` + the
///     `QEDGenMathlib.IndexedState` slice (defines `Map N α := Fin N → α`).
///   * emits `abbrev AccountIdx : Type := Fin <bound>` ahead of any
///     transition (the bound comes from the spec's first `MAX_*` const,
///     falling back to a literal).
///   * `Map[N] T` lowers to `Map N T` (space-separated; Lean parses
///     this as `Map` applied to two args).
///   * Map params are auto-promoted to `Fin N` at handler boundaries:
///     a `member_index : U8` declared in the spec becomes
///     `member_index : Fin MAX_MEMBERS` in Lean iff the handler reads
///     or writes `members[member_index]` or `voted[member_index]`.
///   * subscripted reads `state.members[i] = approver` rewrite to
///     `(s.members i) = approver`.
///   * subscripted writes `voted[i] := 1` lower to
///     `voted := Function.update s.voted i (1)`. Multiple writes to
///     the same `(root, idx)` pair collapse into one
///     `Function.update` with a `{ … with … }` payload.
///   * NO preservation theorems, NO aborts theorems, NO overflow
///     theorems, NO covers / liveness / environments — only the
///     property predicate `def`s land in `Spec.lean`. Proofs live
///     in a sibling `Proofs.lean` (qedgen init seeds it).
pub(super) fn render_indexed_state(mir: &Mir, rec: &mut ObligationRecorder) -> String {
    let mut out = String::new();

    // -- Imports --
    out.push_str("import Mathlib.Algebra.BigOperators.Fin\n");
    out.push_str("import QEDGen.Solana.Account\n");
    out.push_str("import QEDGenMathlib.IndexedState\n\n");

    // -- Namespace + opens --
    out.push_str(&format!("namespace {}\n\n", mir.name));
    out.push_str("open QEDGen.Solana\n");
    out.push_str("open QEDGen.Solana.IndexedState\n\n");

    // -- Uninterpreted helpers + ref_impls --
    emit_uninterpreted_helpers(&mut out, mir);
    emit_ref_impls(&mut out, mir);

    // -- Constants --
    for (name, val) in &mir.constants {
        out.push_str(&format!("abbrev {} : Nat := {}\n", safe_name(name), val));
    }
    if !mir.constants.is_empty() {
        out.push('\n');
    }

    // -- AccountIdx alias --
    let idx_bound = pick_account_idx_bound_mir(mir);
    out.push_str(&format!(
        "abbrev AccountIdx : Type := Fin {}\n\n",
        idx_bound
    ));

    // -- Record structures (e.g. Account) --
    //
    // Skip a record literally named "State": the `type State = { ... }`
    // record-form lowering deposits it into `mir.records` AND the State
    // variant; the dedicated `structure State where` emission below is
    // canonical, and emitting twice is a Lean `redeclaration of State`
    // error. This loop targets auxiliary records (Map value types).
    for rec in &mir.records {
        if rec.name == "State" {
            continue;
        }
        out.push_str(&format!("structure {} where\n", rec.name));
        for (fname, ftype) in &rec.fields {
            out.push_str(&format!(
                "  {} : {}\n",
                safe_name(fname),
                map_scalar_type(ftype)
            ));
        }
        out.push_str("  deriving Repr, DecidableEq, BEq\n\n");

        // Inhabited instance — zero-defaults. Needed for Map.set fallback.
        out.push_str(&format!(
            "instance : Inhabited {} := \u{27E8}{{\n",
            rec.name
        ));
        for (fname, ftype) in &rec.fields {
            out.push_str(&format!(
                "  {} := {},\n",
                safe_name(fname),
                default_value_for(ftype)
            ));
        }
        out.push_str("}\u{27E9}\n\n");
    }

    // -- Status inductive (lifecycle) --
    let lifecycle = &mir.state.lifecycle_states;
    let emit_marker = lifecycle.len() >= 2;
    if emit_marker {
        out.push_str("inductive Status where\n");
        for s in lifecycle {
            out.push_str(&format!("  | {}\n", s));
        }
        out.push_str("  deriving Repr, DecidableEq, BEq\n\n");
    }

    // -- State structure --
    //
    // Multi-variant ADT states with a single "active" variant project
    // that variant's fields into the State record; the variant tag is
    // recovered via the `status : Status` discriminator. Empty
    // variants (Uninitialized / HasProposal) contribute nothing
    // structural — their fields are inherited from the active variant
    // and gated by the `status` check inside transitions.
    let active_variant = mir
        .state
        .variants
        .iter()
        .find(|v| !v.fields.is_empty())
        .or_else(|| mir.state.variants.first());
    out.push_str("structure State where\n");
    if let Some(v) = active_variant {
        for f in &v.fields {
            out.push_str(&format!(
                "  {} : {}\n",
                safe_name(&f.name),
                render_ty_indexed(&f.ty)
            ));
        }
    }
    if emit_marker {
        out.push_str("  status : Status\n");
    }
    out.push('\n');

    // Collect map-field root names so transitions can detect indexed
    // effect LHSes via `parse_indexed_lhs`.
    let map_roots = collect_map_roots(mir);

    // -- Transitions --
    for h in &mir.handlers {
        emit_indexed_transition(&mut out, mir, h, &map_roots, emit_marker, rec);
    }

    // -- Operation inductive + applyOp --
    emit_indexed_operation_inductive(&mut out, mir, &map_roots);

    // -- Property predicate defs (no theorems).
    //
    // Indexed-state proofs need quantifier-aware Mathlib lemmas that
    // qedgen's auto-discharge templates don't cover; ship the
    // predicate `def`s as the spec-of-record and leave preservation
    // proofs to `Proofs.lean`.
    for prop in &mir.properties {
        if let Some(expr) = &prop.expression {
            let rewritten = expr_lean_app(expr);
            out.push_str(&format!(
                "/-- Property: {}. -/\ndef {} (s : State) : Prop :=\n  {}\n\n",
                prop.name,
                safe_name(&prop.name),
                rewritten
            ));
        }
    }

    out.push_str(&format!("end {}\n", mir.name));
    out
}

/// Render a MIR `Ty` in indexed-state form. Differs from
/// `render_ty` (single-account renderer) in that `Map { capacity,
/// value }` becomes `Map <cap> <inner>` (Lean function-application
/// shape) rather than the literal `Map[<cap>] <inner>` placeholder.
pub(super) fn render_ty_indexed(ty: &crate::mir::Ty) -> String {
    use crate::mir::Ty;
    match ty {
        Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::U128 => "Nat".to_string(),
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::I128 => "Int".to_string(),
        Ty::Bool => "Bool".to_string(),
        Ty::Pubkey => "Pubkey".to_string(),
        Ty::Bytes32 => "Bytes32".to_string(),
        Ty::Bytes64 => "Bytes64".to_string(),
        Ty::Fin { bound } => format!("Fin {}", bound),
        Ty::Vec { value } => format!("List {}", render_ty_indexed(value)),
        Ty::Option { value } => format!("Option {}", render_ty_indexed(value)),
        Ty::Custom(name) => name.clone(),
        Ty::Map { capacity, value } => {
            // The Map's inner type stays the literal surface type (e.g.
            // `U8`, not `Nat`) for the downstream Rust-side mirror.
            let inner = match value.as_ref() {
                Ty::U8 => "U8".to_string(),
                Ty::U16 => "U16".to_string(),
                Ty::U32 => "U32".to_string(),
                Ty::U64 => "U64".to_string(),
                Ty::U128 => "U128".to_string(),
                Ty::I8 => "I8".to_string(),
                Ty::I16 => "I16".to_string(),
                Ty::I32 => "I32".to_string(),
                Ty::I64 => "I64".to_string(),
                Ty::I128 => "I128".to_string(),
                Ty::Bool => "Bool".to_string(),
                Ty::Pubkey => "Pubkey".to_string(),
                Ty::Bytes32 => "Bytes32".to_string(),
                Ty::Bytes64 => "Bytes64".to_string(),
                Ty::Custom(n) => n.clone(),
                Ty::Fin { .. } | Ty::Vec { .. } | Ty::Option { .. } | Ty::Map { .. } => {
                    render_ty_indexed(value)
                }
            };
            format!("Map {} {}", capacity, inner)
        }
    }
}

/// Pick the constant bounding `AccountIdx`: first `MAX_*` constant, else
/// `MAX*`, else the literal `1024`. (The `type AccountIdx = Fin[N]` alias
/// path isn't lifted into MIR yet; add when a fixture needs it.)
pub(super) fn pick_account_idx_bound_mir(mir: &Mir) -> String {
    for (n, _) in &mir.constants {
        if n.starts_with("MAX_") && !n.contains("TVL") {
            return n.clone();
        }
    }
    for (n, _) in &mir.constants {
        if n.starts_with("MAX") {
            return n.clone();
        }
    }
    "1024".to_string()
}

/// Collect the set of state-field names whose type is `Ty::Map { .. }`.
/// Used by `parse_indexed_lhs`-style effect-LHS dispatch + by
/// `infer_idx_promotions_mir` to detect Fin-typed param promotions.
pub(super) fn collect_map_roots(mir: &Mir) -> std::collections::BTreeMap<String, String> {
    use crate::mir::Ty;
    let mut out = std::collections::BTreeMap::new();
    for v in &mir.state.variants {
        for f in &v.fields {
            if let Ty::Map { capacity, .. } = &f.ty {
                out.insert(f.name.clone(), capacity.clone());
            }
        }
    }
    out
}

/// Parse an indexed effect LHS (`voted[member_index]` or
/// `members[i].field`) into `(root, idx, inner_field)`. `inner_field` is
/// empty when the LHS targets the whole entry; `None` if no brackets.
pub(super) fn parse_indexed_lhs(lhs: &str) -> Option<(&str, &str, &str)> {
    let bracket = lhs.find('[')?;
    let root = &lhs[..bracket];
    let rest = &lhs[bracket + 1..];
    let close = rest.find(']')?;
    let idx = &rest[..close];
    let after = &rest[close + 1..];
    let inner_field = after.strip_prefix('.').unwrap_or(after);
    Some((root, idx, inner_field))
}

/// Infer Fin-bound promotions for a handler's scalar params used as
/// Map indexes.
pub(super) fn infer_idx_promotions_mir(
    h: &crate::mir::HandlerMir,
    map_roots: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    use crate::mir::{Stmt, Ty};
    let scalar_param_names: std::collections::BTreeSet<String> = h
        .params
        .iter()
        .filter(|(_, t)| matches!(t, Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::U128))
        .map(|(n, _)| n.clone())
        .collect();
    let mut result: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();

    let mut record = |idx: &str, root: &str| {
        if !scalar_param_names.contains(idx) {
            return;
        }
        if let Some(bound) = map_roots.get(root) {
            result
                .entry(idx.to_string())
                .or_insert_with(|| bound.clone());
        }
    };

    // Effect LHS — `voted[member_index] := …`, `members[i].field := …`.
    for stmt in &h.body.stmts {
        let lhs = match stmt {
            Stmt::Assign { path, .. }
            | Stmt::CheckedAdd { path, .. }
            | Stmt::CheckedSub { path, .. }
            | Stmt::WrapAdd { path, .. }
            | Stmt::WrapSub { path, .. }
            | Stmt::SatAdd { path, .. }
            | Stmt::SatSub { path, .. } => path.segments.first().cloned().unwrap_or_default(),
            Stmt::RequireOrAbort { .. }
            | Stmt::TokenTransfer { .. }
            | Stmt::VariantPromote { .. }
            | Stmt::Branch { .. }
            | Stmt::Cpi { .. }
            | Stmt::Emit { .. } => continue,
        };
        if let Some((root, idx, _)) = parse_indexed_lhs(&lhs) {
            record(idx, root);
        }
    }

    // Requires expressions — `state.members[member_index] = approver`,
    // etc. The expression carrier is opaque; scan raw Lean form for
    // `<path>[<idx>]` patterns.
    for pred in &h.pre {
        scan_indexed_in_expr(
            &expr_lean(&pred.0, tree_render::LeanCx::guard()),
            &mut record,
        );
    }
    for stmt in &h.body.stmts {
        if let Stmt::RequireOrAbort { pred, .. } = stmt {
            scan_indexed_in_expr(
                &expr_lean(&pred.0, tree_render::LeanCx::guard()),
                &mut record,
            );
        }
    }

    result
}

/// Walk `expr` for `<root>[<idx>]` patterns. `record` is invoked once per
/// match with the bare root identifier (last `.` segment) and the trimmed
/// index string.
pub(super) fn scan_indexed_in_expr(expr: &str, record: &mut dyn FnMut(&str, &str)) {
    let bytes = expr.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        let mut k = i;
        while k > 0 {
            let c = bytes[k - 1] as char;
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                k -= 1;
            } else {
                break;
            }
        }
        let path = &expr[k..i];
        let root = path.rsplit('.').next().unwrap_or(path);
        if let Some(close_rel) = expr[i + 1..].find(']') {
            let idx = expr[i + 1..i + 1 + close_rel].trim();
            if !idx.is_empty() && !root.is_empty() {
                record(idx, root);
            }
            i += close_rel + 2;
        } else {
            i += 1;
        }
    }
}

/// Emit one transition def for an indexed-state handler. Distinct
/// from `emit_handler_transition` (flat-state path) because:
///   * scalar param types lift to `Fin <bound>` when promoted;
///   * requires clauses are parenthesized as wholes;
///   * subscripted effects collapse into `Function.update` calls;
///   * NO auto over/under-flow guards (legacy behavior — indexed
///     transitions trust the surface DSL's bounds).
pub(super) fn emit_indexed_transition(
    out: &mut String,
    mir: &Mir,
    h: &crate::mir::HandlerMir,
    map_roots: &std::collections::BTreeMap<String, String>,
    emit_marker: bool,
    rec: &mut ObligationRecorder,
) {
    use crate::mir::Stmt;

    let trans_name = safe_name(&format!("{}Transition", h.name));
    let promotions = infer_idx_promotions_mir(h, map_roots);
    let param_sig = indexed_param_sig(&h.params, &promotions);

    // Guard conjuncts (no auto overflow/underflow guards — see fn doc).
    let mut conds: Vec<String> = Vec::new();
    let state_fields = flat_state_fields(mir);
    let auth_name = handler_auth_name(h);
    let who_is_state_field = auth_name
        .as_deref()
        .map(|w| state_fields.iter().any(|(n, _)| n == w))
        .unwrap_or(false);
    if let Some(who) = &auth_name {
        if who_is_state_field {
            conds.push(format!("signer = s.{}", safe_name(who)));
        }
    }
    if let Some((pre, _)) = &h.transition {
        if emit_marker {
            conds.push(format!("s.status = .{}", safe_name(pre)));
        }
    }
    // Requires clauses in ORIGINAL spec order via `requires_in_order`
    // (bare `requires X` interleaved with `requires X else Err`).
    // Iterating the split body-RequireOrAbort then `h.pre` instead would
    // reorder an interleaved sequence (e.g. match-arm-abort: bare arm
    // condition + error-carrying abort marker). Parenthesized as wholes;
    // subscript-rewritten so `state.members[i]` → `(s.members i)`.
    for (i, pred) in h.requires_in_order.iter().enumerate() {
        let lean = expr_lean_app(&pred.0);
        if mentions_handler_account_pubkey(&lean, &h.accounts) {
            rec.unsupported(
                ObligationKind::TransitionGuard,
                &h.name,
                &format!("req_{i}"),
                UnsupportedReason::LeanHandlerAccountPubkey,
            );
            continue;
        }
        conds.push(format!("({})", lean));
    }

    // Effect updates.
    let mut scalar_parts: Vec<String> = Vec::new();
    // (root, idx) → Vec<(inner_field, op_kind, value)>
    let mut indexed_by_root: IndexedEffectsByRoot = std::collections::BTreeMap::new();

    for stmt in &h.body.stmts {
        let (path, op_kind, val) = match stmt {
            Stmt::Assign { path, rhs } => (path, "set", rhs),
            Stmt::CheckedAdd { path, delta, .. }
            | Stmt::WrapAdd { path, delta }
            | Stmt::SatAdd { path, delta } => (path, "add", delta),
            Stmt::CheckedSub { path, delta, .. }
            | Stmt::WrapSub { path, delta }
            | Stmt::SatSub { path, delta } => (path, "sub", delta),
            Stmt::RequireOrAbort { .. }
            | Stmt::TokenTransfer { .. }
            | Stmt::VariantPromote { .. }
            | Stmt::Branch { .. }
            | Stmt::Cpi { .. }
            | Stmt::Emit { .. } => continue,
        };
        // Drop `<field> := <account_binding>.pubkey` — no Lean scope
        // for account-binding pubkey refs.
        if op_kind == "set" && is_account_pubkey_ref(&expr_lean(val, tree_render::LeanCx::guard()))
        {
            continue;
        }
        // Reconstruct the full dotted LHS: an indexed-record-field write
        // lowers to a multi-segment path (`accounts[i].active` →
        // `["accounts[i]", "active"]`). Using only the first segment would
        // drop `.active` and emit a whole-entry `Function.update … (val)`
        // instead of `{ (s.accounts i) with active := val }`, silently
        // losing / mis-typing the record-field write.
        let lhs = path.segments.join(".");
        if let Some((root, idx, inner_field)) = parse_indexed_lhs(&lhs) {
            if map_roots.contains_key(root) {
                indexed_by_root
                    .entry((root.to_string(), idx.to_string()))
                    .or_default()
                    .push((inner_field.to_string(), op_kind.to_string(), val));
                continue;
            }
        }
        // Plain scalar effect.
        let sf = safe_name(&lhs);
        let val_lean = effect_rhs_lean(val);
        match op_kind {
            "add" => scalar_parts.push(format!("{} := s.{} + {}", sf, sf, val_lean)),
            "sub" => scalar_parts.push(format!("{} := s.{} - {}", sf, sf, val_lean)),
            "set" => scalar_parts.push(format!("{} := {}", sf, val_lean)),
            _ => {}
        }
    }

    let mut with_parts = scalar_parts;
    for ((root, idx), ops) in &indexed_by_root {
        let whole_entry = ops.len() == 1 && ops[0].0.is_empty();
        let update = if whole_entry {
            let (_, _, value) = &ops[0];
            // Whole-entry writes never took the s.-prefix heuristic —
            // subscript rewriting only; the tree path renders directly.
            let val_lean = expr_lean_app(value);
            format!("Function.update s.{root} {idx} ({val})", val = val_lean)
        } else {
            let mut inner_updates: Vec<String> = Vec::new();
            for (fname, op_kind, value) in ops {
                let val_lean = effect_rhs_lean(value);
                let rhs = match op_kind.as_str() {
                    "add" => format!("(s.{root} {idx}).{fname} + {val_lean}"),
                    "sub" => format!("(s.{root} {idx}).{fname} - {val_lean}"),
                    _ => val_lean,
                };
                inner_updates.push(format!("{} := {}", fname, rhs));
            }
            format!(
                "Function.update s.{root} {idx} {{ (s.{root} {idx}) with {inners} }}",
                inners = inner_updates.join(", ")
            )
        };
        with_parts.push(format!("{} := {}", safe_name(root), update));
    }

    // Post-status update.
    if let Some((_, post)) = &h.transition {
        if emit_marker {
            with_parts.push(format!("status := .{}", safe_name(post)));
        }
    }

    let then_body = if with_parts.is_empty() {
        "some s".to_string()
    } else {
        format!("some {{ s with {} }}", with_parts.join(", "))
    };

    out.push_str(&format!(
        "def {} (s : State) (signer : Pubkey){} : Option State :=\n",
        trans_name, param_sig
    ));

    // Auth alias-let (only when `who` is not a state field).
    if let Some(who) = &auth_name {
        if !who_is_state_field {
            out.push_str(&format!("  let {} := signer\n", safe_name(who)));
        }
    }

    if conds.is_empty() {
        out.push_str(&format!("  {}\n\n", then_body));
    } else {
        out.push_str(&format!("  if {} then\n", conds.join(" \u{2227} ")));
        out.push_str(&format!("    {}\n", then_body));
        out.push_str("  else none\n\n");
    }
}

/// Render `param_sig_str`-equivalent with `Fin <bound>` promotion for
/// indexed-state handlers.
pub(super) fn indexed_param_sig(
    params: &[(crate::mir::Symbol, crate::mir::Ty)],
    promotions: &std::collections::BTreeMap<String, String>,
) -> String {
    if params.is_empty() {
        return String::new();
    }
    params
        .iter()
        .map(|(n, t)| {
            let lean_ty = if let Some(bound) = promotions.get(n) {
                format!("Fin {}", bound)
            } else {
                render_ty(t)
            };
            format!(" ({} : {})", n, lean_ty)
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Emit `inductive Operation where | ctor (params) …` + the `def applyOp`
/// dispatcher for the indexed-state shape (no `deriving` clause).
pub(super) fn emit_indexed_operation_inductive(
    out: &mut String,
    mir: &Mir,
    map_roots: &std::collections::BTreeMap<String, String>,
) {
    if mir.handlers.is_empty() {
        return;
    }
    out.push_str("inductive Operation where\n");
    for h in &mir.handlers {
        let promotions = infer_idx_promotions_mir(h, map_roots);
        let args: String = h
            .params
            .iter()
            .map(|(n, t)| {
                let lean_ty = if let Some(bound) = promotions.get(n) {
                    format!("Fin {}", bound)
                } else {
                    render_ty(t)
                };
                format!(" ({} : {})", n, lean_ty)
            })
            .collect();
        out.push_str(&format!("  | {}{}\n", safe_name(&h.name), args));
    }
    out.push('\n');

    out.push_str("def applyOp (s : State) (signer : Pubkey) : Operation \u{2192} Option State\n");
    for h in &mir.handlers {
        let binders: Vec<String> = h.params.iter().map(|(n, _)| n.clone()).collect();
        let bind_args = if binders.is_empty() {
            String::new()
        } else {
            format!(" {}", binders.join(" "))
        };
        out.push_str(&format!(
            "  | .{name}{bind} => {name}Transition s signer{bind}\n",
            name = safe_name(&h.name),
            bind = bind_args
        ));
    }
    out.push('\n');
}
