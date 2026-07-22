use super::*;
use crate::obligations::{ObligationKind, ObligationRecorder};

/// Emit overflow-safety obligations: for every handler whose body issues
/// `CheckedAdd`, a theorem that all numeric state fields stay within their
/// declared type bounds across the transition (`valid_<T>` asserted on
/// each numeric field pre and post).
///
/// Flat-state proofs auto-discharge via `unfold + split + cases + refine +
/// simp/omega` (`overflow_proof_script`); ADT-shape proofs remain
/// `:= by sorry` until the pattern-match scrutinee form lands.
pub(super) fn emit_overflow(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    emit_overflow_inner(out, mir, /* adt_form = */ false, rec);
}

/// ADT-shape variant — closes overflow theorems with `:= by sorry`; the
/// statement is identical to the flat shape.
pub(super) fn emit_overflow_adt(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    emit_overflow_inner(out, mir, /* adt_form = */ true, rec);
}

pub(super) fn emit_overflow_inner(
    out: &mut String,
    mir: &Mir,
    adt_form: bool,
    rec: &mut ObligationRecorder,
) {
    use crate::mir::{Stmt, Ty};

    let has_add = |h: &crate::mir::HandlerMir| -> bool {
        h.body
            .stmts
            .iter()
            .any(|s| matches!(s, Stmt::CheckedAdd { .. } | Stmt::WrapAdd { .. }))
    };
    let add_handlers: Vec<&crate::mir::HandlerMir> =
        mir.handlers.iter().filter(|h| has_add(h)).collect();
    if add_handlers.is_empty() {
        return;
    }

    // Numeric state fields unioned across variants in declaration order
    // (same union pass as `emit_state_struct`).
    let numeric_fields: Vec<(String, Ty)> = flat_state_fields(mir)
        .into_iter()
        .filter(|(_, ty)| {
            matches!(
                ty,
                Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::U128 | Ty::I64 | Ty::I128
            )
        })
        .collect();
    if numeric_fields.is_empty() {
        return;
    }

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str(
        "-- Overflow safety obligations (auto-generated for operations with add effects)\n",
    );
    out.push_str(
        "-- ============================================================================\n\n",
    );

    for h in add_handlers {
        let trans_name = safe_name(&format!("{}Transition", h.name));
        let param_sig = param_sig_str(&h.params);
        let param_args = param_args_str(&h.params);

        let pre_parts: Vec<String> = numeric_fields
            .iter()
            .map(|(n, t)| format!("{} s.{}", valid_fn_for(t), safe_name(n)))
            .collect();
        let post_parts: Vec<String> = numeric_fields
            .iter()
            .map(|(n, t)| format!("{} s'.{}", valid_fn_for(t), safe_name(n)))
            .collect();

        // Invariant hypotheses: properties this handler preserves. Binary
        // properties (an `s s'` shape) don't fit as single-state
        // hypotheses, but MIR doesn't carry the binary/unary tag (it lives
        // on `ParsedProperty.class`) — conservatively include every
        // preserved-by entry with an expression; revisit when the tag
        // lands on PropertyMir.
        let inv_hyps: Vec<&str> = mir
            .properties
            .iter()
            .filter(|p| p.expression.is_some() && p.preserved_by.contains(&h.name))
            .map(|p| p.name.as_str())
            .collect();

        rec.emitted(
            ObligationKind::Overflow,
            &h.name,
            &h.name,
            &format!("{}_overflow_safe", safe_name(&h.name)),
        );
        out.push_str(&format!(
            "theorem {}_overflow_safe (s s' : State) (signer : Pubkey){}\n",
            safe_name(&h.name),
            param_sig
        ));
        let pre_joined = pre_parts
            .iter()
            .map(|p| paren_low_prec(p))
            .collect::<Vec<_>>()
            .join(" \u{2227} ");
        out.push_str(&format!("    (h_valid : {})\n", pre_joined));
        for inv in &inv_hyps {
            out.push_str(&format!("    (h_inv_{} : {} s)\n", safe_name(inv), inv));
        }
        out.push_str(&format!(
            "    (h : {} s signer{} = some s') :\n",
            trans_name, param_args
        ));
        let post_joined = post_parts
            .iter()
            .map(|p| paren_low_prec(p))
            .collect::<Vec<_>>()
            .join(" \u{2227} ");
        let proof_tail = if adt_form {
            " := by sorry\n\n".to_string()
        } else {
            overflow_proof_script(mir, h, &numeric_fields)
        };
        out.push_str(&format!("    {}{}", post_joined, proof_tail));
    }
}

/// Generate the mechanical proof script for a flat-state overflow theorem.
///
/// Strategy:
///   1. `unfold <Handler>Transition at h`.
///   2. If the transition body has an `if … then … else none` guard
///      (i.e., `build_guard_cond_parts` is non-empty), `split at h`
///      and discharge the `else none` branch by `contradiction`. The
///      `then` branch's `cases h` exposes the record-update form.
///      Without a guard, `cases h` directly.
///   3. `refine ⟨…⟩` with `h_valid` projections for unchanged numeric
///      fields and `?_` placeholders for fields touched by an `add`
///      effect.
///   4. For each `?_`, emit `simp only [valid_uN, Valid.valid_uN,
///      Valid.UN_MAX]; omega` — the auto overflow guard pushed into
///      the transition body proves the bound.
pub(super) fn overflow_proof_script(
    mir: &Mir,
    h: &crate::mir::HandlerMir,
    numeric_fields: &[(String, crate::mir::Ty)],
) -> String {
    use crate::mir::{Stmt, Ty};

    let trans_name = safe_name(&format!("{}Transition", h.name));

    // Only `CheckedAdd` fires an overflow obligation on a field;
    // `WrapAdd` / `SatAdd` and non-arithmetic stmts don't abort.
    let is_add_field = |field: &str| -> bool {
        h.body.stmts.iter().any(|s| match s {
            Stmt::CheckedAdd { path, .. } => path_field_name(path) == field,
            Stmt::RequireOrAbort { .. }
            | Stmt::TokenTransfer { .. }
            | Stmt::VariantPromote { .. }
            | Stmt::Assign { .. }
            | Stmt::CheckedSub { .. }
            | Stmt::WrapAdd { .. }
            | Stmt::WrapSub { .. }
            | Stmt::SatAdd { .. }
            | Stmt::SatSub { .. }
            | Stmt::Branch { .. }
            | Stmt::Cpi { .. }
            | Stmt::Emit { .. } => false,
        })
    };

    let n = numeric_fields.len();

    // Refine tuple: `h_valid` projections for unchanged fields, `?_` for
    // changed ones (one `simp; omega` line each).
    let mut refine_parts: Vec<String> = Vec::with_capacity(n);
    let mut changed_types: Vec<&Ty> = Vec::new();
    for (i, (name, ty)) in numeric_fields.iter().enumerate() {
        if is_add_field(name) {
            refine_parts.push("?_".to_string());
            changed_types.push(ty);
        } else {
            refine_parts.push(h_valid_projection_mir(i, n));
        }
    }
    let refine_str = format!("\u{27E8}{}\u{27E9}", refine_parts.join(", "));

    let simp_goals: Vec<String> = changed_types
        .iter()
        .map(|ty| {
            let vfn = valid_fn_for(ty);
            let vmod = valid_module_for(ty);
            let vmax = valid_max_for(ty);
            format!("    simp only [{}, {}, {}]; omega", vfn, vmod, vmax)
        })
        .collect();

    let has_cond = !build_guard_cond_parts(mir, h).is_empty();

    // With a single numeric field the post-condition is ONE proposition
    // (`valid_<T> s'.f`), not a `∧`-chain — so `refine ⟨?_⟩` would be
    // ill-typed (`⟨…⟩` needs an anonymous-constructor target). Emit the
    // tuple-introducing `refine` only when there are ≥ 2 fields; with one
    // field discharge the lone goal directly (the `simp/omega` line if it
    // is the changed field, else `exact h_valid` for an unchanged carry).
    let emit_body = |proof: &mut String, indent: &str| {
        if n > 1 {
            proof.push_str(&format!("{}refine {}\n", indent, refine_str));
        } else if simp_goals.is_empty() {
            proof.push_str(&format!("{}exact h_valid\n", indent));
        }
        for g in &simp_goals {
            proof.push_str(&format!("{}\n", g));
        }
    };

    // Handler-level `let` bindings leave the unfolded hypothesis wrapped
    // in `have`/`let` binders; zeta-reduce so `split`/`cases` find the
    // `if` / record form (#156 fixture `let-bindings-fee-split`).
    let zeta = if h.lets.is_empty() {
        ""
    } else {
        " dsimp only at h;"
    };

    let mut proof = String::new();
    if has_cond {
        proof.push_str(&format!(
            " := by\n  unfold {} at h;{} split at h\n",
            trans_name, zeta
        ));
        proof.push_str("  · next hg =>\n    cases h\n");
        emit_body(&mut proof, "    ");
        proof.push_str("  · contradiction\n\n");
    } else {
        proof.push_str(&format!(
            " := by\n  unfold {} at h;{} cases h\n",
            trans_name, zeta
        ));
        emit_body(&mut proof, "  ");
        proof.push('\n');
    }
    proof
}

/// h_valid projection path for position `i` in `n` numeric fields —
/// right-associative ∧ chain: `.2` drops the head, `.1` takes the head
/// of the remainder (except the last position).
pub(super) fn h_valid_projection_mir(i: usize, n: usize) -> String {
    let mut path = "h_valid".to_string();
    for _ in 0..i {
        path.push_str(".2");
    }
    if i + 1 < n {
        path.push_str(".1");
    }
    path
}

/// MIR `Ty` → `valid_uN` function name.
pub(super) fn valid_fn_for(ty: &crate::mir::Ty) -> &'static str {
    use crate::mir::Ty;
    match ty {
        Ty::U8 => "valid_u8",
        Ty::U16 => "valid_u16",
        Ty::U32 => "valid_u32",
        Ty::U64 => "valid_u64",
        Ty::U128 => "valid_u128",
        Ty::I64 => "valid_i64",
        Ty::I128 => "valid_i128",
        _ => "valid_u64",
    }
}

/// MIR `Ty` → fully-qualified `Valid.valid_uN` name (for `simp`
/// unfolding).
pub(super) fn valid_module_for(ty: &crate::mir::Ty) -> &'static str {
    use crate::mir::Ty;
    match ty {
        Ty::U8 => "Valid.valid_u8",
        Ty::U16 => "Valid.valid_u16",
        Ty::U32 => "Valid.valid_u32",
        Ty::U64 => "Valid.valid_u64",
        Ty::U128 => "Valid.valid_u128",
        Ty::I64 => "Valid.valid_i64",
        Ty::I128 => "Valid.valid_i128",
        _ => "Valid.valid_u64",
    }
}

/// MIR `Ty` → `Valid.UN_MAX` constant name.
pub(super) fn valid_max_for(ty: &crate::mir::Ty) -> &'static str {
    use crate::mir::Ty;
    match ty {
        Ty::U8 => "Valid.U8_MAX",
        Ty::U16 => "Valid.U16_MAX",
        Ty::U32 => "Valid.U32_MAX",
        Ty::U64 => "Valid.U64_MAX",
        Ty::U128 => "Valid.U128_MAX",
        Ty::I64 => "Valid.I64_MAX",
        Ty::I128 => "Valid.I128_MAX",
        _ => "Valid.U64_MAX",
    }
}

// ----------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------
