//! Effect rendering: `EffectStmt` → the `(field, op, value)` triple consumed
//! by every backend, plus variant-promotion expansion and sBPF-check forms.

use super::*;

// ============================================================================
// Effect rendering: (field_name, op, value_string)
// ============================================================================

/// One rendered effect: the legacy `(field, op, value)` triple every
/// backend consumes, the per-site error override, and the Rust-form RHS
/// (`value_rust`) the Kani/proptest harness lane reads via MIR
/// `Expr.rust`. For simple RHS shapes (literal / bare path) `value_rust`
/// equals the triple's value so binder-specific resolution downstream
/// (`resolve_value`) keeps working; compound shapes render through the
/// canonicalized typed AST (issues #143/#144/#146). `tree` is the typed
/// RHS carrier (#151 Slice 0) — built for every shape, simple or compound.
pub(super) struct RenderedEffect {
    pub triple: (String, String, String),
    pub lhs: crate::mir::expr_tree::TreePath,
    pub on_error: Option<String>,
    pub value_rust: String,
    pub tree: Option<crate::mir::ExprTree>,
}

impl RenderedEffect {
    /// Convert into the self-contained per-site effect the IR carries
    /// (#151 Slice 4 — one struct instead of four parallel arrays).
    pub fn into_parsed_effect(self) -> crate::check::ParsedEffect {
        let (field, op, value) = self.triple;
        crate::check::ParsedEffect {
            field,
            lhs: Some(self.lhs),
            op,
            value,
            value_rust: self.value_rust,
            on_error: self.on_error,
            tree: self.tree,
        }
    }
}

/// Render an `EffectStmt` to the `(field, op, value)` triple consumed by
/// every backend, plus the per-site error-variant override codegen reads
/// when lowering checked `+=` / `-=`. Override is always `None` for
/// non-checked ops — the parser is permissive for error positioning; the
/// adapter normalizes.
fn render_effect(
    stmt: &a::EffectStmt,
    params: &[(String, String)],
    consts: ConstTable,
    canon: &CanonScope,
    env: &TypeEnv,
    tcx: &TreeCx,
) -> RenderedEffect {
    // Field name: preserve subscript syntax as-is (e.g., `accounts[i].capital`).
    // Both Lean and Rust consumers read this string; Rust-side `as usize`
    // index casting is applied at the codegen.rs::mechanize_effect site
    // so the Lean output stays untouched.
    let field = stmt.lhs.to_source_string();
    // An effect LHS is a write into state even when its bare field name
    // collides with a handler parameter. Expression canonicalization cannot
    // make that assumption, so root the target explicitly here.
    let lhs_path = if stmt.lhs.root == "state" {
        stmt.lhs.clone()
    } else {
        let mut segments = Vec::with_capacity(stmt.lhs.segments.len() + 1);
        segments.push(a::PathSeg::Field(stmt.lhs.root.clone()));
        segments.extend(stmt.lhs.segments.iter().cloned());
        a::Path {
            root: "state".to_string(),
            segments,
        }
    };
    let lhs_node = Node::new(a::Expr::Path(lhs_path), 0..0);
    let crate::mir::ExprTree::Path(lhs) = build_expr_tree(&lhs_node, tcx) else {
        unreachable!("an effect target path must build as ExprTree::Path")
    };
    // Per-effect semantic tag:
    //   - "add" / "sub"               = checked (default)
    //   - "add_sat" / "sub_sat"       = saturating (`+=!` / `-=!`)
    //   - "add_wrap" / "sub_wrap"     = wrapping   (`+=?` / `-=?`)
    // Existing code paths that test `kind == "add"` continue to work for the
    // default case (the one they were written against). Codegen branches on
    // the full tag when the distinction matters.
    let op = match stmt.op {
        a::EffectOp::Add => "add",
        a::EffectOp::AddSat => "add_sat",
        a::EffectOp::AddWrap => "add_wrap",
        a::EffectOp::Sub => "sub",
        a::EffectOp::SubSat => "sub_sat",
        a::EffectOp::SubWrap => "sub_wrap",
        a::EffectOp::Set => "set",
    };
    // Value strings. Simple shapes (literal / bare path) keep the legacy
    // single-form rendering: the triple's value is stripped of `state.`
    // (matching pest output) and `value_rust` mirrors it, so downstream
    // binder-specific resolution (`resolve_value`) is unchanged. Compound
    // shapes (calls / conditionals / arithmetic / match / records) are
    // canonicalized (`active` → `state.active`, issue #139 seam) and
    // rendered per-backend with the real type env: Lean via
    // `expr_to_lean` (state reads become `s.X`), Rust via `expr_to_rust`
    // in checked mode (issues #143/#144/#146 — previously the Lean form
    // leaked into the Kani/proptest harnesses verbatim).
    let (value, value_rust, tree) =
        render_effect_rhs_forms(&stmt.rhs, params, consts, canon, env, tcx);
    // Keep the per-site override only for ops that can fail (checked
    // Add / Sub); saturating / wrapping / Set can't trigger an error
    // variant, so drop any parser-captured override here.
    let on_error = match stmt.op {
        a::EffectOp::Add | a::EffectOp::Sub => stmt.on_error.clone(),
        _ => None,
    };
    RenderedEffect {
        triple: (field, op.to_string(), value),
        lhs,
        on_error,
        value_rust,
        tree,
    }
}

/// Render an effect RHS to its `(lean_or_simple, rust, tree)` forms — see
/// the comment at the `render_effect` call site for the string split.
/// Factored out for the variant-promotion desugaring. The tree is built
/// from the canonicalized AST for *every* shape (the strings keep the
/// legacy simple/compound split for byte-stability during the #151 port).
fn render_effect_rhs_forms(
    rhs: &Node<Expr>,
    params: &[(String, String)],
    consts: ConstTable,
    canon: &CanonScope,
    env: &TypeEnv,
    tcx: &TreeCx,
) -> (String, String, Option<crate::mir::ExprTree>) {
    let tree = Some(build_expr_tree(&canonicalize_state_refs(rhs, canon), tcx));
    match &rhs.node {
        Expr::Int(v) => (v.to_string(), v.to_string(), tree),
        Expr::Path(p) => {
            let is_param = p.segments.is_empty() && params.iter().any(|(n, _)| n == &p.root);
            let s = if is_param {
                p.root.clone()
            } else if p.root == "state" {
                // state.X → X (strip prefix, matches pest output)
                p.segments_source_string()
            } else {
                // Bare path that isn't a param — emit as-is
                p.to_source_string()
            };
            (s.clone(), s, tree)
        }
        // Compound RHS: canonicalize bare state refs once, then render
        // both backend forms from the same AST.
        _ => {
            let canon_rhs = canonicalize_state_refs(rhs, canon);
            let lean = expr_to_lean(&canon_rhs.node, Ctx::Guard, consts, env);
            let rust = expr_to_rust(
                &canon_rhs.node,
                Ctx::Guard,
                consts,
                opts_native(env).with_checked_arith(),
            );
            (lean, rust, tree)
        }
    }
}

/// Desugar `state := .Variant { f := e, ... }` whole-state assignment into
/// per-field effects with variant-prefixed LHS (`Variant.f`), routing the
/// shape through the existing `emit_cross_variant_promotion` emitter. Other
/// shapes return a single-element Vec of `render_effect` output (uniform
/// iteration). Unit-variant promotion (`state := .Closed`) returns an empty
/// Vec — the wrapper assignment handles the transition from
/// `handler.post_status`.
pub(super) fn render_effect_or_expand_variant_promotion(
    stmt: &a::EffectStmt,
    params: &[(String, String)],
    consts: ConstTable,
    canon: &CanonScope,
    env: &TypeEnv,
    tcx: &TreeCx,
) -> Vec<RenderedEffect> {
    if matches!(stmt.op, a::EffectOp::Set)
        && stmt.lhs.root == "state"
        && stmt.lhs.segments.is_empty()
    {
        if let Expr::Ctor { variant, payload } = &stmt.rhs.node {
            match payload {
                None => {
                    // Unit variant — drop. Wrapper handles transition.
                    return Vec::new();
                }
                Some(p) => {
                    if let Expr::RecordLit(fields) = &p.node {
                        // Payload variant + record literal — expand
                        // per field with variant-prefixed LHS.
                        return fields
                            .iter()
                            .map(|(fname, fvalue)| {
                                let lhs_str = format!("{}.{}", variant, fname);
                                let (value_str, value_rust, tree) = render_effect_rhs_forms(
                                    fvalue, params, consts, canon, env, tcx,
                                );
                                RenderedEffect {
                                    triple: (lhs_str, "set".to_string(), value_str),
                                    lhs: crate::mir::expr_tree::TreePath {
                                        root: "state".to_string(),
                                        binding: crate::mir::expr_tree::BindingKind::StateField,
                                        segments: vec![
                                            crate::mir::expr_tree::TreeSeg::Field(variant.clone()),
                                            crate::mir::expr_tree::TreeSeg::Field(fname.clone()),
                                        ],
                                        ty: None,
                                    },
                                    on_error: None,
                                    value_rust,
                                    tree,
                                }
                            })
                            .collect();
                    }
                    // Non-record-literal payload (e.g.
                    // `state := .Active some_bound_record`) — fall
                    // through to single render_effect. Codegen will
                    // bail (currently unsupported); agent fills the
                    // todo!() body.
                }
            }
        }
    }
    vec![render_effect(stmt, params, consts, canon, env, tcx)]
}

// ============================================================================
// sBPF instruction adapter
// ============================================================================

/// Render a simple guard expression into the space-separated ASCII triple
/// form consumed by `derive_guard_hypotheses` in `lean_gen`:
///   `field == RHS`, `field >= RHS`, etc.
/// When `resolve_consts` is true, bare identifiers that are declared constants
/// are substituted with their values (for the `checks` form). Otherwise names
/// are preserved verbatim (for the `checks_raw` form).
pub(super) fn render_sbpf_check(e: &Expr, consts: ConstTable, resolve_consts: bool) -> String {
    fn render(e: &Expr, consts: ConstTable, resolve_consts: bool) -> String {
        match e {
            Expr::Int(v) => v.to_string(),
            Expr::Bool(b) => if *b { "true" } else { "false" }.to_string(),
            Expr::Path(p) => {
                // Render as root[.seg]* with no state prefix substitution.
                if p.segments.is_empty() {
                    if resolve_consts {
                        if let Some(v) = consts.get(&p.root) {
                            return v.clone();
                        }
                    }
                    return p.root.clone();
                }
                p.to_source_string()
            }
            Expr::Paren(inner) => render(&inner.node, consts, resolve_consts),
            Expr::Cmp { op, lhs, rhs } => {
                let sym = match op {
                    a::CmpOp::Eq => "==",
                    a::CmpOp::Ne => "!=",
                    a::CmpOp::Le => "<=",
                    a::CmpOp::Ge => ">=",
                    a::CmpOp::Lt => "<",
                    a::CmpOp::Gt => ">",
                };
                format!(
                    "{} {} {}",
                    render(&lhs.node, consts, resolve_consts),
                    sym,
                    render(&rhs.node, consts, resolve_consts)
                )
            }
            Expr::Arith { op, lhs, rhs } => {
                let sym = match op {
                    a::ArithOp::Add => "+",
                    a::ArithOp::Sub => "-",
                    a::ArithOp::Mul => "*",
                    a::ArithOp::Div => "/",
                    a::ArithOp::Mod => "%",
                };
                format!(
                    "{} {} {}",
                    render(&lhs.node, consts, resolve_consts),
                    sym,
                    render(&rhs.node, consts, resolve_consts)
                )
            }
            // Fallback for unexpected shapes — pretty-print a minimal Lean-ish form.
            other => {
                let env = TypeEnv::default();
                expr_to_lean(other, Ctx::Guard, consts, &env)
            }
        }
    }
    render(e, consts, resolve_consts)
}
