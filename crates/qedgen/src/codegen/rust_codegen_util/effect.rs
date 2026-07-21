//! Account-environment detection, value/state resolution, and effect-triple
//! lowering: the `(field, op_kind, value)` projection of MIR `Stmt`s and the
//! single-effect Rust emitters consumed by the transition bodies.

use super::*;

pub fn handler_needs_account_env(op: &ParsedHandler) -> bool {
    op.requires
        .iter()
        .any(|r| mentions_handler_account_pubkey(&r.rust_expr, &op.accounts))
        || op
            .effects
            .iter()
            .any(|e| is_account_pubkey_ref(e.value.trim(), &op.accounts))
        || op.effect_branches.as_ref().is_some_and(|branches| {
            branches.arms.iter().any(|arm| {
                arm.effects
                    .iter()
                    .any(|e| is_account_pubkey_ref(e.value.trim(), &op.accounts))
            })
        })
}

pub fn handler_account_env_struct_name(op_name: &str) -> String {
    let sanitized = crate::codegen_shared::sanitize_ident(op_name);
    let mut out = String::new();
    for part in sanitized.split('_').filter(|p| !p.is_empty()) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if out.is_empty() {
        "Handler".to_string()
    } else {
        format!("{}Accounts", out)
    }
}

/// The typed tree of a MIR expression. Post-#151 every production effect
/// RHS carries a tree copied from the adapter-built `ParsedEffect.tree`;
/// a `None` here is a hand-built fixture that must be fixed, not worked
/// around.
/// Rust form of a MIR expression — tree render under the native context
/// when the tree is present (#156 sweep), the pre-rendered string
/// otherwise (from_raw carriers, hand-built fixtures).
pub fn mir_expr_rust(e: &crate::mir::Expr) -> String {
    match &e.tree {
        Some(t) => super::tree_render::render_rust(t, super::tree_render::RustCx::native()),
        None => String::new(),
    }
}

pub fn mir_expr_tree(e: &crate::mir::Expr) -> &crate::mir::ExprTree {
    e.tree
        .as_ref()
        .expect("effect-RHS Expr.tree is always populated by the chumsky adapter (#151/#156)")
}

/// Rust scalar type of a flat state field, for annotating the checked-RHS
/// `Option` closure. `None` for indexed / dotted / non-scalar targets —
/// callers fall back to unannotated inference.
fn field_rust_scalar_ty(spec: &ParsedSpec, field: &str) -> Option<&'static str> {
    if field.contains('[') || field.contains('.') {
        return None;
    }
    let dsl_ty = spec
        .state_fields
        .iter()
        .chain(spec.account_types.iter().flat_map(|a| a.fields.iter()))
        .find(|(n, _)| n == field)
        .map(|(_, t)| t.as_str())?;
    match dsl_ty.trim() {
        "U8" => Some("u8"),
        "U16" => Some("u16"),
        "U32" => Some("u32"),
        "U64" => Some("u64"),
        "U128" => Some("u128"),
        "I64" => Some("i64"),
        "I128" => Some("i128"),
        _ => None,
    }
}

// ============================================================================
// Shared helpers — used by kani, proptest, unit_test, integration generators
// ============================================================================

/// Resolve state fields for the spec, handling multi-account layout.
/// Returns the fields for the primary account type.
pub fn resolve_state_fields(spec: &ParsedSpec) -> &[(String, String)] {
    if spec.account_types.len() > 1 {
        &spec.account_types[0].fields
    } else {
        &spec.state_fields
    }
}

/// Borrow every declared state field (was `mutable_fields` — the "mutable"
/// naming was historical; Pubkey fields are first-class since they lower to
/// `[u8; 32]`, so nothing is filtered).
pub fn field_refs(fields: &[(String, String)]) -> Vec<&(String, String)> {
    fields.iter().collect()
}

/// The base field name an effect targets. `accounts[i].active` → `accounts`;
/// `foo.bar` → `foo`; `plain` → `plain`. Used by `check_effect_targets` to
/// look up the target in the declared state schema.
pub fn effect_target_base(path: &str) -> &str {
    let path = path.trim();
    let end = path.find(['[', '.']).unwrap_or(path.len());
    &path[..end]
}

/// Strip a leading `<Variant>.` prefix when the root names a multi-variant
/// ADT variant; unchanged otherwise. Harness `State` models carry fields in
/// union form, so `Active.balance := …` must lower to `s.balance = …`.
/// Owned return so callers can pass through `&str`-only APIs.
pub fn strip_variant_prefix_for_flat_state(path: &str, spec: &ParsedSpec) -> String {
    if let Some(dot) = path.find('.') {
        let head = &path[..dot];
        let is_variant = spec
            .account_types
            .iter()
            .any(|a| a.variants.iter().any(|v| v.name == head));
        if is_variant {
            return path[dot + 1..].to_string();
        }
    }
    path.to_string()
}

/// Source spelling of a MIR target path. This is the compatibility view for
/// schema lookups and diagnostics; Rust emission must use
/// [`render_effect_target`] so structured subscripts reach the canonical
/// precedence-aware renderer.
pub fn effect_path_source(path: &crate::mir::Path) -> String {
    path.segments.join(".")
}

/// Render an effect target with its runtime receiver. Adapter-built paths
/// use the same canonical renderer as every other Rust expression, including
/// structural `usize` casts for index segments. The fallback is only for
/// legacy/fixture paths that predate the typed carrier.
pub fn render_effect_target(path: &crate::mir::Path, spec: &ParsedSpec, receiver: &str) -> String {
    use crate::mir::expr_tree::{BindingKind, TreeSeg};
    if let Some(mut tree) = path.tree.clone() {
        let root_is_variant = spec
            .account_types
            .iter()
            .any(|a| a.variants.iter().any(|v| v.name == tree.root));
        if root_is_variant {
            tree.root = "state".to_string();
            tree.binding = BindingKind::StateField;
        }
        if matches!(tree.binding, BindingKind::StateField | BindingKind::Ghost) {
            if let Some(TreeSeg::Field(first)) = tree.segments.first() {
                let is_variant = spec
                    .account_types
                    .iter()
                    .any(|a| a.variants.iter().any(|v| v.name == *first));
                if is_variant {
                    tree.segments.remove(0);
                }
            }
        }
        return super::tree_render::render_rust(
            &crate::mir::ExprTree::Path(tree),
            super::tree_render::RustCx::native()
                .with_binder(super::tree_render::Binder::SelfAcct(receiver)),
        );
    }
    let flat = strip_variant_prefix_for_flat_state(&effect_path_source(path), spec);
    format!("{receiver}.{}", cast_subscripts(&flat))
}

/// Project an effect-shaped MIR `Stmt` back onto the `(field, op_kind,
/// value)` triple the string templates below consume — the #66 adaptor
/// that makes `Stmt` the iteration source for the Kani/proptest transition
/// bodies while keeping output byte-identical (`lower_body` preserves spec
/// order, `Path` round-trips the dotted field, `Expr::from_raw` carries
/// the RHS verbatim in `.rust`).
///
/// Returns `None` for every non-effect variant — each with the reason it
/// renders as *nothing* in the pure spec-model transition. The match is
/// exhaustive by discipline (no `_` arm; see the `Stmt` enum doc): a new
/// `Stmt` variant is a compile error here, forcing an explicit decision
/// for the Kani/proptest backends.
pub fn stmt_effect_triple(
    stmt: &crate::mir::Stmt,
) -> Option<(&crate::mir::Path, &'static str, &crate::mir::Expr)> {
    use crate::mir::Stmt;
    match stmt {
        Stmt::Assign { path, rhs } => Some((path, "set", rhs)),
        Stmt::CheckedAdd { path, delta, .. } => {
            // The lowered per-site error name is Lean/scaffold surface;
            // the pure model signals overflow via `return false`.
            Some((path, "add", delta))
        }
        Stmt::CheckedSub { path, delta, .. } => Some((path, "sub", delta)),
        Stmt::SatAdd { path, delta } => Some((path, "add_sat", delta)),
        Stmt::SatSub { path, delta } => Some((path, "sub_sat", delta)),
        Stmt::WrapAdd { path, delta } => Some((path, "add_wrap", delta)),
        Stmt::WrapSub { path, delta } => Some((path, "sub_wrap", delta)),
        // Guard surface: `requires X else Err` folds into the guard
        // conjunction via `collect_full_guard*`, not the body.
        Stmt::RequireOrAbort { .. } => None,
        // CPI surface: no state mutation in the pure model; the Kani
        // CPI-fact harnesses read the call sites separately.
        Stmt::TokenTransfer { .. } => None,
        Stmt::Cpi { .. } => None,
        // Lifecycle surface: the transition body drives variant changes
        // through the pre/post-status writes, not a promote statement.
        Stmt::VariantPromote { .. } => None,
        // Branch arms are rendered by the per-arm match path; walking
        // them here would double-emit.
        Stmt::Branch { .. } => None,
        // Events: auxiliary, no state mutation in the pure model.
        Stmt::Emit { .. } => None,
    }
}

/// All effect triples of a lowered handler body, in spec order. The
/// shared iteration source for the Kani/proptest transition emitters,
/// conformance harnesses, and overflow filters. Top-level only —
/// `Stmt::Branch` arms are NOT descended (per-arm rendering owns
/// those); use [`block_effect_triples_deep`] for analyses that must
/// see conditionally-applied effects.
pub fn block_effect_triples(
    body: &crate::mir::Block,
) -> Vec<(&crate::mir::Path, &'static str, &crate::mir::Expr)> {
    body.stmts.iter().filter_map(stmt_effect_triple).collect()
}

/// Like [`block_effect_triples`] but descends into `Stmt::Branch`
/// arms/default. For *may-this-effect-fire* analyses (overflow filters
/// and tests) where a conditionally-applied effect counts.
pub fn block_effect_triples_deep(
    body: &crate::mir::Block,
) -> Vec<(&crate::mir::Path, &'static str, &crate::mir::Expr)> {
    fn walk<'a>(
        stmts: &'a [crate::mir::Stmt],
        out: &mut Vec<(&'a crate::mir::Path, &'static str, &'a crate::mir::Expr)>,
    ) {
        for s in stmts {
            if let Some(t) = stmt_effect_triple(s) {
                out.push(t);
            }
            if let crate::mir::Stmt::Branch { arms, default, .. } = s {
                for a in arms {
                    walk(&a.block.stmts, out);
                }
                if let Some(d) = default {
                    walk(&d.stmts, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&body.stmts, &mut out);
    out
}

/// State fields read anywhere in `tree` (StateField / Ghost bindings,
/// first field segment). The read side of the parallel-semantics
/// snapshot computation below.
pub fn state_field_reads(
    tree: &crate::mir::ExprTree,
    out: &mut std::collections::BTreeSet<String>,
) {
    use super::tree_render::for_each_path;
    use crate::mir::expr_tree::{BindingKind, TreeSeg};
    for_each_path(tree, &mut |p| {
        if matches!(p.binding, BindingKind::StateField | BindingKind::Ghost) {
            if let Some(TreeSeg::Field(f)) = p.segments.first() {
                out.insert(f.clone());
            }
        }
    });
}

/// Fields that need a `pre_<field>` snapshot under the spec's PARALLEL
/// effect semantics: every RHS state read in an `effect { … }` block sees
/// the PRE-state value — the semantics the Lean model's record update
/// (`{ s with a := …, b := s.a }`) and the Kani conformance harnesses'
/// `pre_<field>` assertions already encode. A field needs a snapshot when
/// the block both writes it (any effect target) and reads it (any effect
/// RHS); emitting the transition statements in spec order would otherwise
/// let later reads observe earlier writes (the v2.43 read-after-write
/// soundness divergence — a sequential implementation verified against a
/// parallel model).
///
/// Takes the exact triple stream the caller will emit (post pubkey-skip
/// filtering), so every snapshot is referenced by an emitted statement.
pub fn parallel_snapshot_fields(
    triples: &[(&crate::mir::Path, &'static str, &crate::mir::Expr)],
    spec: &ParsedSpec,
) -> Vec<String> {
    let mut written: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut read: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (field, _, value) in triples {
        let flat = strip_variant_prefix_for_flat_state(&effect_path_source(field), spec);
        written.insert(effect_target_base(&flat).to_string());
        state_field_reads(mir_expr_tree(value), &mut read);
    }
    written.intersection(&read).cloned().collect()
}

/// Boundary-safe rewrite of `<receiver>.<field>` reads to `pre_<field>`
/// in a rendered Rust expression — the substitution half of the parallel
/// snapshot. Token boundaries on both sides: `accounts.balance` and
/// `s.balance_total` survive a `balance` rewrite.
pub fn substitute_pre_reads(expr: &str, receiver: &str, fields: &[String]) -> String {
    let mut out = expr.to_string();
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    for f in fields {
        let needle = format!("{receiver}.{f}");
        let replacement = format!("pre_{f}");
        let mut rebuilt = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(i) = rest.find(&needle) {
            let before_ok = i == 0 || !rest[..i].chars().next_back().is_some_and(is_ident);
            let after = &rest[i + needle.len()..];
            let after_ok = !after.chars().next().is_some_and(is_ident);
            rebuilt.push_str(&rest[..i]);
            if before_ok && after_ok {
                rebuilt.push_str(&replacement);
            } else {
                rebuilt.push_str(&needle);
            }
            rest = after;
        }
        rebuilt.push_str(rest);
        out = rebuilt;
    }
    out
}

/// Rewrite `[ident]` subscripts in a flattened effect-target path to
/// `[ident as usize]` — harness params bind at their spec types (`u8`
/// etc.) while Rust arrays index by `usize`. Numeric and compound
/// subscripts pass through unchanged. (Same rewrite the unit-test
/// emitter applies to its triples; the LHS is a flattened string until
/// the #294 P1 renderer unification makes it structural.)
pub fn cast_subscripts(field: &str) -> String {
    let mut out = String::new();
    let mut rest = field;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..=i]);
        rest = &rest[i + 1..];
        let Some(j) = rest.find(']') else {
            out.push_str(rest);
            return out;
        };
        let idx = &rest[..j];
        let is_numeric = !idx.is_empty() && idx.chars().all(|c| c.is_ascii_digit());
        let is_ident = !idx.is_empty()
            && idx.chars().all(|c| c.is_alphanumeric() || c == '_')
            && !idx.starts_with(|c: char| c.is_ascii_digit());
        if is_ident && !is_numeric {
            out.push_str(&format!("{} as usize", idx));
        } else {
            out.push_str(idx);
        }
        out.push(']');
        rest = &rest[j + 1..];
    }
    out.push_str(rest);
    out
}

/// Render a single `(field, op_kind, value)` triple into Rust at the given
/// indent. The helper writes the trailing newline; the caller controls
/// where the statement sits relative to its surrounding block.
#[allow(clippy::too_many_arguments)]
pub fn emit_one_effect(
    out: &mut String,
    spec: &ParsedSpec,
    wrapping: bool,
    field: &crate::mir::Path,
    op_kind: &str,
    value: &crate::mir::Expr,
    indent: &str,
    pre_fields: &[String],
) {
    emit_one_effect_inner(
        out, spec, wrapping, field, op_kind, value, indent, None, pre_fields,
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_one_effect_inner(
    out: &mut String,
    spec: &ParsedSpec,
    wrapping: bool,
    field: &crate::mir::Path,
    op_kind: &str,
    value: &crate::mir::Expr,
    indent: &str,
    account_binder: Option<&str>,
    pre_fields: &[String],
) {
    use super::tree_render::{self, ArithMode, RustCx};

    // Harnesses run against a flat `State` (union-of-variant-fields view):
    // strip the variant prefix so `Variant.field := …` emits `s.field = …`;
    // the variant itself is tracked via `s.status`. Subscripts are cast to
    // `usize` — params bind at their spec types (`u8` etc.) while Rust
    // arrays index by `usize` (`s.voted[member_index]` was E0277, #295).
    let effect_path = field;
    let field_owned = strip_variant_prefix_for_flat_state(&effect_path_source(effect_path), spec);
    let field = field_owned.as_str();
    let field_target = render_effect_target(effect_path, spec, "s");
    // Tree-native RHS (#151 Slice 1): one render call replaces the
    // adapter's pre-rendered string + `resolve_value`'s binder surgery +
    // the account-pubkey substring rewrite. Fallibility is structural,
    // not a `contains('?')` scan.
    let (rust_value, fallible) = {
        let tree = mir_expr_tree(value);
        let cx = RustCx::native()
            .with_arith(ArithMode::Checked)
            .with_acct_env(account_binder.map(|b| b.trim_end_matches('.')));
        (
            tree_render::render_rust(tree, cx),
            tree_render::contains_fallible_arith(tree),
        )
    };
    // Checked-expression RHS (bare arithmetic lowered to `checked_*` + `?`):
    // give the `?` ops an `Option` context via an immediately-invoked
    // closure; `None` (over/underflow, div-by-zero, failed narrowing)
    // rejects the transition, matching the checked `+=`/`-=` doctrine
    // (issue #146). The return-type annotation pins inference for
    // `try_into()`-narrowed helpers; unresolvable field types fall back
    // to inference.
    let rust_value = if fallible {
        match field_rust_scalar_ty(spec, field) {
            Some(ty) => format!(
                "match (|| -> Option<{ty}> {{ Some({rust_value}) }})() {{ Some(__rhs) => __rhs, None => return false }}"
            ),
            None => format!(
                "match (|| Some({rust_value}))() {{ Some(__rhs) => __rhs, None => return false }}"
            ),
        }
    } else {
        rust_value
    };
    // Parallel effect semantics: RHS reads of fields the block also
    // writes route through the `pre_<field>` snapshots the transition
    // emitter binds up front (see `parallel_snapshot_fields`). RHS only —
    // the LHS write and the checked-op self-read stay on `s.` (each field
    // is written once, so its own read still observes pre-state).
    let rust_value = substitute_pre_reads(&rust_value, "s", pre_fields);
    match op_kind {
        "set" => {
            out.push_str(&format!("{indent}{field_target} = {rust_value};\n"));
        }
        "add" => {
            if wrapping {
                out.push_str(&format!(
                    "{indent}{field_target} = {field_target}.wrapping_add({rust_value});\n"
                ));
            } else {
                out.push_str(&format!(
                    "{indent}match {field_target}.checked_add({rust_value}) {{\n\
                     {indent}    Some(__v) => {field_target} = __v,\n\
                     {indent}    None => return false,\n\
                     {indent}}}\n"
                ));
            }
        }
        "add_sat" => {
            out.push_str(&format!(
                "{indent}{field_target} = {field_target}.saturating_add({rust_value});\n"
            ));
        }
        "add_wrap" => {
            out.push_str(&format!(
                "{indent}{field_target} = {field_target}.wrapping_add({rust_value});\n"
            ));
        }
        "sub" => {
            if wrapping {
                out.push_str(&format!(
                    "{indent}{field_target} = {field_target}.wrapping_sub({rust_value});\n"
                ));
            } else {
                out.push_str(&format!(
                    "{indent}match {field_target}.checked_sub({rust_value}) {{\n\
                     {indent}    Some(__v) => {field_target} = __v,\n\
                     {indent}    None => return false,\n\
                     {indent}}}\n"
                ));
            }
        }
        "sub_sat" => {
            out.push_str(&format!(
                "{indent}{field_target} = {field_target}.saturating_sub({rust_value});\n"
            ));
        }
        "sub_wrap" => {
            out.push_str(&format!(
                "{indent}{field_target} = {field_target}.wrapping_sub({rust_value});\n"
            ));
        }
        _ => {
            out.push_str(&format!(
                "{indent}// unknown effect: {field} {op_kind} {}\n",
                mir_expr_rust(value)
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn emit_one_effect_with_account_env(
    out: &mut String,
    spec: &ParsedSpec,
    wrapping: bool,
    field: &crate::mir::Path,
    op_kind: &str,
    value: &crate::mir::Expr,
    indent: &str,
    account_binder: &str,
    pre_fields: &[String],
) {
    emit_one_effect_inner(
        out,
        spec,
        wrapping,
        field,
        op_kind,
        value,
        indent,
        Some(account_binder),
        pre_fields,
    );
}

/// Effect targets written more than once within a single effect block,
/// as their normalized (variant-prefix-stripped) LHS paths, first-seen
/// order, deduplicated. Under the spec's PARALLEL effect semantics every
/// RHS reads pre-state, so two writes to the same target are ambiguous:
/// the Lean record update keeps the last write (`b := 1, b := 2` → 2)
/// while the sequential Rust body accumulates (`b += 1; b += 2` → 3) —
/// a model-vs-implementation divergence. The two spellings agree only
/// when a target is written once; a repeated target must be rewritten as
/// a single combined effect (`b += 3`).
///
/// Callers pass one block's effects at a time. For a `match` handler,
/// pass each arm's effects separately — writes in mutually-exclusive
/// arms don't conflict — NOT the flattened union `ParsedHandler.effects`.
pub fn duplicate_effect_targets(
    effects: &[crate::check::ParsedEffect],
    spec: &ParsedSpec,
) -> Vec<String> {
    let mut seen: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for eff in effects {
        let target = strip_variant_prefix_for_flat_state(&eff.field, spec);
        let count = seen.entry(target.clone()).or_insert(0);
        *count += 1;
        if *count == 2 {
            order.push(target);
        }
    }
    order
}

/// Verify every effect-target field is declared somewhere in the state
/// schema (`state_fields`, per-account fields, or a sum-variant payload),
/// and that no block writes the same target twice (see
/// [`duplicate_effect_targets`] for why that diverges). Errors name the
/// handler and field — catching this at codegen time beats a `cargo
/// check` error 1000 lines into the generated harness.
pub fn check_effect_targets(spec: &ParsedSpec) -> anyhow::Result<()> {
    // Duplicate-target check, per effect block (arms are independent).
    for handler in &spec.handlers {
        let blocks: Vec<&[crate::check::ParsedEffect]> = match &handler.effect_branches {
            Some(branches) => branches.arms.iter().map(|a| a.effects.as_slice()).collect(),
            None => vec![handler.effects.as_slice()],
        };
        for block in blocks {
            if let Some(dup) = duplicate_effect_targets(block, spec).into_iter().next() {
                anyhow::bail!(
                    "handler `{}` writes effect target `{}` more than once in one effect block. \
                     Under parallel effect semantics each RHS reads the pre-state, so repeated \
                     writes diverge (the Lean model keeps the last write; the generated Rust \
                     accumulates). Combine them into a single effect (e.g. `{} += <total>`).",
                    handler.name,
                    dup,
                    dup,
                );
            }
        }
    }

    check_effect_targets_declared(spec)
}

/// The original declared-target check (split out so the duplicate-target
/// guard above runs first).
fn check_effect_targets_declared(spec: &ParsedSpec) -> anyhow::Result<()> {
    use std::collections::HashSet;

    // Collect every declared field name from every place fields can live.
    let mut declared: HashSet<&str> = HashSet::new();
    for (n, _) in &spec.state_fields {
        declared.insert(n.as_str());
    }
    for acct in &spec.account_types {
        for (n, _) in &acct.fields {
            declared.insert(n.as_str());
        }
    }
    for rec in &spec.records {
        for (n, _) in &rec.fields {
            declared.insert(n.as_str());
        }
    }
    for sum in &spec.sum_types {
        for variant in &sum.variants {
            for (n, _) in &variant.fields {
                declared.insert(n.as_str());
            }
        }
    }

    // Variant-prefixed targets (`Active.balance`) are legal: index variant
    // fields so the check re-targets at the field beneath the prefix
    // instead of false-positive-bailing on the variant name.
    let mut variant_fields: std::collections::HashMap<&str, HashSet<&str>> =
        std::collections::HashMap::new();
    for acct in &spec.account_types {
        for variant in &acct.variants {
            let entry = variant_fields.entry(variant.name.as_str()).or_default();
            for (n, _) in &variant.fields {
                entry.insert(n.as_str());
            }
        }
    }

    for handler in &spec.handlers {
        for eff in &handler.effects {
            let field = &eff.field;
            let base = effect_target_base(field);
            // Variant-prefixed: the root is a variant name, so check
            // the field beneath it against that variant's payload.
            if let Some(variant_payload) = variant_fields.get(base) {
                let after = field.trim_start_matches(base).trim_start_matches('.');
                let nested_base = effect_target_base(after);
                if !nested_base.is_empty()
                    && !variant_payload.contains(nested_base)
                    && !declared.contains(nested_base)
                {
                    anyhow::bail!(
                        "handler `{}` writes effect target `{}` but `{}` is not declared in variant `{}`'s payload — add it to the variant or rename the effect",
                        handler.name,
                        field,
                        nested_base,
                        base,
                    );
                }
                continue;
            }
            if !declared.contains(base) {
                // `state := .Variant { … }` desugars to per-field effects
                // at the adapter, but non-RecordLit / unit-variant shapes
                // can survive here as a single bare-`state` effect. Accept
                // `state` whenever the spec has a multi-variant ADT —
                // downstream either handles it (RecordLit) or bails to a
                // `todo!()`.
                if base == "state" && spec.account_types.iter().any(|a| !a.variants.is_empty()) {
                    continue;
                }
                anyhow::bail!(
                    "handler `{}` writes effect target `{}` but `{}` is not declared in any state, account, record, or sum-variant payload — add it to the state declaration or remove the effect",
                    handler.name,
                    field,
                    base,
                );
            }
        }
    }
    Ok(())
}
