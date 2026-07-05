use super::*;

/// Emit `inductive Status where | V1 | V2 …` (no per-constructor `: Status`
/// annotation, `deriving Repr, DecidableEq, BEq`). Distinct from the
/// flat-state `emit_lifecycle_marker`, which emits the `: Status`
/// annotation and the `deriving DecidableEq, Repr` order.
pub(super) fn emit_status_inductive_adt(out: &mut String, mir: &Mir) {
    let lifecycle: Vec<&str> = mir.state.variants.iter().map(|v| v.tag.as_str()).collect();
    if lifecycle.len() < 2 {
        return;
    }
    out.push_str("inductive Status where\n");
    for v in &lifecycle {
        out.push_str(&format!("  | {}\n", v));
    }
    out.push_str("  deriving Repr, DecidableEq, BEq\n\n");
}

/// Emit the `inductive State where | V1 | V2 (f : T) …` block plus the
/// `Inhabited State` instance. The first variant supplies the Inhabited
/// default — specs canonically declare the initial state first.
pub(super) fn emit_inductive_state_adt(out: &mut String, mir: &Mir) {
    out.push_str("inductive State where\n");
    for v in &mir.state.variants {
        if v.fields.is_empty() {
            out.push_str(&format!("  | {}\n", v.tag));
        } else {
            let params: Vec<String> = v
                .fields
                .iter()
                .map(|f| format!("({} : {})", safe_name(&f.name), render_ty(&f.ty)))
                .collect();
            out.push_str(&format!("  | {} {}\n", v.tag, params.join(" ")));
        }
    }
    out.push_str("  deriving Repr, DecidableEq, BEq\n\n");
    if let Some(first) = mir.state.variants.first() {
        if first.fields.is_empty() {
            out.push_str(&format!(
                "instance : Inhabited State := \u{27E8}.{}\u{27E9}\n\n",
                first.tag,
            ));
        } else {
            let defaults: Vec<String> =
                first.fields.iter().map(|_| "default".to_string()).collect();
            out.push_str(&format!(
                "instance : Inhabited State := \u{27E8}.{} {}\u{27E9}\n\n",
                first.tag,
                defaults.join(" "),
            ));
        }
    }
}

/// Emit `def State.status : State → Status` with one match arm per variant.
pub(super) fn emit_state_status_accessor_adt(out: &mut String, mir: &Mir) {
    out.push_str("def State.status : State \u{2192} Status\n");
    for v in &mir.state.variants {
        let pat = if v.fields.is_empty() {
            format!(".{}", v.tag)
        } else {
            let wild: Vec<&str> = v.fields.iter().map(|_| "_").collect();
            format!(".{} {}", v.tag, wild.join(" "))
        };
        out.push_str(&format!("  | {} => .{}\n", pat, v.tag));
    }
    out.push('\n');
}

/// Emit per-field `def State.<field> : State → <Type>` accessors across
/// the union of variant fields. Each arm returns the bound field when the
/// variant carries it; the type default otherwise.
pub(super) fn emit_state_field_accessors_adt(out: &mut String, mir: &Mir) {
    let fields = flat_state_fields(mir);
    for (fname, fty) in &fields {
        let lean_ty = render_ty(fty);
        let default = ty_default_literal(fty);
        out.push_str(&format!(
            "def State.{} : State \u{2192} {}\n",
            safe_name(fname),
            lean_ty
        ));
        for v in &mir.state.variants {
            if v.fields.iter().any(|f| &f.name == fname) {
                let pat_parts: Vec<String> = v
                    .fields
                    .iter()
                    .map(|f| {
                        if &f.name == fname {
                            safe_name(&f.name)
                        } else {
                            "_".to_string()
                        }
                    })
                    .collect();
                let pat = format!(".{} {}", v.tag, pat_parts.join(" "));
                out.push_str(&format!("  | {} => {}\n", pat, safe_name(fname)));
            } else {
                let pat = if v.fields.is_empty() {
                    format!(".{}", v.tag)
                } else {
                    let wild: Vec<&str> = v.fields.iter().map(|_| "_").collect();
                    format!(".{} {}", v.tag, wild.join(" "))
                };
                out.push_str(&format!("  | {} => {}\n", pat, default));
            }
        }
        out.push('\n');
    }
}

pub(super) fn emit_header(out: &mut String, _mir: &Mir) {
    out.push_str("import QEDGen.Solana.Account\n");
    out.push_str("import QEDGen.Solana.Cpi\n");
    out.push_str("import QEDGen.Solana.State\n");
    out.push_str("import QEDGen.Solana.Valid\n\n");
}

pub(super) fn emit_namespace_open(out: &mut String, mir: &Mir) {
    out.push_str(&format!("namespace {}\n\n", mir.name));
    out.push_str("open QEDGen.Solana\n\n");
}

pub(super) fn emit_namespace_close(out: &mut String, mir: &Mir) {
    out.push_str(&format!("end {}\n", mir.name));
}

/// Emit `abbrev NAME : Nat := VALUE` lines for top-level constants.
pub(super) fn emit_constants(out: &mut String, mir: &Mir) {
    if mir.constants.is_empty() {
        return;
    }
    for (name, val) in &mir.constants {
        out.push_str(&format!("abbrev {} : Nat := {}\n", safe_name(name), val));
    }
    out.push('\n');
}

/// Emit uninterpreted helpers as Lean `opaque <name> : T1 → … → R`
/// declarations (`opaque`, not `axiom`, so transitions stay computable).
pub(super) fn emit_uninterpreted_helpers(out: &mut String, mir: &Mir) {
    if mir.uninterpreted_helpers.is_empty() {
        return;
    }
    out.push_str(
        "-- Uninterpreted helpers: declared opaquely so generated\n\
         -- transitions typecheck even though the DSL doesn't model\n\
         -- their semantics. Treat each as an abstract Bool predicate;\n\
         -- strengthen into a concrete definition in your support\n\
         -- module if you want to discharge it (rather than trust it).\n\
         -- `opaque` keeps the transition functions computable\n\
         -- (axioms would force them noncomputable).\n",
    );
    for h in &mir.uninterpreted_helpers {
        let sig = if h.arg_types.is_empty() {
            h.return_type.clone()
        } else {
            let mut parts: Vec<String> = h.arg_types.clone();
            parts.push(h.return_type.clone());
            parts.join(" \u{2192} ") // →
        };
        out.push_str(&format!("opaque {} : {}\n", safe_name(&h.name), sig));
    }
    out.push('\n');
}

/// Emit `ref_impl` bodies as Lean `def`s. Bodies are emitted verbatim:
/// Map-indexed subscripts (`m[i]` → `(m i)`; `Map N T = Fin N → T` has no
/// GetElem instance) aren't rewritten here — apply
/// `rewrite_subscripts_lean` if a fixture trips on it.
pub(super) fn emit_ref_impls(out: &mut String, mir: &Mir) {
    if mir.ref_impls.is_empty() {
        return;
    }
    out.push_str(
        "-- Reference implementations: pure expressions named so\n\
         -- ensures clauses can call them. The user's Rust impl is\n\
         -- verified to satisfy the ensures referencing these, not\n\
         -- forced to implement them verbatim.\n",
    );
    for r in &mir.ref_impls {
        let params = r
            .params
            .iter()
            .map(|(n, t)| format!("({} : {})", safe_name(n), map_dsl_ty(t)))
            .collect::<Vec<_>>()
            .join(" ");
        let ret = map_dsl_ty(&r.return_type);
        let body = &r.lean_body;
        if params.is_empty() {
            out.push_str(&format!(
                "def {} : {} := {}\n",
                safe_name(&r.name),
                ret,
                body
            ));
        } else {
            out.push_str(&format!(
                "def {} {} : {} := {}\n",
                safe_name(&r.name),
                params,
                ret,
                body
            ));
        }
    }
    out.push('\n');
}

pub(super) fn emit_lifecycle_marker(out: &mut String, mir: &Mir) {
    // Emit `inductive Status` only when the lifecycle has ≥ 2 states: a
    // single-state lifecycle is no discriminator, and emitting Status for
    // it collides with user-declared `status` fields (issue #43).
    let states = &mir.state.lifecycle_states;
    if states.len() < 2 {
        return;
    }
    out.push_str("inductive Status where\n");
    for s in states {
        out.push_str(&format!("  | {}\n", safe_name(s)));
    }
    // `Inhabited` so the flat `State` (which carries a `status : Status`
    // field) can itself derive `Inhabited` — required by the polymorphic
    // CPI ensures-axioms (`{State} [Inhabited State] …`). Harmless for
    // specs without CPI composition.
    out.push_str("  deriving Repr, DecidableEq, BEq, Inhabited\n\n");
}

/// Emit the `inductive Operation` enum + `def applyOp` dispatcher.
pub(super) fn emit_operation_inductive(out: &mut String, mir: &Mir) {
    if mir.handlers.is_empty() {
        return;
    }

    out.push_str("inductive Operation where\n");
    for h in &mir.handlers {
        let ctor = safe_name(&h.name);
        if h.params.is_empty() {
            out.push_str(&format!("  | {}\n", ctor));
        } else {
            let params: Vec<String> = h
                .params
                .iter()
                .map(|(n, t)| format!("({} : {})", n, render_ty(t)))
                .collect();
            out.push_str(&format!("  | {} {}\n", ctor, params.join(" ")));
        }
    }
    out.push_str("  deriving Repr, DecidableEq, BEq\n\n");

    out.push_str("def applyOp (s : State) (signer : Pubkey) : Operation \u{2192} Option State\n");
    for h in &mir.handlers {
        let ctor = safe_name(&h.name);
        let trans = safe_name(&format!("{}Transition", h.name));
        let names: Vec<String> = h.params.iter().map(|(n, _)| n.clone()).collect();
        let pattern_args = if names.is_empty() {
            String::new()
        } else {
            format!(" {}", names.join(" "))
        };
        let call_args = if names.is_empty() {
            String::new()
        } else {
            format!(" {}", names.join(" "))
        };
        out.push_str(&format!(
            "  | .{}{} => {} s signer{}\n",
            ctor, pattern_args, trans, call_args
        ));
    }
    out.push('\n');
}

pub(super) fn emit_state_struct(out: &mut String, mir: &Mir) {
    // Flat-state form: union every variant's fields into one struct keyed
    // by name; `status` carries the lifecycle discriminator. Per-variant
    // constructors are the `render_single_account_adt` path.
    if mir.state.variants.is_empty() {
        return;
    }

    let has_lifecycle = mir.state.lifecycle_states.len() >= 2;

    out.push_str("structure State where\n");
    for (fname, fty) in &flat_state_fields(mir) {
        out.push_str(&format!("  {} : {}\n", safe_name(fname), render_ty(fty)));
    }
    if has_lifecycle {
        out.push_str("  status : Status\n");
    }
    // Ghost (spec-only) fields, appended LAST so the non-ghost field
    // prefix keeps a stable order for any positional `⟨…⟩` State
    // construction (e.g. cover witnesses).
    for ghost in &mir.ghosts {
        out.push_str(&format!(
            "  {} : {}\n",
            safe_name(&ghost.name),
            render_ty(&ghost.ty)
        ));
    }
    // `Inhabited` enables the polymorphic CPI ensures-axioms
    // (`{State} [Inhabited State] …`) to apply to this state; all field
    // types (Pubkey / Nat / Bool / Status) are themselves Inhabited.
    out.push_str("  deriving Repr, DecidableEq, BEq, Inhabited\n\n");
}

// ----------------------------------------------------------------------
// Cover-witness machinery — builds concrete state witnesses for
// cover-trace proofs by symbolically evaluating each handler in a trace.
// `emit_covers` uses it to replace `:= sorry` with a real
// `exact ⟨…, by decide, …⟩` discharge when every step is computable.
// ----------------------------------------------------------------------
