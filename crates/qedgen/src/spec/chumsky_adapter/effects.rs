//! Effect rendering: `EffectStmt` → the `(field, op, value)` triple consumed
//! by every backend, plus variant-promotion expansion and sBPF-check forms.

use super::*;

// ============================================================================
// Effect rendering: (field_name, op, value_string)
// ============================================================================

/// Render an `EffectStmt` to the `(field, op, value)` triple consumed by
/// every backend, plus the per-site error-variant override codegen reads
/// when lowering checked `+=` / `-=`. Override is always `None` for
/// non-checked ops — the parser is permissive for error positioning; the
/// adapter normalizes.
fn render_effect(
    stmt: &a::EffectStmt,
    params: &[(String, String)],
    consts: ConstTable,
) -> ((String, String, String), Option<String>) {
    // Field name: preserve subscript syntax as-is (e.g., `accounts[i].capital`).
    // Both Lean and Rust consumers read this string; Rust-side `as usize`
    // index casting is applied at the codegen.rs::mechanize_effect site
    // so the Lean output stays untouched.
    let field = {
        let mut s = stmt.lhs.root.clone();
        for seg in &stmt.lhs.segments {
            match seg {
                a::PathSeg::Field(f) => {
                    s.push('.');
                    s.push_str(f);
                }
                a::PathSeg::Index(i) => {
                    s.push('[');
                    s.push_str(i);
                    s.push(']');
                }
            }
        }
        s
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
    // Value string — match pest's effect_value_to_string which strips
    // `state.` prefix for qualified refs and leaves bare idents / integers.
    let value = match &stmt.rhs.node {
        Expr::Int(v) => v.to_string(),
        Expr::Path(p) => {
            let is_param = p.segments.is_empty() && params.iter().any(|(n, _)| n == &p.root);
            if is_param {
                p.root.clone()
            } else if p.root == "state" {
                // state.X → X (strip prefix, matches pest output)
                let mut s = String::new();
                for seg in &p.segments {
                    match seg {
                        a::PathSeg::Field(f) => {
                            if !s.is_empty() {
                                s.push('.');
                            }
                            s.push_str(f);
                        }
                        a::PathSeg::Index(i) => {
                            s.push('[');
                            s.push_str(i);
                            s.push(']');
                        }
                    }
                }
                s
            } else {
                // Bare path that isn't a param — emit as-is
                let mut s = p.root.clone();
                for seg in &p.segments {
                    match seg {
                        a::PathSeg::Field(f) => {
                            s.push('.');
                            s.push_str(f);
                        }
                        a::PathSeg::Index(i) => {
                            s.push('[');
                            s.push_str(i);
                            s.push(']');
                        }
                    }
                }
                s
            }
        }
        // Complex RHS (match / ctor / record update / arithmetic):
        // render in Lean form. The effect value is consumed by lean_gen,
        // so Lean-form rendering is what matters. Build a minimal type env
        // for coercion — params only; spec-wide types would require the
        // full env but aren't usually relevant on effect RHS.
        other => {
            let env = TypeEnv::default().with_params(&[]);
            let params_slice: Vec<(String, a::TypeRef)> = params
                .iter()
                .map(|(n, t)| (n.clone(), string_to_typeref_best_effort(t)))
                .collect();
            let _ = params_slice; // future: plumb real params here for coercion
            expr_to_lean(other, Ctx::Guard, consts, &env)
        }
    };
    // Keep the per-site override only for ops that can fail (checked
    // Add / Sub); saturating / wrapping / Set can't trigger an error
    // variant, so drop any parser-captured override here.
    let on_error = match stmt.op {
        a::EffectOp::Add | a::EffectOp::Sub => stmt.on_error.clone(),
        _ => None,
    };
    ((field, op.to_string(), value), on_error)
}

/// Best-effort reconstruction of a `TypeRef` from its rendered string form,
/// used only inside `render_effect` where we don't have the original AST.
fn string_to_typeref_best_effort(s: &str) -> a::TypeRef {
    a::TypeRef::Named(s.trim().to_string())
}

/// Render an effect RHS to the same string form `render_effect` uses —
/// factored out for the variant-promotion desugaring. Mirrors
/// `render_effect`'s value branch exactly.
fn render_effect_rhs_value(rhs: &Expr, params: &[(String, String)], consts: ConstTable) -> String {
    match rhs {
        Expr::Int(v) => v.to_string(),
        Expr::Path(p) => {
            let is_param = p.segments.is_empty() && params.iter().any(|(n, _)| n == &p.root);
            if is_param {
                p.root.clone()
            } else if p.root == "state" {
                let mut s = String::new();
                for seg in &p.segments {
                    match seg {
                        a::PathSeg::Field(f) => {
                            if !s.is_empty() {
                                s.push('.');
                            }
                            s.push_str(f);
                        }
                        a::PathSeg::Index(i) => {
                            s.push('[');
                            s.push_str(i);
                            s.push(']');
                        }
                    }
                }
                s
            } else {
                let mut s = p.root.clone();
                for seg in &p.segments {
                    match seg {
                        a::PathSeg::Field(f) => {
                            s.push('.');
                            s.push_str(f);
                        }
                        a::PathSeg::Index(i) => {
                            s.push('[');
                            s.push_str(i);
                            s.push(']');
                        }
                    }
                }
                s
            }
        }
        other => {
            let env = TypeEnv::default().with_params(&[]);
            let _ = params;
            expr_to_lean(other, Ctx::Guard, consts, &env)
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
) -> Vec<((String, String, String), Option<String>)> {
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
                                let value_str =
                                    render_effect_rhs_value(&fvalue.node, params, consts);
                                ((lhs_str, "set".to_string(), value_str), None)
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
    vec![render_effect(stmt, params, consts)]
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
                let mut s = p.root.clone();
                for seg in &p.segments {
                    match seg {
                        a::PathSeg::Field(f) => {
                            s.push('.');
                            s.push_str(f);
                        }
                        a::PathSeg::Index(i) => {
                            s.push('[');
                            s.push_str(i);
                            s.push(']');
                        }
                    }
                }
                s
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
