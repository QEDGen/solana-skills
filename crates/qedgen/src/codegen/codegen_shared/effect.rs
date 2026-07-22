use super::*;

/// Resolve the state-bearing account for a handler with a spec-wide
/// canonical fallback: `find_state_account` returns `None` when multiple
/// writable candidates exist and no PDA / `on_account` disambiguator
/// picks one (real-world specs often mark the state account `readonly`
/// in read-only handlers); fall back to the spec-wide canonical name and
/// re-find it in this handler.
pub(crate) fn resolve_handler_state_account<'a>(
    handler: &'a ParsedHandler,
    spec: &ParsedSpec,
) -> Option<&'a crate::check::ParsedHandlerAccount> {
    if let Some(direct) = find_state_account(handler) {
        return Some(direct);
    }
    let canon = find_canonical_state_account_name(spec)?;
    handler.accounts.iter().find(|a| a.name == canon)
}

/// Pick the most-likely state-bearing account name across the whole
/// spec. Heuristic over non-signer / non-program / non-token / non-mint
/// account names: compare `(writable-handler count, total-mention
/// count)` lexicographically, ties broken alphabetically for
/// determinism; `None` when nothing is ever writable. The total-mentions
/// tiebreaker keeps the heuristic robust for read-heavy programs where
/// the state account is `readonly` in most handlers.
pub(crate) fn find_canonical_state_account_name(spec: &ParsedSpec) -> Option<String> {
    let mut writable: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut total: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for h in &spec.handlers {
        for a in &h.accounts {
            if a.is_signer || a.is_program {
                continue;
            }
            if matches!(a.account_type.as_deref(), Some("token") | Some("mint")) {
                continue;
            }
            *total.entry(a.name.clone()).or_insert(0) += 1;
            if a.is_writable {
                *writable.entry(a.name.clone()).or_insert(0) += 1;
            }
        }
    }
    total
        .into_iter()
        .filter(|(name, _)| writable.get(name).copied().unwrap_or(0) > 0)
        .max_by(|(an, at), (bn, bt)| {
            let aw = writable.get(an).copied().unwrap_or(0);
            let bw = writable.get(bn).copied().unwrap_or(0);
            aw.cmp(&bw)
                .then_with(|| at.cmp(bt))
                .then_with(|| bn.cmp(an))
        })
        .map(|(name, _)| name)
}

/// Resolve a numeric / value argument's rust_expr to a form in scope
/// inside the handler `impl` block. Bare identifiers matching a state
/// field get the `self.<state_acct>.` prefix; handler params and
/// literals pass through. Multi-variant ADT fields route through the
/// `.inner.<field>()` accessor (the wrapper exposes only `inner`).
pub(crate) fn resolve_call_arg_for_amount(
    rust_expr: &str,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> String {
    // `s.<field>` (Ctx::Guard lowers `state` → `s`): CPI emission
    // contexts have no `s` binding, so route to the runtime
    // `self.<state_acct>.<field>` form — otherwise rustc rejects with
    // `cannot find value 's'`.
    if let Some(field) = rust_expr.strip_prefix("s.") {
        if field.chars().all(|c| c.is_alphanumeric() || c == '_') {
            if let Some(sa) = find_state_account(handler) {
                if is_multi_variant_adt_state(spec) {
                    if let Some(acct) = spec.account_types.first() {
                        let is_variant_field = acct
                            .variants
                            .iter()
                            .any(|v| v.fields.iter().any(|(n, _)| n == field));
                        if is_variant_field {
                            return format!("(*self.{}.inner.{}())", sa.name, field);
                        }
                    }
                }
                return format!("self.{}.{}", sa.name, field);
            }
        }
    }
    let is_simple_ident = rust_expr
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-');
    if !is_simple_ident {
        return rust_expr.to_string();
    }
    if handler.takes_params.iter().any(|(n, _)| n == rust_expr) {
        return rust_expr.to_string();
    }
    if let Some(sa) = find_state_account(handler) {
        if is_multi_variant_adt_state(spec) {
            // Variant-payload reads route through the accessor; bare form
            // when the field isn't a known variant payload (wrapper's own
            // fields like `bump` stay valid).
            if let Some(acct) = spec.account_types.first() {
                let is_variant_field = acct
                    .variants
                    .iter()
                    .any(|v| v.fields.iter().any(|(n, _)| n == rust_expr));
                if is_variant_field {
                    return format!("(*self.{}.inner.{}())", sa.name, rust_expr);
                }
            }
        }
        return format!("self.{}.{}", sa.name, rust_expr);
    }
    rust_expr.to_string()
}

/// Structural "is simple" gate + bare rendering for the mechanize paths
/// (#151 Slice 3) — the typed replacement for the char-whitelist test over
/// the pre-rendered value string. Admits exactly the shapes the whitelist
/// admitted (the adapter keeps simple values as bare strings): integer /
/// boolean literals, bare params / `let`-bound locals / abstract binders,
/// consts (substituted to their resolved value, mirroring `resolve_value`),
/// and single-segment state-field reads (rendered bare — the destructured
/// ADT path binds the field name as a local; `mechanize_effect` re-renders
/// with its `self.<acct>` receiver via `Binder::SelfAcct`). `None` for
/// every compound shape — the caller falls through to a `todo!()`.
pub(crate) fn tree_bare_rhs(tree: &crate::mir::ExprTree) -> Option<String> {
    use crate::mir::expr_tree::{BindingKind, ExprTree, TreeSeg};
    match tree {
        ExprTree::Int(v) => Some(v.to_string()),
        ExprTree::Bool(b) => Some(b.to_string()),
        ExprTree::Path(p) => match &p.binding {
            BindingKind::StateField | BindingKind::Ghost => match p.segments.as_slice() {
                [TreeSeg::Field(f)] => Some(f.clone()),
                _ => None,
            },
            BindingKind::Const(value) => p.segments.is_empty().then(|| value.clone()),
            BindingKind::External => None,
            BindingKind::Param
            | BindingKind::LetBound
            | BindingKind::Account
            | BindingKind::AbstractBinder
            | BindingKind::ExprBinder
            | BindingKind::Unresolved => p.segments.is_empty().then(|| p.root.clone()),
        },
        ExprTree::Old(_)
        | ExprTree::Sum { .. }
        | ExprTree::Quant { .. }
        | ExprTree::QuantIn { .. }
        | ExprTree::BoolOp { .. }
        | ExprTree::Not(_)
        | ExprTree::Cmp { .. }
        | ExprTree::Contains { .. }
        | ExprTree::Len(_)
        | ExprTree::Arith { .. }
        | ExprTree::MulDivFloor { .. }
        | ExprTree::MulDivCeil { .. }
        | ExprTree::MulDivRoundHalfUp { .. }
        | ExprTree::Match { .. }
        | ExprTree::Ctor { .. }
        | ExprTree::RecordLit { .. }
        | ExprTree::RecordUpdate { .. }
        | ExprTree::IsVariant { .. }
        | ExprTree::App { .. }
        | ExprTree::Field { .. }
        | ExprTree::Let { .. }
        | ExprTree::IfThenElse { .. } => None,
    }
}

/// Fields needing a `pre_<field>` snapshot in this handler's generated
/// body under PARALLEL effect semantics: an effect RHS that reads a
/// field the same block writes means the PRE-state value — the
/// semantics the Lean model's record update and the Kani conformance
/// `pre_<field>` assertions encode. Without the snapshot, spec-ordered
/// emission lets later reads observe earlier writes (the v2.43
/// read-after-write divergence: a sequential implementation stamped
/// verified against a parallel model).
pub(crate) fn parallel_pre_fields_for_handler(
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Vec<String> {
    let mut written: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut read: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for eff in &handler.effects {
        let stripped = strip_variant_prefix(&eff.field, spec);
        written.insert(strip_array_index_suffix(&stripped));
        crate::rust_codegen_util::state_field_reads(effect_tree(eff), &mut read);
    }
    written.intersection(&read).cloned().collect()
}

/// Account-bound RHS shapes that mean "this account's address": bare
/// `<acct>` and `<acct>.pubkey`. Returns the account binding name.
/// Distinct from [`tree_bare_rhs`] because the bare render is exactly
/// wrong here — the handler body has no free `<acct>` binding (E0425),
/// and `self.<acct>` is an account wrapper, not a `Pubkey` (E0308);
/// the caller must lower to the target's runtime key load instead.
pub(crate) fn account_key_rhs(tree: &crate::mir::ExprTree) -> Option<&str> {
    use crate::mir::expr_tree::{BindingKind, ExprTree, TreeSeg};
    let ExprTree::Path(p) = tree else { return None };
    if !matches!(p.binding, BindingKind::Account) {
        return None;
    }
    match p.segments.as_slice() {
        [] => Some(&p.root),
        [TreeSeg::Field(f)] if f == "pubkey" => Some(&p.root),
        _ => None,
    }
}

/// Is the effect's destination field declared `Pubkey`? Account-key
/// lowering (`field := <acct>`) is only sound when it does — assigning an
/// address into a `U64`/other slot (`pool : U64; pool := new_admin`)
/// would emit `self.state.pool = self.new_admin.key()`, an E0308
/// type mismatch. Resolves a variant-prefixed (`Active.admin`), bare
/// (`admin`), or flat state-field name across every declaration site;
/// conservative (`false` when the field isn't found or isn't `Pubkey`),
/// so a non-Pubkey / unknown destination falls back to the `todo!()`
/// path instead of a miscompile.
pub(crate) fn effect_dest_is_pubkey(
    field: &str,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> bool {
    // Precise path: handles flat `state_fields`, `on_account`
    // disambiguation for multi-account specs, and `Variant.field` forms.
    if crate::rust_codegen_util::field_type_is_pubkey(field, handler, spec) {
        return true;
    }
    // Bare field naming a multi-variant ADT payload: fields live in the
    // variants, not flat `state_fields`, so `field_type_is_pubkey`'s
    // flat lookup misses them. Scan variant payloads directly.
    if !field.contains('.') {
        let base = strip_array_index_suffix(field);
        return spec.account_types.iter().any(|a| {
            a.variants
                .iter()
                .any(|v| v.fields.iter().any(|(n, t)| *n == base && t == "Pubkey"))
        });
    }
    false
}

/// The runtime key load for an account binding inside a handler `impl`
/// body (`&mut self` receiver) — the `self.`-rooted sibling of
/// `Surface::account_key_expr`'s `ctx.` forms.
pub(crate) fn self_account_key_expr(target: Target, account_name: &str) -> String {
    match target {
        Target::Anchor => format!("self.{}.key()", account_name),
        Target::Quasar => format!("(*self.{}.to_account_view().address())", account_name),
        // pinocchio's AccountInfo::key() returns &Pubkey ([u8; 32]).
        Target::Pinocchio => format!("*self.{}.key()", account_name),
    }
}

/// Resolve the `(overflow, underflow)` error variants for a checked-arith
/// lowering site. Three-tiered:
///   1. per-site `or <Variant>` override (`on_error`),
///   2. `pragma checked_{over,under}flow_error =` default,
///   3. built-in `MathOverflow` / `MathUnderflow` — with the back-compat
///      fallback: `MathOverflow` declared without `MathUnderflow` keeps
///      `-=` raising `MathOverflow` so existing specs build.
pub(crate) fn checked_arith_error_variants<'a>(
    spec: &'a ParsedSpec,
    on_error: Option<&'a str>,
) -> (&'a str, &'a str) {
    let has_decl = |name: &str| spec.error_codes.iter().any(|c| c == name);
    let pragma_overflow = spec.pragma_value("checked_overflow_error");
    let pragma_underflow = spec.pragma_value("checked_underflow_error");
    let builtin_underflow = if has_decl("MathUnderflow") || !has_decl("MathOverflow") {
        "MathUnderflow"
    } else {
        "MathOverflow"
    };
    let overflow_variant = on_error.or(pragma_overflow).unwrap_or("MathOverflow");
    let underflow_variant = on_error.or(pragma_underflow).unwrap_or(builtin_underflow);
    (overflow_variant, underflow_variant)
}

/// Try to translate a single effect tuple to a real Rust statement. Returns
/// None when the RHS is too complex for mechanical expansion (match/arith/
/// pre-rendered Lean form); the caller falls through to a `todo!()` so an
/// LLM or human fills the body.
///
/// `effect.tree` is the typed RHS (always adapter-populated post-#151).
///
/// `effect.on_error` is the per-site override (`pool += amount or X`) for
/// the `checked_add` / `checked_sub` error variant; when `None`, fall back
/// to the `pragma checked_{over,under}flow_error =` default, then built-in
/// `MathOverflow` / `MathUnderflow`. Always `None` for non-checked ops.
pub(crate) fn mechanize_effect(
    effect: &crate::check::ParsedEffect,
    state_acct: &crate::check::ParsedHandlerAccount,
    spec: &ParsedSpec,
    target: Target,
    handler: &ParsedHandler,
    pre_fields: &[String],
) -> Option<String> {
    let (field, op_kind) = (&effect.field, &effect.op);
    let tree = effect_tree(effect);
    let on_error = effect.on_error.as_deref();

    // Account-bound RHS (`field := <acct>` / `<acct>.pubkey`): the spec
    // means the account's ADDRESS, so lower to the runtime key load —
    // the bare render `tree_bare_rhs` would produce is a miscompile
    // (see `account_key_rhs`). Two guards keep it sound:
    //   - set-only: arithmetic on a key (`pool += new_admin`) is a spec
    //     bug;
    //   - Pubkey destination only: `pool : U64; pool := new_admin` would
    //     assign a `[u8; 32]` into a scalar (E0308).
    // Either mismatch falls through to the `todo!()` path.
    let acct_key = account_key_rhs(tree);
    if acct_key.is_some() && (op_kind != "set" || !effect_dest_is_pubkey(field, handler, spec)) {
        return None;
    }
    // Refuse complex RHS — structural on the tree (#151 Slice 3). A simple
    // param / literal / constant / bare state-field read is what's always
    // safe.
    if acct_key.is_none() {
        tree_bare_rhs(tree)?;
    }
    // Multi-variant ADT state on Anchor: the wrapper carries only
    // `inner` + `bump`, so the flat `self.<acct>.<field>` lowering
    // doesn't apply. Bail so the per-effect path surfaces a `todo!()`
    // instead of a silent miscompile — `emit_variant_state_handler_body`
    // owns the cases it can mechanize.
    if is_multi_variant_adt_state(spec) && matches!(target, Target::Anchor) {
        return None;
    }
    // Single-variant ADT specs also accept the `Variant.field` LHS form;
    // the flat struct has no `Variant.` prefix, so strip it (otherwise
    // `Active.total_burned := X` lowers to a non-compiling path).
    let field = strip_variant_prefix(field, spec);
    let field = field.as_str();

    // Anchor / Quasar handler bodies bind state as `self.<acct>.<field>`,
    // so a bare state-field RHS (e.g. `bid_buyer := state.rfp_buyer` after
    // upstream strips `state.`) needs to resolve to `self.<acct>.rfp_buyer`.
    // One render under `Binder::SelfAcct` — name resolution already
    // happened at adapt time.
    let acct = &state_acct.name;
    let rhs = if let Some(account_name) = acct_key {
        self_account_key_expr(target, account_name)
    } else {
        use crate::rust_codegen_util::tree_render::{render_rust, Binder, RustCx};
        let receiver = format!("self.{}", acct);
        let rendered = render_rust(
            tree,
            RustCx::native().with_binder(Binder::SelfAcct(&receiver)),
        );
        // Parallel effect semantics: RHS reads of block-written fields
        // route through the `pre_<field>` snapshots the caller binds
        // before the effect lines (`parallel_pre_fields_for_handler`).
        // Type-neutral — `pre_<f>` has exactly the type of
        // `self.<acct>.<f>`, so the Quasar Pod arms are unaffected.
        crate::rust_codegen_util::substitute_pre_reads(&rendered, &receiver, pre_fields)
    };
    // Cast index expressions in the LHS path to `usize`. `render_effect`
    // emits the field as `voted[member_index]` (Lean-friendly); on the
    // Rust side, indexing `[u8; N]` with `u8`/`u16`/Fin fails — Rust
    // requires `usize`. Same shape as `path_to_rust`'s Index emission;
    // applied here so the Lean output stays untouched.
    let field = rewrite_index_to_usize(field);
    let field = field.as_str();
    // `+=` defaults to `checked_add(...).ok_or(err)?` (the pattern
    // deployed Anchor programs use); explicit `+=!` / `+=?` opt into
    // saturating / wrapping. Error-variant resolution is three-tiered:
    //   1. per-site `or <Variant>` override,
    //   2. `pragma checked_{over,under}flow_error =` default,
    //   3. built-in `MathOverflow` / `MathUnderflow`.
    let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
    let (overflow_variant, underflow_variant) = checked_arith_error_variants(spec, on_error);
    // Quasar's `#[account]` auto-wraps integer fields in Pod companions
    // (u64 → PodU64). Plain `=` / `wrapping_*` between u64 and PodU64
    // don't type-check, so on Quasar: `set` rhs gets `.into()`;
    // `checked_*` / `saturating_*` work as-is (PodU64 ships them);
    // `wrapping_*` unwinds to `<lhs>.get().wrapping_*(rhs).into()`.
    // Anchor uses native ints.
    let is_quasar = matches!(target, Target::Quasar);
    let line = match op_kind.as_str() {
        "set" => {
            if is_quasar {
                format!("        self.{}.{} = ({}).into();\n", acct, field, rhs)
            } else {
                format!("        self.{}.{} = {};\n", acct, field, rhs)
            }
        }
        "add" => format!(
            "        self.{acct}.{field} = self.{acct}.{field}.checked_add({rhs}).ok_or({err_enum}::{overflow_variant})?;\n"
        ),
        "add_sat" => format!(
            "        self.{acct}.{field} = self.{acct}.{field}.saturating_add({rhs});\n"
        ),
        "add_wrap" => {
            if is_quasar {
                format!(
                    "        self.{acct}.{field} = self.{acct}.{field}.get().wrapping_add({rhs}).into();\n"
                )
            } else {
                format!(
                    "        self.{acct}.{field} = self.{acct}.{field}.wrapping_add({rhs});\n"
                )
            }
        }
        "sub" => format!(
            "        self.{acct}.{field} = self.{acct}.{field}.checked_sub({rhs}).ok_or({err_enum}::{underflow_variant})?;\n"
        ),
        "sub_sat" => format!(
            "        self.{acct}.{field} = self.{acct}.{field}.saturating_sub({rhs});\n"
        ),
        "sub_wrap" => {
            if is_quasar {
                format!(
                    "        self.{acct}.{field} = self.{acct}.{field}.get().wrapping_sub({rhs}).into();\n"
                )
            } else {
                format!(
                    "        self.{acct}.{field} = self.{acct}.{field}.wrapping_sub({rhs});\n"
                )
            }
        }
        _ => return None,
    };
    Some(line)
}

/// Strip a leading `Variant.` prefix from an effect LHS when the root
/// matches a known variant (`Active.pool` → `pool`; `accounts[i].cap` /
/// `pool` unchanged). Pure normalization; the lint side validates.
pub(crate) fn strip_variant_prefix(lhs: &str, spec: &ParsedSpec) -> String {
    if let Some(dot) = lhs.find('.') {
        let head = &lhs[..dot];
        let is_variant = spec
            .account_types
            .iter()
            .any(|a| a.variants.iter().any(|v| v.name == head));
        if is_variant {
            return lhs[dot + 1..].to_string();
        }
    }
    lhs.to_string()
}

/// `mechanize_effect`'s sibling for the multi-variant ADT path: the
/// state binder is a destructured local (`pool: &mut u64` from
/// `match &mut self.<acct>.inner { Inner::Active { pool, .. } => …`),
/// so emit `*pool = …` / `pool[i] = …`. `None` for shapes the caller
/// routes to a per-effect `todo!()`.
pub(crate) fn mechanize_effect_destructured(
    effect: &crate::check::ParsedEffect,
    spec: &ParsedSpec,
    handler: &ParsedHandler,
    pre_fields: &[String],
) -> Option<String> {
    let (field_raw, op_kind) = (&effect.field, &effect.op);
    let on_error = effect.on_error.as_deref();
    // The destructured binding is the bare field root — `pool[i]` keeps
    // the indexing; scalars need a `*` deref to write through `&mut T`.
    let field = strip_variant_prefix(field_raw, spec);
    let field = rewrite_index_to_usize(&field);
    let is_indexed = field.contains('[');
    // The destructure binding shadows the field name in scope, so a
    // bare-identifier RHS that names a sibling state field resolves
    // directly (no `self.<acct>.` prefix) — `tree_bare_rhs` emits exactly
    // that shape (structural gate + render in one; #151 Slice 3).
    // Account-bound RHS lowers to the key load instead — the match on
    // `self.<state_acct>.inner` borrows a disjoint field, so reading
    // `self.<acct>.key()` inside the arm is legal. This emitter is
    // Anchor-only (see `emit_variant_state_handler_body`), so the
    // Anchor key form is the only one needed. Set-only + Pubkey
    // destination, same soundness gate as `mechanize_effect`.
    // Parallel semantics: a state-field read of a block-written field
    // routes to the `pre_<field>` local the caller binds at the top of
    // the match arm instead of the (already-mutated) destructured
    // binding.
    let rhs = if let Some(account_name) = account_key_rhs(effect_tree(effect)) {
        if op_kind != "set" || !effect_dest_is_pubkey(field_raw, handler, spec) {
            return None;
        }
        self_account_key_expr(Target::Anchor, account_name)
    } else {
        use crate::mir::expr_tree::{BindingKind, ExprTree, TreeSeg};
        let tree = effect_tree(effect);
        match tree {
            ExprTree::Path(p)
                if matches!(p.binding, BindingKind::StateField | BindingKind::Ghost)
                    && matches!(p.segments.as_slice(),
                        [TreeSeg::Field(f)] if pre_fields.iter().any(|pf| pf == f)) =>
            {
                match p.segments.as_slice() {
                    [TreeSeg::Field(f)] => format!("pre_{f}"),
                    _ => unreachable!("guarded by the match arm pattern"),
                }
            }
            _ => tree_bare_rhs(tree)?,
        }
    };

    let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
    let (overflow_variant, underflow_variant) = checked_arith_error_variants(spec, on_error);

    // Scalars need an explicit deref to write through `&mut T`;
    // indexed access through `&mut [T; N]` works as-is.
    let lhs = if is_indexed {
        field.clone()
    } else {
        format!("*{}", field)
    };
    // Method calls auto-deref `&mut T`, so the RHS-side read can
    // stay as the bare binding name even for scalar bindings.
    let read = strip_array_index_suffix(&field);

    let line = match op_kind.as_str() {
        "set" => format!("            {} = {};\n", lhs, rhs),
        "add" => format!(
            "            {} = {}.checked_add({}).ok_or({}::{})?;\n",
            lhs, read, rhs, err_enum, overflow_variant
        ),
        "add_sat" => format!("            {} = {}.saturating_add({});\n", lhs, read, rhs),
        "add_wrap" => format!("            {} = {}.wrapping_add({});\n", lhs, read, rhs),
        "sub" => format!(
            "            {} = {}.checked_sub({}).ok_or({}::{})?;\n",
            lhs, read, rhs, err_enum, underflow_variant
        ),
        "sub_sat" => format!("            {} = {}.saturating_sub({});\n", lhs, read, rhs),
        "sub_wrap" => format!("            {} = {}.wrapping_sub({});\n", lhs, read, rhs),
        _ => return None,
    };
    Some(line)
}

/// The typed RHS tree of an effect. Post-#151 every production
/// `ParsedEffect` is adapter-built with `tree: Some(...)`; a `None`
/// here is a hand-built fixture that must be fixed, not worked around.
pub(crate) fn effect_tree(effect: &crate::check::ParsedEffect) -> &crate::mir::ExprTree {
    effect
        .tree
        .as_ref()
        .expect("ParsedEffect.tree is always populated by the chumsky adapter (#151/#156)")
}

/// Drop `[<idx>]` from a destructured field reference so the RHS-side
/// read uses the bare binding name (`voted[i as usize]` → `voted`).
pub(crate) fn strip_array_index_suffix(field: &str) -> String {
    match field.find('[') {
        Some(i) => field[..i].to_string(),
        None => field.to_string(),
    }
}

/// Return shape of the multi-variant ADT handler-body emitter
/// (`emit_variant_state_handler_body`, below): wraps per-effect lines in
/// a destructure-and-mutate (same-variant) or destructure-and-promote
/// (cross-variant) match on `self.<acct>.inner`, with a `WrongState`
/// fallthrough arm. `needs_fill_tail` is true when at least one
/// `modifies` field landed as an agent-fill `todo!()` site — the caller
/// propagates it so the tail `todo!("fill non-mechanical …")` fires even
/// when every effect line mechanized cleanly.
pub(crate) struct VariantHandlerBody {
    pub(crate) body: String,
    pub(crate) needs_fill_tail: bool,
}

pub(crate) fn emit_variant_state_handler_body(
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    target: Target,
    state_acct: &crate::check::ParsedHandlerAccount,
) -> Option<VariantHandlerBody> {
    if !matches!(target, Target::Anchor) {
        return None;
    }
    if !is_multi_variant_adt_state(spec) {
        return None;
    }
    let pre = handler.pre_status.as_deref()?;
    let post = handler.post_status.as_deref()?;
    // WrongState is the error variant returned on a variant
    // mismatch. Demand it's declared so codegen doesn't emit a
    // dangling reference to an undeclared error variant.
    let has_wrong_state = spec.error_codes.iter().any(|c| c == "WrongState");
    if !has_wrong_state {
        return None;
    }

    let acct = spec.account_types.first()?;
    let post_variant = acct.variants.iter().find(|v| v.name == post)?;
    let inner_name = format!("{}AccountInner", to_pascal_case(&spec.program_name));
    let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
    let acct_binder = &state_acct.name;

    // Cross-variant promotion (init / promote handlers,
    // `pre != post`) routes through a separate emitter: destructure
    // the pre (if it carries payload) + assemble the post-variant
    // payload + assign `self.<acct>.inner = <Inner>::<Post>{ … }`.
    // Same-variant continues with the in-place match destructure
    // below.
    if pre != post {
        return emit_cross_variant_promotion(
            handler,
            spec,
            acct_binder,
            pre,
            post_variant,
            &inner_name,
            &err_enum,
        )
        .map(|body| VariantHandlerBody {
            body,
            needs_fill_tail: false,
        });
    }

    // Collect the unique set of bare field names referenced on the
    // LHS of any effect (after stripping `<Variant>.` prefix and
    // any `[…]` indexing). These go into the match destructure
    // pattern so the inner block can rebind them.
    let mut mutated_fields: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for eff in &handler.effects {
        let stripped = strip_variant_prefix(&eff.field, spec);
        let bare = strip_array_index_suffix(&stripped);
        // Only fields that actually live on the post variant get
        // destructured. References that don't match are a spec
        // bug — fall back so the per-effect path emits a clear
        // `todo!()` with the offending line surfaced verbatim.
        if !post_variant.fields.iter().any(|(n, _)| n == &bare) {
            return None;
        }
        mutated_fields.insert(bare);
    }

    // v2.26 Slice 2 — modifies-driven agent-fill on the ADT path.
    // Diff `modifies` against the effect-LHS set; any leftover field
    // becomes a `*field = todo!(...)` site inside the match arm,
    // with the relevant `ensures` clauses quoted as comments. The
    // destructure must bind the modifies-only field too, otherwise
    // the `*field = ...` reference doesn't resolve.
    let modifies_only_fields: Vec<String> = handler
        .modifies
        .as_ref()
        .map(|modifies| {
            modifies
                .iter()
                .filter(|f| {
                    !mutated_fields.contains(f.as_str())
                        && post_variant.fields.iter().any(|(n, _)| n == *f)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    for f in &modifies_only_fields {
        mutated_fields.insert(f.clone());
    }

    if mutated_fields.is_empty() {
        // Pure read-only handler (no effects on state, no modifies).
        // Emit a skip-style match so the runtime still rejects the
        // wrong variant — the guard layer doesn't catch variant
        // drift on its own.
        let mut out = String::new();
        out.push_str(&format!("        match self.{}.inner {{\n", acct_binder));
        out.push_str(&format!(
            "            {}::{} {{ .. }} => {{}}\n",
            inner_name, post
        ));
        out.push_str(&format!(
            "            _ => return Err({}::WrongState.into()),\n",
            err_enum
        ));
        out.push_str("        }\n");
        return Some(VariantHandlerBody {
            body: out,
            needs_fill_tail: false,
        });
    }

    let destructure = mutated_fields
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");

    let mut out = String::new();
    out.push_str(&format!(
        "        match &mut self.{}.inner {{\n",
        acct_binder
    ));
    out.push_str(&format!(
        "            {}::{} {{ {}, .. }} => {{\n",
        inner_name, post, destructure
    ));
    // Parallel effect semantics: snapshot block-written fields that some
    // RHS also reads, so those reads observe pre-state (the Lean-model
    // meaning) instead of whatever an earlier effect line already wrote.
    // RAW ⊆ written ⊆ destructured bindings, so `*<f>` always resolves.
    let pre_fields = parallel_pre_fields_for_handler(handler, spec);
    let lines: Vec<String> = handler
        .effects
        .iter()
        .map(|effect| mechanize_effect_destructured(effect, spec, handler, &pre_fields))
        .collect::<Option<Vec<_>>>()?;
    for f in &pre_fields {
        if lines.iter().any(|l| l.contains(&format!("pre_{f}"))) {
            out.push_str(&format!("                let pre_{f} = *{f};\n"));
        }
    }
    for line in &lines {
        out.push_str(line);
    }

    // Modifies-driven agent-fill sites inside the same match arm so the
    // destructured binding is in scope (mirrors the flat-fields template,
    // with `*field` deref instead of `self.X.field`).
    let mut needs_fill_tail = false;
    for field in &modifies_only_fields {
        let mut referencing: Vec<&str> = Vec::new();
        for e in &handler.ensures {
            if e.rust_expr.contains(field.as_str()) {
                referencing.push(e.rust_expr.as_str());
            }
        }
        out.push_str(&format!(
            "                // QED agent-fill site: `{}` is in `modifies` but not in `effect`.\n",
            field
        ));
        if referencing.is_empty() {
            out.push_str(&format!(
                "                //   No `ensures` clause references `{}` — the field is\n",
                field
            ));
            out.push_str(
                "                //   unconstrained. Either add an `ensures` constraint or\n",
            );
            out.push_str(&format!(
                "                //   remove `{}` from `modifies`. (Lint: unconstrained_modifies)\n",
                field
            ));
        } else {
            out.push_str("                //   Implement against the spec's ensures:\n");
            for r in &referencing {
                out.push_str(&format!("                //     ensures {}\n", r));
            }
            out.push_str(
                "                //   The Kani / proptest harness verifies the impl satisfies\n",
            );
            out.push_str(
                "                //   these clauses against the pre-state captured before the call.\n",
            );
        }
        out.push_str(&format!(
            "                *{} = todo!(\"compute {} to satisfy ensures above\");\n",
            field, field
        ));
        needs_fill_tail = true;
    }

    out.push_str("            }\n");
    out.push_str(&format!(
        "            _ => return Err({}::WrongState.into()),\n",
        err_enum
    ));
    out.push_str("        }\n");
    Some(VariantHandlerBody {
        body: out,
        needs_fill_tail,
    })
}

/// Render an effect RHS for the cross-variant promotion emitter. Legal
/// shapes (deterministically lowerable only):
///
///   - account address (`<account>` / `<account>.pubkey`) →
///     `self.<account>.key()` — resolved off the typed `ExprTree` via
///     [`account_key_rhs`], so the bare and `.pubkey` spellings lower
///     identically; only fires into a `Pubkey` destination field
///     (`dest_is_pubkey`)
///   - bare param / bare const / integer literal → bare identifier
///   - bare pre-variant field name → bare identifier (the destructure
///     preamble binds it as a local)
///
/// Anything else returns `None`, bailing the whole cross-variant emitter
/// back to the per-effect `todo!()` path instead of a silent miscompile.
pub(crate) fn resolve_cross_variant_rhs(
    eff: &crate::check::ParsedEffect,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    pre_fields: &[String],
    dest_is_pubkey: bool,
) -> Option<String> {
    // Account address (bare `<acct>` or `<acct>.pubkey`): resolved off
    // the binding, so both spellings unify. Only sound when the post
    // field is `Pubkey` — the same gate `mechanize_effect` applies (an
    // address into a scalar is E0308).
    if let Some(tree) = eff.tree.as_ref() {
        if let Some(account_name) = account_key_rhs(tree) {
            if !dest_is_pubkey {
                return None;
            }
            // Anchor-only emitter (cross-variant lives on the Anchor
            // path), so the Anchor key form is the only one needed.
            return Some(self_account_key_expr(Target::Anchor, account_name));
        }
    }

    let raw = eff.value.as_str();
    if raw.is_empty() {
        return None;
    }
    // Integer literal — render_effect emits `Expr::Int(v)` as its
    // decimal form.
    if raw.chars().all(|c| c.is_ascii_digit() || c == '-') {
        return Some(raw.to_string());
    }
    // Bare param or const — no dot, no bracket, matches a known name.
    if !raw.contains('.') && !raw.contains('[') {
        if handler.takes_params.iter().any(|(n, _)| n == raw) {
            return Some(raw.to_string());
        }
        if spec.constants.iter().any(|(n, _)| n == raw) {
            return Some(raw.to_string());
        }
        // Pre-variant field: the destructure preamble binds it as a
        // local, so a bare reference resolves (render_effect strips
        // `state.` for bare paths).
        if pre_fields.iter().any(|n| n == raw) {
            return Some(raw.to_string());
        }
        // Unknown bare ident — possibly a param shadowed by a state
        // field; bail rather than guess.
        return None;
    }
    None
}

/// Emit cross-variant promotion (init / promote) for a multi-variant ADT
/// state: assembles every post-variant field from the handler's effect
/// lines and assigns `self.<acct>.inner = <Inner>::<Post> { … };`.
///
/// Bail-outs (each falls back to the per-effect `todo!()` path): a
/// post-variant field with no matching effect line (we don't guess
/// defaults), or an effect RHS `resolve_cross_variant_rhs` can't lower.
pub(crate) fn emit_cross_variant_promotion(
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    acct_binder: &str,
    pre: &str,
    post_variant: &crate::check::ParsedVariant,
    inner_name: &str,
    err_enum: &str,
) -> Option<String> {
    // All four promotion patterns supported; payload→payload emits a
    // destructure preamble capturing the pre fields referenced by post
    // RHSs as locals.
    let pre_variant = spec
        .account_types
        .first()?
        .variants
        .iter()
        .find(|v| v.name == pre)?;

    let pre_field_names: Vec<String> = pre_variant.fields.iter().map(|(n, _)| n.clone()).collect();

    // {field → RHS-rust} from the handler's effects. Cross-variant only
    // accepts `:=` ("set"); checked-arith makes no sense in a promotion
    // (no pre value to read).
    let mut field_rhs: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for eff in &handler.effects {
        // Cross-variant only accepts `:=` ("set").
        if eff.op != "set" {
            return None;
        }
        let stripped = strip_variant_prefix(&eff.field, spec);
        let bare = strip_array_index_suffix(&stripped);
        // Field must live on the post variant.
        let (_, dest_ty) = post_variant.fields.iter().find(|(n, _)| n == &bare)?;
        let dest_is_pubkey = dest_ty == "Pubkey";
        let resolved =
            resolve_cross_variant_rhs(eff, handler, spec, &pre_field_names, dest_is_pubkey)?;
        field_rhs.insert(bare, resolved);
    }

    // Every post-variant field must have an RHS — silent defaults hide
    // bugs (a zeroed pubkey is a famously bad one). Unit-style post
    // variants iterate zero times.
    for (fname, _) in &post_variant.fields {
        if !field_rhs.contains_key(fname) {
            return None;
        }
    }

    // Only pre fields referenced by a post RHS get bound by the
    // destructure preamble; binding the rest would warn as unused.
    let referenced_pre_fields: Vec<String> = pre_field_names
        .iter()
        .filter(|f| field_rhs.values().any(|rhs| rhs == *f))
        .cloned()
        .collect();

    let is_init_pre = matches!(pre, "Uninitialized" | "Empty");
    let mut out = String::new();

    if pre_variant.fields.is_empty() {
        // Unit pre — bare matches! gate. Init handlers skip it:
        // `#[account(init, …)]` zeroes the account.
        if !is_init_pre {
            out.push_str(&format!(
                "        if !matches!(self.{}.inner, {}::{}) {{ return Err({}::WrongState.into()); }}\n",
                acct_binder, inner_name, pre, err_enum
            ));
        }
    } else if referenced_pre_fields.is_empty() {
        // Payload pre, no fields read by post — variant check only.
        if !is_init_pre {
            out.push_str(&format!(
                "        if !matches!(self.{}.inner, {}::{} {{ .. }}) {{ return Err({}::WrongState.into()); }}\n",
                acct_binder, inner_name, pre, err_enum
            ));
        }
    } else {
        // Payload pre with fields read by post — one `match` doubles as
        // variant-gate and field capture (referenced fields only, rest
        // via `..`). The inner enum derives Clone, so `.clone()` per
        // field works for Copy and non-Copy types alike.
        let bind_pat = referenced_pre_fields.join(", ");
        let local_pat = if referenced_pre_fields.len() == 1 {
            referenced_pre_fields[0].clone()
        } else {
            format!("({})", bind_pat)
        };
        let tuple_expr = if referenced_pre_fields.len() == 1 {
            format!("{}.clone()", referenced_pre_fields[0])
        } else {
            let parts: Vec<String> = referenced_pre_fields
                .iter()
                .map(|n| format!("{}.clone()", n))
                .collect();
            format!("({})", parts.join(", "))
        };
        out.push_str(&format!(
            "        let {} = match &self.{}.inner {{\n",
            local_pat, acct_binder
        ));
        out.push_str(&format!(
            "            {}::{} {{ {}, .. }} => {},\n",
            inner_name, pre, bind_pat, tuple_expr
        ));
        out.push_str(&format!(
            "            _ => return Err({}::WrongState.into()),\n",
            err_enum
        ));
        out.push_str("        };\n");
    }

    // Unit post: bare constructor (no `{}`); payload post: field-by-field
    // initializer.
    if post_variant.fields.is_empty() {
        out.push_str(&format!(
            "        self.{}.inner = {}::{};\n",
            acct_binder, inner_name, post_variant.name
        ));
    } else {
        out.push_str(&format!(
            "        self.{}.inner = {}::{} {{\n",
            acct_binder, inner_name, post_variant.name
        ));
        // Iterate the variant's `fields` (not the name-sorted map) to
        // preserve source-declared order.
        for (fname, _) in &post_variant.fields {
            let rhs = &field_rhs[fname];
            out.push_str(&format!("            {}: {},\n", fname, rhs));
        }
        out.push_str("        };\n");
    }
    Some(out)
}

/// Emit a destructure-then-compare auth guard for a handler whose
/// `auth X` field lives in a variant payload. Empty string when the
/// conditions don't apply (single-variant state, no `auth X`, X on the
/// wrapper, no matching signer account, or `Unauthorized` undeclared).
/// Fires after the lifecycle pre-check, so the destructure is
/// guaranteed to bind.
pub(crate) fn emit_variant_auth_guard(
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    target: Target,
) -> String {
    // Anchor-only: Quasar keeps the flat-struct shape (no inner enum to
    // destructure) and keeps firing `has_one` instead — the check.rs
    // `has_one` suppression is target-gated to match.
    if !matches!(target, Target::Anchor) {
        return String::new();
    }
    let Some(ref who) = handler.who else {
        return String::new();
    };
    if !is_multi_variant_adt_with_field_in_variant(spec, who) {
        return String::new();
    }
    let Some(ref pre) = handler.pre_status else {
        return String::new();
    };
    if matches!(pre.as_str(), "Uninitialized" | "Empty") {
        return String::new();
    }
    let Some(acct) = spec.account_types.first() else {
        return String::new();
    };
    let Some(variant) = acct.variants.iter().find(|v| v.name == *pre) else {
        return String::new();
    };
    if !variant.fields.iter().any(|(n, _)| n == who) {
        return String::new();
    }
    // Need a signer account in the handler with the same name as
    // the auth field, so the auth comparison can resolve
    // `ctx.<signer>.key()` without renaming dance.
    let Some(signer_acct) = handler
        .accounts
        .iter()
        .find(|a| a.is_signer && a.name == *who)
    else {
        return String::new();
    };
    if !spec.error_codes.iter().any(|c| c == "Unauthorized") {
        return String::new();
    }
    let Some(state_acct) = find_state_account(handler) else {
        return String::new();
    };

    let inner_name = format!("{}AccountInner", to_pascal_case(&spec.program_name));
    let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
    // Pin the auth-field destructure binding to a stable local name
    // (`auth_field`) to avoid shadowing param names that might match
    // the field's name in user code. The rest of the variant payload
    // gets ignored via `..`.
    format!(
        "    // auth: variant payload {pre}.{who} == ctx.{signer}.key()\n\
         \x20   let {inner}::{pre} {{ {who}: auth_field, .. }} = &ctx.{acct}.inner\n\
         \x20   else {{ return Err({err}::InvalidLifecycle.into()); }};\n\
         \x20   if auth_field != &ctx.{signer}.key() {{ return Err({err}::Unauthorized.into()); }}\n",
        inner = inner_name,
        pre = pre,
        who = who,
        signer = signer_acct.name,
        acct = state_acct.name,
        err = err_enum,
    )
}
