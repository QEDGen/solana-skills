//! qedgen Lean codegen — the MIR consumer. Sole Lean-codegen path
//! since v2.32 deleted the legacy `ParsedSpec`-direct `lean_gen.rs`.
//! Consumes `mir::Mir` and writes `Spec.lean` (plus interface sidecars
//! via [`crate::lean_sidecars`]). Gated by `tests/mir_snapshot.rs`.
//!
//! `render` dispatches on spec shape:
//! - sBPF (`mir.is_assembly`) → `render_sbpf` (handled in `generate`,
//!   reads `ParsedSpec` directly — assembly proofs over `Program.lean`).
//! - Indexed (records or `Map[N] T`) → `render_indexed_state`.
//! - Multi-account (`account_states.len() > 1`) → `render_multi_account`.
//! - Multi-variant ADT (`type State | A | B of { … }`) →
//!   `render_single_account_adt`.
//! - Single-account → `render_single_account`.
//!
//! The single-account renderer emits these sections in order:
//! 1. `import QEDGen.Solana.{Account, Cpi, State, Valid}`.
//! 2. `namespace <ProgramName>` + `open QEDGen.Solana`.
//! 3. Uninterpreted helpers + ref-impls.
//! 4. Constants (`abbrev NAME : Nat := VALUE`).
//! 5. `inductive Status` if ≥2 lifecycle states.
//! 6. `structure State` with all state fields.
//! 7. Transition functions — one `def <handler>_transition (s : State)
//!    … : Option State` per handler.
//! 8. CPI theorems — per-handler `theorem <handler>_cpi_correct` for
//!    Tier-1/2 callees.
//! 9. Invariant theorems.
//! 10. `inductive Operation` + `def applyOp` — union of all handlers.
//! 11. Property theorems.
//! 12. Abort theorems.
//! 13. Ensures theorems.
//! 14. Frame conditions.
//! 15. Cover / liveness / environment / overflow theorems.
//! 16. `end <ProgramName>`.

use crate::mir::Mir;
use anyhow::Result;
use std::path::Path;

/// Top-level entry. Renders the `Spec.lean` body from MIR, then
/// delegates the sidecar work (import injection, sibling axiom modules,
/// lakefile updates, verified-callee require directives) to
/// `lean_sidecars::write_spec_with_sidecars` — the renderer-agnostic
/// sidecar writer. The snapshot suite (`tests/mir_snapshot.rs`) gates
/// the emitted `Spec.lean` against checked-in references.
pub fn generate(mir: &Mir, parsed: &crate::check::ParsedSpec, output_path: &Path) -> Result<()> {
    // sBPF assembly specs render a wholly different shape (Program.lean
    // import + per-instruction guard/property theorem stubs over
    // `executeFn`/`wp_exec`) that has nothing to do with the
    // state-machine `Stmt` IR. Dispatch on the MIR-lifted `is_assembly`
    // flag; the renderer reads instruction/layout/guard data straight
    // from `ParsedSpec` (the single consumer means MIR carries only the
    // dispatch signal, not the data — see `Mir::is_assembly`).
    let content = if mir.is_assembly {
        render_sbpf(parsed)
    } else {
        render(mir)
    };
    crate::lean_sidecars::write_spec_with_sidecars(content, parsed, output_path)
}

/// Pure render. Dispatches based on the MIR shape and emits the full
/// Spec.lean as a String.
///
/// Phase 1 stub: emits header + namespace + state struct only. Other
/// sections land iteratively as Phase 1c progresses.
pub fn render(mir: &Mir) -> String {
    // Dispatch by spec shape — indexed, multi-account, single. sBPF
    // assembly specs are dispatched earlier in `generate` (they read
    // `ParsedSpec` directly via `render_sbpf`), so `render` only sees
    // state-machine shapes here.
    if is_indexed(mir) {
        return render_indexed_state(mir);
    }
    if is_multi_account(mir) {
        return render_multi_account(mir);
    }
    if is_multi_variant_adt(mir) {
        return render_single_account_adt(mir);
    }
    render_single_account(mir)
}

// ----------------------------------------------------------------------
// Shape detection — mirrors lean_gen.rs predicates
// ----------------------------------------------------------------------

fn is_indexed(mir: &Mir) -> bool {
    // Indexed spec: declares records (modeled as `Custom` types in MIR)
    // or uses `Map[N]` fields. Detect by scanning state-field types.
    mir.state.variants.iter().any(|v| {
        v.fields
            .iter()
            .any(|f| matches!(&f.ty, crate::mir::Ty::Map { .. }))
    })
}

fn is_multi_account(mir: &Mir) -> bool {
    // v2.30 Phase 2: multi-account specs declare > 1 `type Account`
    // block. MIR carries the full list in `account_states`;
    // `render_multi_account` walks them and emits per-account
    // `<Name>State`, `<Name>Status`, `<Name>Operation`, `apply<Name>Op`.
    // Single-account specs route through `render_single_account` /
    // `render_single_account_adt` as before. Indexed-state specs are
    // dispatched earlier in `render` and skip this gate.
    mir.account_states.len() > 1
}

/// True iff the single-account spec opts into the multi-variant ADT
/// shape (real `inductive State` with per-variant payload):
///   * declares `pragma state_repr = adt` (lifted to `Mir::adt_state` via
///     `ParsedSpec::state_repr_is_adt`) — the explicit opt-in that
///     replaced the pre-v2.33 `WrongState`-error footgun;
///   * has ≥ 2 state variants;
///   * is not indexed (Map / record fields route elsewhere).
fn is_multi_variant_adt(mir: &Mir) -> bool {
    mir.adt_state && mir.state.variants.len() > 1 && !is_indexed(mir)
}

// ----------------------------------------------------------------------
// Shape-specific renderers
// ----------------------------------------------------------------------

fn render_single_account(mir: &Mir) -> String {
    let mut out = String::new();
    emit_header(&mut out, mir);
    emit_namespace_open(&mut out, mir);
    emit_uninterpreted_helpers(&mut out, mir);
    emit_ref_impls(&mut out, mir);
    emit_constants(&mut out, mir);
    emit_lifecycle_marker(&mut out, mir);
    emit_state_struct(&mut out, mir);
    emit_transitions(&mut out, mir);
    // §8 CPI theorems — emitted after transitions for section ordering.
    // This emits the in-`Spec.lean` half (per-handler CPI theorems); the
    // sibling `<Iface>.lean` axiom modules + lakefile wiring are written
    // separately by `lean_sidecars::write_spec_with_sidecars`, which
    // recomputes the pinned set, so the returned value is unused here.
    let _pinned = emit_cpi_theorems(&mut out, mir);
    emit_invariants(&mut out, mir);
    emit_operation_inductive(&mut out, mir);
    emit_properties(&mut out, mir);
    emit_aborts_if(&mut out, mir);
    emit_ensures(&mut out, mir);
    emit_frame_conditions(&mut out, mir);
    emit_covers(&mut out, mir);
    emit_liveness(&mut out, mir);
    emit_environments(&mut out, mir);
    emit_overflow(&mut out, mir);
    emit_namespace_close(&mut out, mir);
    out
}

/// v2.24 §S5 multi-variant ADT path — port of
/// `lean_gen::render_single_account_adt`. The state lowers as a real
/// `inductive State where | V1 | V2 …` block (with payload per
/// variant); transitions pattern-match on the pre-variant; covers
/// build per-variant witnesses; properties / aborts / overflow take
/// the ADT-flavored emitter pair.
///
/// Phase A scope (this commit): state ADT block + status accessor +
/// per-field accessor bridges. Section ordering matches the legacy
/// emitter so the file boundary diffs are localized to the state
/// shape itself. Subsequent phases port the remaining `_adt`-
/// flavored emitters (transitions, properties, aborts, frame,
/// covers, liveness, overflow).
fn render_single_account_adt(mir: &Mir) -> String {
    let mut out = String::new();
    emit_header(&mut out, mir);
    emit_namespace_open(&mut out, mir);
    emit_uninterpreted_helpers(&mut out, mir);
    emit_ref_impls(&mut out, mir);
    emit_constants(&mut out, mir);

    emit_status_inductive_adt(&mut out, mir);
    emit_inductive_state_adt(&mut out, mir);
    emit_state_status_accessor_adt(&mut out, mir);
    emit_state_field_accessors_adt(&mut out, mir);

    // Phase B — match-based transitions over the inductive State.
    emit_transitions_adt(&mut out, mir);
    // Phase C — ADT-flavored emitters (aborts / frame / overflow)
    // emit `:= by sorry` and the True-placeholder frame, matching
    // `lean_gen::render_*_adt`. Other sections (ensures, properties,
    // covers, liveness, environments) share the flat-shape emitters
    // — their statements are independent of the State carrier.
    let _pinned = emit_cpi_theorems(&mut out, mir);
    emit_invariants(&mut out, mir);
    emit_operation_inductive(&mut out, mir);
    emit_properties(&mut out, mir);
    emit_aborts_if_adt(&mut out, mir);
    emit_ensures(&mut out, mir);
    emit_frame_conditions_adt(&mut out, mir);
    emit_covers_adt(&mut out, mir);
    emit_liveness_adt(&mut out, mir);
    emit_environments(&mut out, mir);
    emit_overflow_adt(&mut out, mir);
    emit_namespace_close(&mut out, mir);
    out
}

/// Emit `inductive Status where | V1 | V2 …`. Mirrors the v2.24 ADT
/// `emit_status_inductive` (no per-constructor `: Status` annotation,
/// `deriving Repr, DecidableEq, BEq`). Distinct from the
/// pre-v2.24 flat-state `emit_lifecycle_marker` which emits the
/// `: Status` annotation and `deriving DecidableEq, Repr` order.
fn emit_status_inductive_adt(out: &mut String, mir: &Mir) {
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

/// Emit the `inductive State where | V1 | V2 (f : T) …` block plus
/// the `Inhabited State` instance. Mirrors
/// `lean_gen::emit_inductive_state`. The first variant supplies the
/// Inhabited default — qedgen specs canonically declare the initial
/// state first (e.g. `Uninitialized`), so this preserves intent.
fn emit_inductive_state_adt(out: &mut String, mir: &Mir) {
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

/// Emit `def State.status : State → Status` with one match arm per
/// variant. Mirrors `lean_gen::emit_state_status_accessor`.
fn emit_state_status_accessor_adt(out: &mut String, mir: &Mir) {
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

/// Emit per-field `def State.<field> : State → <Type>` accessors
/// across the union of variant fields. Each arm returns the bound
/// field when the variant carries it; type defaults otherwise.
/// Mirrors `lean_gen::emit_state_field_accessors`.
fn emit_state_field_accessors_adt(out: &mut String, mir: &Mir) {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut fields: Vec<(String, crate::mir::Ty)> = Vec::new();
    for v in &mir.state.variants {
        for f in &v.fields {
            if seen.insert(f.name.clone()) {
                fields.push((f.name.clone(), f.ty.clone()));
            }
        }
    }
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

/// Return the Lean literal for the type's default value, used when
/// a variant doesn't carry a field referenced through an accessor.
/// Mirrors `lean_gen::default_value_for` against MIR's typed enum.
fn ty_default_literal(ty: &crate::mir::Ty) -> &'static str {
    use crate::mir::Ty;
    match ty {
        Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::U128 => "0",
        Ty::I64 | Ty::I128 => "0",
        Ty::Bool => "false",
        _ => "default",
    }
}

/// Return the Lean literal for the type's maximum value, used by the
/// ADT transition emitter to synthesize overflow bound checks on
/// `Stmt::CheckedAdd` sites. Mirrors `lean_gen::type_max_const`.
/// `None` for non-numeric / signed types.
fn ty_max_const(ty: &crate::mir::Ty) -> Option<&'static str> {
    use crate::mir::Ty;
    match ty {
        Ty::U8 => Some("255"),
        Ty::U16 => Some("65535"),
        Ty::U32 => Some("4294967295"),
        Ty::U64 => Some("18446744073709551615"),
        Ty::U128 => Some("340282366920938463463374607431768211455"),
        _ => None,
    }
}

/// Strip a leading `Variant.` prefix from a `Path` so an effect like
/// `Open.pool_balance := initial` resolves to the bare field name
/// for variant-arm binding. Mirrors
/// `rust_codegen_util::strip_variant_prefix_for_flat_state`.
fn strip_variant_prefix(path: &crate::mir::Path, mir: &Mir) -> String {
    if path.segments.len() >= 2 {
        let head = &path.segments[0];
        let is_variant = mir.state.variants.iter().any(|v| &v.tag == head);
        if is_variant {
            return path.segments[1..].join(".");
        }
    }
    path.segments.join(".")
}

/// Emit a transition function for each handler under the multi-variant
/// ADT shape. Body: `match s with | .Pre <bindings> => … | _ => none`,
/// where the arm constructs a `.Post <args>` from the effects + the
/// pre-variant bindings.
///
/// Mirrors `lean_gen::render_transition_adt`. Cross-variant transitions
/// whose post-variant has fields not derivable from the spec (effects,
/// auth, pre-variant carry-over) fall back to type defaults with a
/// `todo!()` comment, matching legacy behavior.
fn emit_transitions_adt(out: &mut String, mir: &Mir) {
    for h in &mir.handlers {
        emit_handler_transition_adt(out, mir, h);
    }
}

fn emit_handler_transition_adt(out: &mut String, mir: &Mir, h: &crate::mir::HandlerMir) {
    use crate::mir::Stmt;

    let trans_name = safe_name(&format!("{}Transition", h.name));
    let param_sig = param_sig_str(&h.params);

    out.push_str(&format!(
        "def {} (s : State) (signer : Pubkey){} : Option State :=\n",
        trans_name, param_sig
    ));

    let Some((pre_name, post_name)) = h.transition.clone() else {
        out.push_str(
            "  -- todo!(): handler has no declared pre-variant; emitting structural rejection.\n",
        );
        out.push_str("  none\n\n");
        return;
    };
    let pre = match mir.state.variants.iter().find(|v| v.tag == pre_name) {
        Some(p) => p,
        None => {
            out.push_str(&format!(
                "  -- todo!(): unknown pre-variant `{}` in spec\n  none\n\n",
                pre_name
            ));
            return;
        }
    };
    let post = match mir.state.variants.iter().find(|v| v.tag == post_name) {
        Some(p) => p,
        None => {
            out.push_str(&format!(
                "  -- todo!(): unknown post-variant `{}` in spec\n  none\n\n",
                post_name
            ));
            return;
        }
    };

    // Pre-variant pattern with field-name bindings.
    let pre_pat = if pre.fields.is_empty() {
        format!(".{}", pre.tag)
    } else {
        let bindings: Vec<String> = pre.fields.iter().map(|f| safe_name(&f.name)).collect();
        format!(".{} {}", pre.tag, bindings.join(" "))
    };

    // Auth alias: when `auth <who>` is not a pre-variant field, bind
    // `who := signer` so downstream references resolve. When it IS a
    // pre field, the variant-arm binding scopes it and we emit a
    // signer-equality check instead.
    let mut pre_let_lines: Vec<String> = Vec::new();
    let mut cond_parts: Vec<String> = Vec::new();
    if let Some(auth_name) = handler_auth_name(h) {
        let pre_has_who = pre.fields.iter().any(|f| f.name == auth_name);
        if pre_has_who {
            cond_parts.push(format!("signer = {}", safe_name(&auth_name)));
        } else {
            pre_let_lines.push(format!("    let {} := signer", safe_name(&auth_name)));
        }
    }

    // User-declared requires (both `pre` and `requires_or_abort`
    // — combined here so the original spec ordering survives).
    // Filter out clauses that mention a handler-account `.pubkey` /
    // `.key()` projection: those identifiers have no Lean scope, so
    // including them produces theorem statements with free
    // variables. Mirrors `lean_gen::render_transition_adt:316-321`.
    for p in &h.pre {
        if mentions_handler_account_pubkey(&p.0.lean, &h.accounts) {
            continue;
        }
        cond_parts.push(p.0.lean.clone());
    }
    for r in &h.requires_or_abort {
        if mentions_handler_account_pubkey(&r.pred.0.lean, &h.accounts) {
            continue;
        }
        cond_parts.push(r.pred.0.lean.clone());
    }

    // Effect-derived bound checks. Only `CheckedAdd` / `CheckedSub`
    // gain bounds — `Wrap*` / `Sat*` handle the boundary without
    // aborting. Lookup the field's type in the pre-variant so the
    // bound is correctly typed (unsigned add → overflow; unsigned sub
    // → underflow; signed types skip the check).
    for stmt in &h.body.stmts {
        match stmt {
            Stmt::CheckedAdd { path, delta, .. } => {
                let stripped = strip_variant_prefix(path, mir);
                if let Some(ty) = pre
                    .fields
                    .iter()
                    .find(|f| f.name == stripped)
                    .map(|f| &f.ty)
                {
                    if let Some(max) = ty_max_const(ty) {
                        cond_parts.push(format!(
                            "{} + {} \u{2264} {}",
                            safe_name(&stripped),
                            delta.lean,
                            max
                        ));
                    }
                }
            }
            Stmt::CheckedSub { path, delta, .. } => {
                let stripped = strip_variant_prefix(path, mir);
                if let Some(ty) = pre
                    .fields
                    .iter()
                    .find(|f| f.name == stripped)
                    .map(|f| &f.ty)
                {
                    if !matches!(ty, crate::mir::Ty::I64 | crate::mir::Ty::I128) {
                        cond_parts.push(format!(
                            "{} \u{2264} {}",
                            delta.lean,
                            safe_name(&stripped)
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    // Build an effect map keyed by stripped field name. Account-
    // binding `.pubkey` assignments (no Lean scope) are skipped
    // — matches `lean_gen::render_transition_adt:355`.
    let mut effect_map: std::collections::HashMap<String, (&'static str, String)> =
        std::collections::HashMap::new();
    for stmt in &h.body.stmts {
        match stmt {
            Stmt::Assign { path, rhs } => {
                if is_account_pubkey_ref(&rhs.rust) {
                    continue;
                }
                effect_map.insert(strip_variant_prefix(path, mir), ("set", rhs.lean.clone()));
            }
            Stmt::CheckedAdd { path, delta, .. }
            | Stmt::WrapAdd { path, delta }
            | Stmt::SatAdd { path, delta } => {
                effect_map.insert(strip_variant_prefix(path, mir), ("add", delta.lean.clone()));
            }
            Stmt::CheckedSub { path, delta, .. }
            | Stmt::WrapSub { path, delta }
            | Stmt::SatSub { path, delta } => {
                effect_map.insert(strip_variant_prefix(path, mir), ("sub", delta.lean.clone()));
            }
            _ => {}
        }
    }

    // Build post-variant constructor args.
    let mut unconstrained: Vec<String> = Vec::new();
    let auth_who = handler_auth_name(h);
    let post_args: Vec<String> = post
        .fields
        .iter()
        .map(|f| {
            if let Some((kind, value)) = effect_map.get(&f.name) {
                return match *kind {
                    "add" => format!("({} + {})", safe_name(&f.name), value),
                    "sub" => format!("({} - {})", safe_name(&f.name), value),
                    _ => value.clone(),
                };
            }
            if let Some(ref who) = auth_who {
                if *who == f.name && matches!(&f.ty, crate::mir::Ty::Pubkey) {
                    return safe_name(who);
                }
            }
            if pre.fields.iter().any(|p| p.name == f.name && p.ty == f.ty) {
                return safe_name(&f.name);
            }
            unconstrained.push(f.name.clone());
            ty_default_literal(&f.ty).to_string()
        })
        .collect();

    let post_ctor = if post_args.is_empty() {
        format!(".{}", post.tag)
    } else {
        format!(".{} {}", post.tag, post_args.join(" "))
    };

    if !unconstrained.is_empty() {
        out.push_str(&format!(
            "  -- todo!(): post-variant `{}` has unconstrained field(s) not derivable from spec: {}\n",
            post.tag,
            unconstrained.join(", ")
        ));
        out.push_str(
            "  -- Using type defaults; add effects or handler params to constrain these.\n",
        );
    }

    out.push_str("  match s with\n");
    let let_block: String = if pre_let_lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", pre_let_lines.join("\n"))
    };
    if cond_parts.is_empty() {
        if pre_let_lines.is_empty() {
            out.push_str(&format!("  | {} => some ({})\n", pre_pat, post_ctor));
        } else {
            out.push_str(&format!("  | {} =>\n", pre_pat));
            out.push_str(&let_block);
            out.push_str(&format!("    some ({})\n", post_ctor));
        }
    } else {
        let if_cond = cond_parts
            .iter()
            .map(|p| paren_low_prec(p))
            .collect::<Vec<_>>()
            .join(" \u{2227} ");
        out.push_str(&format!("  | {} =>\n", pre_pat));
        if !pre_let_lines.is_empty() {
            out.push_str(&let_block);
        }
        out.push_str(&format!(
            "    if {} then some ({}) else none\n",
            if_cond, post_ctor
        ));
    }
    out.push_str("  | _ => none\n\n");
}

// ----------------------------------------------------------------------
// sBPF assembly renderer — ported verbatim from `lean_gen::render_sbpf`
// (v2.32 sBPF workstream). Reads `ParsedSpec` directly: the assembly
// domain (instructions / input_layout / insn_layout / guards over
// `executeFn`) has no representation in the state-machine `Stmt` IR, and
// Lean is the only backend that renders sBPF (Kani/proptest skip it), so
// there is no cross-codegen divergence for MIR to prevent. Output is
// byte-identical to the legacy renderer (gated by `tests/sbpf_lean_parity.rs`).
// ----------------------------------------------------------------------
fn render_sbpf(spec: &crate::check::ParsedSpec) -> String {
    let mut out = String::new();

    // Derive Prog module name from spec program_name.
    // E.g., spec Slippage → "SlippageProg", spec Transfer → "TransferProg"
    let prog_module = format!("{}Prog", spec.program_name);

    // Header
    out.push_str(&format!(
        "-- Generated by qedgen lean-gen from {}.qedspec\n\
         -- Source of truth: the .qedspec file. Regenerate with:\n\
         --   qedgen lean-gen --spec <spec>.qedspec --output <this-file>\n\n",
        spec.program_name.to_lowercase()
    ));

    out.push_str("import QEDGen\n");
    out.push_str(&format!("import {}\n\n", prog_module));

    out.push_str("open SVM.SBPF\n");
    out.push_str("open SVM.SBPF.Memory\n\n");

    // ── Global constants ─────────────────────────────────────────────────
    if !spec.constants.is_empty() {
        out.push_str("-- Global constants (from prog module, not re-declared):\n");
        for (name, val) in &spec.constants {
            let clean_val = val.replace('_', "");
            out.push_str(&format!("--   {} = {}\n", name, clean_val));
        }
        out.push('\n');
    }

    // ── Pubkey constants ───────────────────────────────────────────────────
    if !spec.pubkeys.is_empty() {
        out.push_str("-- Known pubkey constants (from prog module, not re-declared):\n");
        for pk in &spec.pubkeys {
            for (i, chunk) in pk.chunks.iter().enumerate() {
                let clean = chunk.replace('_', "");
                out.push_str(&format!(
                    "--   PUBKEY_{}_CHUNK_{} = {}\n",
                    pk.name.to_ascii_uppercase(),
                    i,
                    clean
                ));
            }
        }
        out.push('\n');
    }

    // ── Per-instruction blocks ───────────────────────────────────────────
    for instr in &spec.instructions {
        let ns = &instr.name;
        out.push_str(&format!("namespace {}\n\n", ns));

        // Instruction-level constants
        if !instr.constants.is_empty() {
            out.push_str("-- Instruction-level constants\n");
            for (name, val) in &instr.constants {
                let clean_val = val.replace('_', "");
                out.push_str(&format!("abbrev {} : Nat := {}\n", name, clean_val));
            }
            out.push('\n');
        }

        // Error constants — use instruction-level if present, else global
        let errors = if !instr.errors.is_empty() {
            &instr.errors
        } else {
            &spec.valued_errors
        };
        if !errors.is_empty() {
            out.push_str("-- Error constants\n");
            for err in errors {
                if let Some(val) = err.value {
                    let lean_name = error_to_lean_name(&err.name);
                    out.push_str(&format!("abbrev {} : Nat := {}\n", lean_name, val));
                }
            }
            out.push('\n');
        }

        // Offset constants (from input_layout + insn_layout)
        let all_offsets: Vec<(&str, &str, i64, bool)> = instr
            .input_layout
            .iter()
            .map(|f| (f.name.as_str(), f.field_type.as_str(), f.offset, false))
            .chain(
                instr
                    .insn_layout
                    .iter()
                    .map(|f| (f.name.as_str(), f.field_type.as_str(), f.offset, true)),
            )
            .collect();

        if !all_offsets.is_empty() {
            out.push_str("-- Offset constants\n");
            for (name, _ftype, offset, _is_insn) in &all_offsets {
                let lean_name = offset_to_lean_name(name);
                out.push_str(&format!("abbrev {} : Int := {}\n", lean_name, offset));
            }
            out.push('\n');

            // ea_* lemmas
            out.push_str("-- Effective address lemmas\n");
            for (name, _ftype, offset, _is_insn) in &all_offsets {
                let lean_name = offset_to_lean_name(name);
                let rhs = if *offset == 0 {
                    "b".to_string()
                } else if *offset > 0 {
                    format!("b + {}", offset)
                } else {
                    format!("b - {}", offset.unsigned_abs())
                };
                out.push_str(&format!(
                    "@[simp] theorem ea_{} (b : Nat) : effectiveAddr b {} = {} := by\n  \
                     unfold effectiveAddr {}; omega\n\n",
                    lean_name, lean_name, rhs, lean_name
                ));
            }
        }

        // Entry point
        let entry = instr.entry.unwrap_or(0);
        let has_insn_reg = !instr.insn_layout.is_empty();
        // qedsvm's initState takes a RegionTable; accesses outside it trap
        // to ERR_ACCESS_VIOLATION. Obligations bind `rt` and carry one
        // coverage hypothesis per derived read (`rt.containsRange ... = true`,
        // the SVM.SBPF.Patterns idiom) — with a symbolic inputAddr the check
        // cannot reduce by decidability, only by hypothesis rewrite.
        let init_expr = if has_insn_reg {
            format!("initState2 inputAddr insnAddr mem rt {}", entry)
        } else {
            "initState inputAddr mem rt".to_string()
        };

        // Guard theorem stubs
        if !instr.guards.is_empty() {
            out.push_str("-- Guard theorem stubs\n");
            out.push_str(
                "-- Hypotheses derived from checks + layout. Fill proofs with wp_exec.\n\n",
            );

            let mut accumulated_after: Vec<(String, String)> = Vec::new();

            for guard in &instr.guards {
                let error_lean = error_to_lean_name(&guard.error);
                let hyps = derive_guard_hypotheses(guard, &all_offsets, instr, spec);

                if let Some(ref doc) = guard.doc {
                    out.push_str(&format!("/-- {} -/\n", doc.trim()));
                }

                out.push_str(&format!("theorem {}\n", guard.name));

                if has_insn_reg {
                    out.push_str("    (inputAddr insnAddr : Nat) (mem : Mem) (rt : RegionTable)\n");
                } else {
                    out.push_str("    (inputAddr : Nat) (mem : Mem) (rt : RegionTable)\n");
                }

                for (var_decl, _) in &accumulated_after {
                    out.push_str(&format!("    {}\n", var_decl));
                }

                for hyp in &hyps.bindings {
                    out.push_str(&format!("    {}\n", hyp));
                }

                let fuel_str = match guard.fuel {
                    Some(f) => f.to_string(),
                    None => "FUEL".to_string(),
                };
                out.push_str(&format!(
                    "    :\n    (executeFn {}.progAt ({}) {}).exitCode\n      \
                     = some {} := sorry\n\n",
                    prog_module, init_expr, fuel_str, error_lean
                ));

                if let Some(ref after_hyps) = hyps.after {
                    for ah in after_hyps {
                        accumulated_after.push((ah.clone(), String::new()));
                    }
                }
            }

            // Spec completeness structure
            out.push_str(
                "-- Completeness structure: fill all fields to prove every guard is covered\n",
            );
            out.push_str("structure Spec (progAt : Nat \u{2192} Option Insn) where\n");

            let mut acc_after_for_spec: Vec<String> = Vec::new();
            for guard in &instr.guards {
                let error_lean = error_to_lean_name(&guard.error);
                let hyps = derive_guard_hypotheses(guard, &all_offsets, instr, spec);

                let mut binders = Vec::new();
                if has_insn_reg {
                    binders.push("(inputAddr insnAddr : Nat)".to_string());
                } else {
                    binders.push("(inputAddr : Nat)".to_string());
                }
                binders.push("(mem : Mem)".to_string());
                binders.push("(rt : RegionTable)".to_string());
                for ah in &acc_after_for_spec {
                    binders.push(prefix_unused_binder(ah));
                }
                for b in &hyps.bindings {
                    if !b.starts_with("--") {
                        binders.push(prefix_unused_binder(b));
                    }
                }

                let binder_str = binders.join(" ");
                let fuel_str = match guard.fuel {
                    Some(f) => f.to_string(),
                    None => "FUEL".to_string(),
                };
                out.push_str(&format!(
                    "  {} :\n    \u{2200} {},\n    \
                     (executeFn progAt ({}) {}).exitCode = some {}\n",
                    guard.name, binder_str, init_expr, fuel_str, error_lean
                ));

                if let Some(ref after_hyps) = hyps.after {
                    for ah in after_hyps {
                        acc_after_for_spec.push(ah.clone());
                    }
                }
            }
            out.push('\n');
        }

        // Property theorem stubs
        if !instr.properties.is_empty() {
            out.push_str("-- Property theorem stubs\n\n");
            for prop in &instr.properties {
                if let Some(ref doc) = prop.doc {
                    out.push_str(&format!("/-- {} -/\n", doc.trim()));
                }
                out.push_str(&format!("theorem {} : True := trivial\n\n", prop.name));
            }
        }

        out.push_str(&format!("end {}\n\n", ns));
    }

    out
}

/// Hypotheses derived from a guard's checks expression and the layout.
struct DerivedHypotheses {
    /// Lean hypothesis binders (e.g., "(disc : Nat)", "(h_disc_val : readU8 mem insnAddr = disc)")
    bindings: Vec<String>,
    /// After-hypotheses for the next guard (what becomes true if this guard passes)
    after: Option<Vec<String>>,
}

/// Region-coverage hypothesis for one derived read: under qedsvm semantics
/// the `ldx` at this address traps unless `rt` maps it, and with a symbolic
/// base address the check only closes by hypothesis rewrite.
fn region_hyp(var_name: &str, read_fn: &str, addr_expr: &str) -> String {
    let width = match read_fn {
        "readU8" => 1,
        "readU16" => 2,
        "readU32" => 4,
        _ => 8,
    };
    format!(
        "(h_rt_{} : rt.containsRange {} {} = true)",
        var_name, addr_expr, width
    )
}

/// Derive guard hypotheses from checks expression + input/insn layout.
fn derive_guard_hypotheses(
    guard: &crate::check::ParsedGuard,
    all_offsets: &[(&str, &str, i64, bool)],
    _instr: &crate::check::ParsedInstruction,
    _spec: &crate::check::ParsedSpec,
) -> DerivedHypotheses {
    // Use raw checks (preserves constant names) for Lean output
    let checks_str = guard.checks_raw.as_ref().or(guard.checks.as_ref());
    let Some(checks) = checks_str else {
        // No checks expression — generate minimal placeholder
        return DerivedHypotheses {
            bindings: vec!["-- TODO: add guard-specific hypotheses".to_string()],
            after: None,
        };
    };

    // Parse checks expression: "field == CONST" or "field >= CONST"
    // Support patterns: X == Y, X >= Y, X == Y (pubkey 4-chunk comparison)
    let parts: Vec<&str> = checks.split_whitespace().collect();

    if parts.len() == 3 {
        let field_name = parts[0];
        let op = parts[1];
        let const_name = parts[2];

        // Look up the field in layouts
        if let Some((_, ftype, offset, is_insn)) = all_offsets
            .iter()
            .find(|(name, _, _, _)| *name == field_name)
        {
            let read_fn = match *ftype {
                "U8" => "readU8",
                "U64" => "readU64",
                "Pubkey" => "readU64", // Pubkey fields are 4-chunk comparisons
                _ => "readU64",
            };

            let base_reg = if *is_insn { "insnAddr" } else { "inputAddr" };
            let addr_expr = if *offset == 0 {
                base_reg.to_string()
            } else if *offset > 0 {
                format!("({} + {})", base_reg, offset)
            } else {
                format!("({} - {})", base_reg, offset.unsigned_abs())
            };

            // Variable name: derive from field name
            let var_name = field_name_to_var(field_name);

            // Check if const_name is also a layout field (field-vs-field comparison)
            let rhs_is_field = all_offsets
                .iter()
                .find(|(name, _, _, _)| *name == const_name);

            // Build RHS: if it's a field, introduce a variable and read hypothesis for it
            let (rhs_var, rhs_bindings) = if let Some((_, rtype, roffset, r_is_insn)) = rhs_is_field
            {
                let rhs_read = match *rtype {
                    "U8" => "readU8",
                    _ => "readU64",
                };
                let rhs_base = if *r_is_insn { "insnAddr" } else { "inputAddr" };
                let rhs_addr = if *roffset == 0 {
                    rhs_base.to_string()
                } else if *roffset > 0 {
                    format!("({} + {})", rhs_base, roffset)
                } else {
                    format!("({} - {})", rhs_base, roffset.unsigned_abs())
                };
                let rhs_vname = field_name_to_var(const_name);
                let binds = vec![
                    format!("({} : Nat)", rhs_vname),
                    format!(
                        "(h_{}_val : {} mem {} = {})",
                        rhs_vname, rhs_read, rhs_addr, rhs_vname
                    ),
                    region_hyp(&rhs_vname, rhs_read, &rhs_addr),
                ];
                (rhs_vname, binds)
            } else {
                // RHS is a constant name (preserve as-is from checks_raw)
                (const_name.to_string(), vec![])
            };

            match op {
                "==" => {
                    let mut bindings = vec![
                        format!("({} : Nat)", var_name),
                        format!(
                            "(h_{}_val : {} mem {} = {})",
                            var_name, read_fn, addr_expr, var_name
                        ),
                        region_hyp(&var_name, read_fn, &addr_expr),
                    ];
                    bindings.extend(rhs_bindings.clone());
                    bindings.push(format!(
                        "(h_{}_ne : {} \u{2260} {})",
                        var_name, var_name, rhs_var
                    ));
                    // Later guards re-execute through this access: carry the
                    // pass-condition AND its region coverage forward.
                    let after = Some(vec![
                        format!(
                            "(h_{} : {} mem {} = {})",
                            var_name, read_fn, addr_expr, rhs_var
                        ),
                        region_hyp(&var_name, read_fn, &addr_expr),
                    ]);
                    DerivedHypotheses { bindings, after }
                }
                ">=" => {
                    let mut bindings = vec![
                        format!("({} : Nat)", var_name),
                        format!(
                            "(h_{}_val : {} mem {} = {})",
                            var_name, read_fn, addr_expr, var_name
                        ),
                        region_hyp(&var_name, read_fn, &addr_expr),
                    ];
                    bindings.extend(rhs_bindings.clone());
                    bindings.push(format!("(h_{}_lt : {} < {})", var_name, var_name, rhs_var));
                    let mut after_binds = vec![
                        format!("({} : Nat)", var_name),
                        format!(
                            "(h_{}_val : {} mem {} = {})",
                            var_name, read_fn, addr_expr, var_name
                        ),
                        region_hyp(&var_name, read_fn, &addr_expr),
                    ];
                    after_binds.extend(rhs_bindings);
                    after_binds.push(format!(
                        "(h_{}_ge : \u{00AC}({} < {}))",
                        var_name, var_name, rhs_var
                    ));
                    DerivedHypotheses {
                        bindings,
                        after: Some(after_binds),
                    }
                }
                _ => DerivedHypotheses {
                    bindings: vec![format!("-- TODO: derive hypotheses for checks: {}", checks)],
                    after: None,
                },
            }
        } else {
            // Field not found in layout — generate placeholder
            DerivedHypotheses {
                bindings: vec![format!("-- TODO: derive hypotheses for checks: {}", checks)],
                after: None,
            }
        }
    } else {
        // Complex expression — placeholder
        DerivedHypotheses {
            bindings: vec![format!("-- TODO: derive hypotheses for checks: {}", checks)],
            after: None,
        }
    }
}

/// Prefix hypothesis binder names (starting with `h_`) with `_` to suppress
/// unused-variable warnings in the Spec structure. Value variables like
/// `discriminant`, `nAccounts` etc. must keep their names because hypothesis
/// types reference them (e.g., `readU8 mem addr = discriminant`).
fn prefix_unused_binder(binder: &str) -> String {
    if let Some(rest) = binder.strip_prefix("(h_") {
        return format!("(_h_{}", rest);
    }
    binder.to_string()
}

/// Convert error name from qedspec to Lean constant name.
/// E.g., "InvalidDiscriminant" → "E_INVALID_DISCRIMINANT"
fn error_to_lean_name(name: &str) -> String {
    let mut result = String::from("E_");
    let mut prev_was_upper = false;
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() && i > 0 && !prev_was_upper {
            result.push('_');
        }
        result.push(c.to_ascii_uppercase());
        prev_was_upper = c.is_uppercase();
    }
    result
}

/// Convert layout field name to a Lean variable name.
fn field_name_to_var(name: &str) -> String {
    // Convert snake_case to camelCase for variable names
    let parts: Vec<&str> = name.split('_').collect();
    if parts.len() <= 1 {
        return name.to_string();
    }
    let mut result = parts[0].to_string();
    for part in &parts[1..] {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            result.push(first.to_ascii_uppercase());
            result.extend(chars);
        }
    }
    result
}

/// Convert offset field name to a Lean constant name.
/// Uses naming convention matching qedguards: uppercase with prefix.
fn offset_to_lean_name(name: &str) -> String {
    name.to_ascii_uppercase()
}

/// `(root_field, idx)` → `Vec<(inner_field, op_kind, value)>`.
/// Indexed-effect grouping used by `emit_indexed_transition` to
/// collapse multiple writes to the same `Map` slot into a single
/// `Function.update` call. Mirrors `lean_gen::IndexedEffectsByRoot`.
type IndexedEffectsByRoot =
    std::collections::BTreeMap<(String, String), Vec<(String, String, String)>>;

/// Map a scalar DSL type string to its Lean type. Mirrors
/// `lean_gen::map_scalar_type` (record fields carry string types, not the
/// typed `Ty`, so the indexed renderer needs this string-form mapper).
fn map_scalar_type(t: &str) -> String {
    match t.trim() {
        "U8" | "U16" | "U32" | "U64" | "U128" => "Nat".to_string(),
        "I8" | "I16" | "I32" | "I64" | "I128" => "Int".to_string(),
        "Bool" => "Bool".to_string(),
        "Pubkey" => "Pubkey".to_string(),
        other => other.to_string(),
    }
}

/// Default value for a record field's `Inhabited` instance. Mirrors
/// `lean_gen::default_value_for`.
fn default_value_for(t: &str) -> &'static str {
    match t.trim() {
        "U8" | "U16" | "U32" | "U64" | "U128" => "0",
        "I8" | "I16" | "I32" | "I64" | "I128" => "0",
        "Bool" => "false",
        _ => "default",
    }
}

/// Indexed-state Lean renderer — port of `lean_gen::render_indexed_state`.
///
/// Fires when any state field is `Ty::Map { .. }`. Indexed-state specs
/// (multisig, etc.) need Mathlib's `Fin`/`Function.update` machinery,
/// so the output shape diverges substantially from the flat /
/// ADT-State renderers:
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
fn render_indexed_state(mir: &Mir) -> String {
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
    // Skip a record literally named "State": v2.26's `type State = { ... }`
    // record-form lowering deposits the State record into `mir.records`
    // AND the State variant. The dedicated `structure State where` emission
    // below is the canonical source; emitting it twice produces a Lean
    // `redeclaration of State` error. The Account-style records this loop
    // targets are auxiliary records (Map value types). Mirrors
    // `lean_gen::render_indexed_state`'s record loop.
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
        emit_indexed_transition(&mut out, mir, h, &map_roots, emit_marker);
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
            let rewritten = rewrite_subscripts_lean(&expr.lean);
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
fn render_ty_indexed(ty: &crate::mir::Ty) -> String {
    use crate::mir::Ty;
    match ty {
        Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::U128 => "Nat".to_string(),
        Ty::I64 | Ty::I128 => "Int".to_string(),
        Ty::Bool => "Bool".to_string(),
        Ty::Pubkey => "Pubkey".to_string(),
        Ty::Custom(name) => name.clone(),
        Ty::Map { capacity, value } => {
            // Legacy emits the inner unchanged (e.g. `U8`, not `Nat`)
            // since indexed-state struct fields preserve the surface
            // type for the codegen's downstream Rust-side mirror.
            // Matches `render_indexed_state`'s state-field branch
            // which calls `map_scalar_type` for non-Map fields but
            // leaves the Map's inner type literal.
            let inner = match value.as_ref() {
                Ty::U8 => "U8".to_string(),
                Ty::U16 => "U16".to_string(),
                Ty::U32 => "U32".to_string(),
                Ty::U64 => "U64".to_string(),
                Ty::U128 => "U128".to_string(),
                Ty::I64 => "I64".to_string(),
                Ty::I128 => "I128".to_string(),
                Ty::Bool => "Bool".to_string(),
                Ty::Pubkey => "Pubkey".to_string(),
                Ty::Custom(n) => n.clone(),
                Ty::Map { .. } => render_ty_indexed(value),
            };
            format!("Map {} {}", capacity, inner)
        }
    }
}

/// Pick the constant name used to bound `AccountIdx`. Mirrors
/// `lean_gen::pick_account_idx_bound` — first `MAX_*` constant
/// declared, falling back to `MAX*`, then the literal `1024`. (The
/// `type AccountIdx = Fin[N]` alias path isn't lifted into MIR yet;
/// add when a fixture needs it.)
fn pick_account_idx_bound_mir(mir: &Mir) -> String {
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
fn collect_map_roots(mir: &Mir) -> std::collections::BTreeMap<String, String> {
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
/// `members[i].field`) into `(root, idx, inner_field)`. `inner_field`
/// is empty when the LHS targets the whole entry. Returns `None` if
/// the LHS lacks brackets. Mirrors `lean_gen::parse_indexed_lhs`.
fn parse_indexed_lhs(lhs: &str) -> Option<(&str, &str, &str)> {
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
/// Map indexes. Mirrors `lean_gen::infer_idx_promotions`.
fn infer_idx_promotions_mir(
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
            _ => continue,
        };
        if let Some((root, idx, _)) = parse_indexed_lhs(&lhs) {
            record(idx, root);
        }
    }

    // Requires expressions — `state.members[member_index] = approver`,
    // etc. The expression carrier is opaque; scan raw Lean form for
    // `<path>[<idx>]` patterns.
    for pred in &h.pre {
        scan_indexed_in_expr(&pred.0.lean, &mut record);
    }
    for stmt in &h.body.stmts {
        if let Stmt::RequireOrAbort { pred, .. } = stmt {
            scan_indexed_in_expr(&pred.0.lean, &mut record);
        }
    }

    result
}

/// Walk `expr` for `<root>[<idx>]` patterns. `record` is invoked once
/// per match with the bare root identifier (last `.` segment) and the
/// trimmed index string. Mirrors `lean_gen::scan_indexed_in_expr`.
fn scan_indexed_in_expr(expr: &str, record: &mut dyn FnMut(&str, &str)) {
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

/// Subscript rewriter — `state.members[i] = approver` →
/// `(s.members i) = approver`. Mirrors
/// `lean_gen::rewrite_subscripts_lean` byte-for-byte. Operates on a
/// pre-rendered Lean expression string (the opaque-expression
/// discipline applies here too).
fn rewrite_subscripts_lean(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut it = s.char_indices().peekable();
    while let Some((i, ch)) = it.next() {
        if ch != '[' {
            out.push(ch);
            continue;
        }
        let mut k = out.len();
        while k > 0 {
            let bytes = out.as_bytes();
            let c = bytes[k - 1] as char;
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                k -= 1;
            } else {
                break;
            }
        }
        let after = &s[i + 1..];
        let close_rel = match after.find(']') {
            Some(n) => n,
            None => {
                out.push(ch);
                continue;
            }
        };
        let idx = after[..close_rel].trim().to_string();
        let path: String = out[k..].to_string();
        out.truncate(k);
        out.push('(');
        out.push_str(&path);
        out.push(' ');
        out.push_str(&idx);
        out.push(')');
        let consumed_until = i + 1 + close_rel + 1;
        while let Some(&(p, _)) = it.peek() {
            if p < consumed_until {
                it.next();
            } else {
                break;
            }
        }
    }
    out
}

/// Render an effect RHS for the indexed-state transition body. Bare
/// numeric literals and bare param refs pass through; pre-rendered
/// Lean compounds (starting with `s.`, `(`, `match`, etc.) get
/// subscript rewriting only. Bare field names take an `s.` prefix
/// plus subscript rewriting. Mirrors `lean_gen::effect_value_to_lean`.
fn effect_value_to_lean_mir(
    value: &str,
    params: &[(crate::mir::Symbol, crate::mir::Ty)],
) -> String {
    let trimmed = value.trim();
    if !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| c.is_ascii_digit() || c == '_' || c == '-')
    {
        return trimmed.replace('_', "");
    }
    let is_bare_ident = trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_');
    if is_bare_ident && params.iter().any(|(n, _)| n == trimmed) {
        return trimmed.to_string();
    }
    let looks_prerendered = trimmed.starts_with("s.")
        || trimmed.starts_with("s'.")
        || trimmed.starts_with('(')
        || trimmed.contains("match ")
        || trimmed.contains("=> ")
        || trimmed.contains(" with ")
        || trimmed.contains(".{");
    if looks_prerendered {
        return rewrite_subscripts_lean(trimmed);
    }
    let first = trimmed.chars().next().unwrap_or('_');
    let prefixed = if first.is_ascii_alphabetic() || first == '_' {
        format!("s.{}", trimmed)
    } else {
        trimmed.to_string()
    };
    rewrite_subscripts_lean(&prefixed)
}

/// Emit one transition def for an indexed-state handler. Distinct
/// from `emit_handler_transition` (flat-state path) because:
///   * scalar param types lift to `Fin <bound>` when promoted;
///   * requires clauses are parenthesized as wholes;
///   * subscripted effects collapse into `Function.update` calls;
///   * NO auto over/under-flow guards (legacy behavior — indexed
///     transitions trust the surface DSL's bounds).
fn emit_indexed_transition(
    out: &mut String,
    mir: &Mir,
    h: &crate::mir::HandlerMir,
    map_roots: &std::collections::BTreeMap<String, String>,
    emit_marker: bool,
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
    // Requires clauses — emitted in ORIGINAL spec order via
    // `requires_in_order` (both `requires X else Err` and bare
    // `requires X`), matching legacy `render_indexed_state`'s
    // single-list iteration of `ParsedHandler.requires`. Iterating the
    // split `body RequireOrAbort` then `h.pre` instead reorders an
    // interleaved bare/with-err sequence (e.g. percolator's
    // match-arm-abort: the arm condition is bare, the abort marker
    // carries an error). Parenthesized as wholes; subscript-rewritten
    // so `state.members[i]` → `(s.members i)`.
    for pred in &h.requires_in_order {
        if mentions_handler_account_pubkey(&pred.0.lean, &h.accounts) {
            continue;
        }
        conds.push(format!("({})", rewrite_subscripts_lean(&pred.0.lean)));
    }

    // Effect updates.
    let mut scalar_parts: Vec<String> = Vec::new();
    // (root, idx) → Vec<(inner_field, op_kind, value)>
    let mut indexed_by_root: IndexedEffectsByRoot = std::collections::BTreeMap::new();

    for stmt in &h.body.stmts {
        let (path, op_kind, val) = match stmt {
            Stmt::Assign { path, rhs } => (path, "set", rhs.lean.as_str()),
            Stmt::CheckedAdd { path, delta, .. }
            | Stmt::WrapAdd { path, delta }
            | Stmt::SatAdd { path, delta } => (path, "add", delta.lean.as_str()),
            Stmt::CheckedSub { path, delta, .. }
            | Stmt::WrapSub { path, delta }
            | Stmt::SatSub { path, delta } => (path, "sub", delta.lean.as_str()),
            _ => continue,
        };
        // Drop `<field> := <account_binding>.pubkey` — no Lean scope
        // for account-binding pubkey refs.
        if op_kind == "set" && is_account_pubkey_ref(val) {
            continue;
        }
        // Reconstruct the full dotted LHS: an indexed-record-field write
        // lowers to a multi-segment path (`accounts[i].active` →
        // `["accounts[i]", "active"]`). Using only the first segment drops
        // the `.active` field, so `parse_indexed_lhs` would see an empty
        // inner-field and emit a whole-entry `Function.update … (val)`
        // instead of `{ (s.accounts i) with active := val }` (issue: the
        // record-field write would be silently lost / mis-typed).
        let lhs = path.segments.join(".");
        if let Some((root, idx, inner_field)) = parse_indexed_lhs(&lhs) {
            if map_roots.contains_key(root) {
                indexed_by_root
                    .entry((root.to_string(), idx.to_string()))
                    .or_default()
                    .push((
                        inner_field.to_string(),
                        op_kind.to_string(),
                        val.to_string(),
                    ));
                continue;
            }
        }
        // Plain scalar effect.
        let sf = safe_name(&lhs);
        let val_lean = effect_value_to_lean_mir(val, &h.params);
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
            let val_lean = rewrite_subscripts_lean(value);
            format!("Function.update s.{root} {idx} ({val})", val = val_lean)
        } else {
            let mut inner_updates: Vec<String> = Vec::new();
            for (fname, op_kind, value) in ops {
                let val_lean = effect_value_to_lean_mir(value, &h.params);
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
fn indexed_param_sig(
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

/// Emit `inductive Operation where | ctor (params) …` + the
/// `def applyOp` dispatcher for the indexed-state shape. The
/// Operation enum doesn't carry a `deriving` clause (matches legacy).
fn emit_indexed_operation_inductive(
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

/// v2.30 Phase 2 — multi-account renderer. Mirrors
/// `lean_gen::render_multi_account` (`crates/qedgen/src/lean_gen.rs`).
///
/// For each `type <Account>` declared in the spec, emits a separate
/// `<Account>State` structure, `<Account>Status` lifecycle inductive,
/// per-account transition functions, a per-account `<Account>Operation`
/// inductive + `apply<Account>Op` dispatcher. CPI theorems for handlers
/// owned by the account are interleaved with that account's block (same
/// ordering as the legacy renderer). Invariants are lowered as
/// structured comments (multi-account variant-typed binders need a
/// richer lowering — v3.0). Properties group by which account's fields
/// they touch; aborts / ensures / overflow emit per account. Covers /
/// liveness / environments bind to the primary account (covers whose
/// traces span accounts emit skip-comments).
///
/// Implementation strategy: build a per-account *scoped Mir* whose
/// `state` and `handlers` are filtered to that account, call the
/// existing single-account section emitters, then rewrite the bare
/// type/function identifiers (`State`, `Status`, `Operation`,
/// `applyOp`, `applyOps`) to their per-account form via
/// `rename_state_idents`. This keeps the multi-account port small
/// without duplicating every emitter.
fn render_multi_account(mir: &Mir) -> String {
    let mut out = String::new();
    emit_header(&mut out, mir);
    emit_namespace_open(&mut out, mir);
    emit_uninterpreted_helpers(&mut out, mir);
    emit_ref_impls(&mut out, mir);
    emit_constants(&mut out, mir);

    // Pass 1 — per-account: Status, State, Transitions, CPI theorems,
    // Operation + applyOp. Mirrors the first `for acct in &spec.account_types`
    // loop in `lean_gen::render_multi_account` lines 1334–1387.
    for acct in &mir.account_states {
        let scoped = scope_mir_to_account(mir, acct);
        if scoped.handlers.is_empty() {
            continue;
        }
        let mut block = String::new();
        emit_lifecycle_marker(&mut block, &scoped);
        emit_state_struct(&mut block, &scoped);
        emit_transitions(&mut block, &scoped);
        let _pinned = emit_cpi_theorems(&mut block, &scoped);
        emit_operation_inductive(&mut block, &scoped);
        out.push_str(&rename_state_idents(&block, &acct.name));
    }

    // Invariants — multi-account translation deferred. Emit as
    // structured comments to match `lean_gen::render_invariants_as_comments`
    // (lines 2390–2406).
    emit_invariants_as_comments(&mut out, mir);

    // Properties grouped by account ownership. Mirrors
    // `lean_gen::render_properties_multi` lines 2521–2601.
    emit_properties_multi(&mut out, mir);

    // Pass 2 — per-account: aborts_if, ensures, frame, overflow.
    // Mirrors `lean_gen::render_multi_account` lines 1405–1428.
    // Overflow needs each account's properties on the scoped Mir so the
    // `h_inv_<prop>` hypothesis threads correctly.
    let prop_groups = group_properties_by_account(mir);
    for acct in &mir.account_states {
        let mut scoped = scope_mir_to_account(mir, acct);
        if scoped.handlers.is_empty() {
            continue;
        }
        if let Some(props) = prop_groups.get(&acct.name) {
            scoped.properties = props.clone();
        }
        let mut block = String::new();
        emit_aborts_if(&mut block, &scoped);
        emit_ensures(&mut block, &scoped);
        emit_frame_conditions(&mut block, &scoped);
        emit_overflow(&mut block, &scoped);
        out.push_str(&rename_state_idents(&block, &acct.name));
    }

    // Spec-level covers: emit the section header when any covers
    // exist (matches legacy). Cross-account traces become skip-comments;
    // single-account traces emit through the regular cover-witness
    // machinery scoped to the primary account.
    let primary = &mir.account_states[0];
    let primary_scoped = scope_mir_to_account(mir, primary);
    {
        let mut tail = String::new();
        emit_covers_multi(&mut tail, mir, &primary_scoped);
        out.push_str(&rename_state_idents(&tail, &primary.name));
    }

    // Liveness — each `liveness <name> : <from> ~> <to> via [op1, ...]`
    // binds to the account that owns the via-ops (resolved via
    // `via_ops[0].on_account`). Matches `lean_gen::render_liveness`
    // line ~3910.
    emit_liveness_multi(&mut out, mir);

    // Environments — each property × environment cross emits its
    // preservation theorem against the account-scoped state type.
    emit_environments_multi(&mut out, mir);

    emit_namespace_close(&mut out, mir);
    out
}

/// Group properties by the account whose fields they touch. Same
/// heuristic as `emit_properties_multi` but returned as a map so the
/// pass-2 overflow theorems can re-use it.
fn group_properties_by_account(
    mir: &Mir,
) -> std::collections::BTreeMap<String, Vec<crate::mir::PropertyMir>> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<String, Vec<crate::mir::PropertyMir>> = BTreeMap::new();
    if mir.account_states.is_empty() {
        return groups;
    }
    let primary_name = mir.account_states[0].name.clone();
    for prop in &mir.properties {
        let target = if let Some(expr) = &prop.expression {
            mir.account_states
                .iter()
                .find(|a| {
                    a.fields
                        .iter()
                        .any(|f| expr.lean.contains(&format!("s.{}", f.name)))
                })
                .map(|a| a.name.clone())
                .unwrap_or_else(|| primary_name.clone())
        } else {
            primary_name.clone()
        };
        groups.entry(target).or_default().push(prop.clone());
    }
    groups
}

/// Per-liveness account resolution + section emit. The header is
/// emitted once at the top; each liveness then runs through the
/// existing single-account `emit_liveness` against a Mir scoped to its
/// owning account, with token renames applied to the per-liveness
/// block.
fn emit_liveness_multi(out: &mut String, mir: &Mir) {
    if mir.liveness_props.is_empty() || mir.account_states.is_empty() {
        return;
    }

    let by_handler: std::collections::HashMap<String, Option<String>> = mir
        .handlers
        .iter()
        .map(|h| (h.name.clone(), h.on_account.clone()))
        .collect();
    let primary_name = mir.account_states[0].name.clone();

    let resolve = |via_ops: &[String]| -> String {
        if let Some(first) = via_ops.first() {
            if let Some(Some(acct)) = by_handler.get(first) {
                return acct.clone();
            }
        }
        primary_name.clone()
    };

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Liveness properties \u{2014} bounded reachability (leads-to)\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    let mut emitted_helpers: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for liveness in &mir.liveness_props {
        let acct_name = resolve(&liveness.via_ops);
        let acct = match mir.account_states.iter().find(|a| a.name == acct_name) {
            Some(a) => a,
            None => continue,
        };
        let mut scoped = scope_mir_to_account(mir, acct);
        scoped.liveness_props = vec![liveness.clone()];

        let mut block = String::new();
        emit_liveness_inner_body(&mut block, &scoped, &mut emitted_helpers, &acct.name);
        out.push_str(&block);
    }
}

/// Emit the body of one liveness theorem against a scoped Mir,
/// tracking which `apply<Account>Ops` helpers we've already emitted so
/// we don't repeat them. The helper itself + the theorem are written
/// with bare `State` / `Operation` / `applyOp` identifiers, then
/// renamed in one pass at the end.
fn emit_liveness_inner_body(
    out: &mut String,
    scoped: &Mir,
    emitted_helpers: &mut std::collections::BTreeSet<String>,
    account_name: &str,
) {
    // Buffer raw output (still using bare identifiers) so we can rename
    // before pushing into the caller's `out`. The applyOps helper is
    // emitted at most once per account.
    let mut buf = String::new();
    let helper_key = format!("apply{}Ops", account_name);
    if !emitted_helpers.contains(&helper_key) {
        buf.push_str(
            "def applyOps (s : State) (signer : Pubkey) : List Operation \u{2192} Option State\n",
        );
        buf.push_str("  | [] => some s\n");
        buf.push_str("  | op :: ops => match applyOp s signer op with\n");
        buf.push_str("    | some s' => applyOps s' signer ops\n");
        buf.push_str("    | none => none\n\n");
        emitted_helpers.insert(helper_key);
    }

    // Reuse the existing liveness theorem emitter via a temporary
    // header-stripped wrapper. emit_liveness_inner emits its own
    // section header which we've already written, so we render to a
    // throwaway buffer and slice off the header lines.
    let mut tmp = String::new();
    emit_liveness_inner_no_header(&mut tmp, scoped, /* adt_form */ false);
    // emit_liveness_inner_no_header skips both the section header AND
    // the always-emitted applyOps helper; we manage the helper above.
    buf.push_str(&tmp);

    out.push_str(&rename_state_idents(&buf, account_name));
}

/// Body of `emit_liveness_inner` minus the section header and the
/// helper-emit (those are handled by `emit_liveness_multi` /
/// `emit_liveness_inner_body`). Inline copy of the per-theorem block
/// from `emit_liveness_inner` (line ~3844). When the legacy auto-
/// discharge script lands, this stays in sync.
fn emit_liveness_inner_no_header(out: &mut String, mir: &Mir, adt_form: bool) {
    for liveness in &mir.liveness_props {
        let bound = liveness.within_steps.unwrap_or(10);
        out.push_str(&format!(
            "/-- {} \u{2014} from {} leads to {} within {} steps via [{}]. -/\n",
            liveness.name,
            liveness.from_state,
            liveness.leads_to_state,
            bound,
            liveness.via_ops.join(", ")
        ));
        out.push_str(&format!(
            "theorem liveness_{} (s : State) (signer : Pubkey)\n",
            liveness.name
        ));
        out.push_str(&format!(
            "    (h : s.status = .{}) :\n",
            liveness.from_state
        ));
        if adt_form {
            out.push_str(&format!(
                "    \u{2203} ops, ops.length \u{2264} {} \u{2227} \u{2200} s', applyOps s signer ops = some s' \u{2192} s'.status = .{} := by sorry\n\n",
                bound, liveness.leads_to_state
            ));
            continue;
        }
        let path = find_liveness_path(
            &liveness.from_state,
            &liveness.leads_to_state,
            &liveness.via_ops,
            &mir.handlers,
        );
        if let Some(ref ops_path) = path {
            let proof = liveness_proof_script(ops_path, &mir.handlers);
            out.push_str(&format!(
                "    \u{2203} ops, ops.length \u{2264} {} \u{2227} \u{2200} s', applyOps s signer ops = some s' \u{2192} s'.status = .{}{}\n",
                bound, liveness.leads_to_state, proof
            ));
        } else {
            out.push_str(&format!(
                "    \u{2203} ops s', ops.length \u{2264} {} \u{2227} applyOps s signer ops = some s' \u{2227} s'.status = .{} := by sorry\n\n",
                bound, liveness.leads_to_state
            ));
        }
    }
}

/// Multi-account environment emit. Each property × environment cross
/// emits a preservation theorem against the property's owning account.
/// Mirrors the structure of `emit_environments` but groups by the
/// account whose fields the property touches; the legacy single-call
/// `render_environments(out, spec, primary)` works for lending only
/// because `pool_solvency` happens to bind to the primary, but the
/// shape generalizes.
fn emit_environments_multi(out: &mut String, mir: &Mir) {
    if mir.environments.is_empty() || mir.properties.is_empty() {
        return;
    }

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Environment \u{2014} properties hold under external state changes\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    let groups = group_properties_by_account(mir);
    for acct in &mir.account_states {
        let props = match groups.get(&acct.name) {
            Some(p) => p,
            None => continue,
        };
        let mut scoped = scope_mir_to_account(mir, acct);
        scoped.properties = props.clone();
        scoped.environments = mir.environments.clone();
        let mut block = String::new();
        emit_environments_no_header(&mut block, &scoped);
        out.push_str(&rename_state_idents(&block, &acct.name));
    }
}

/// Body of `emit_environments` minus the section header (already
/// written by `emit_environments_multi`). Keeps the rest of the
/// per-theorem rendering in lockstep with the single-account path,
/// including the bare-field-name rewrite that the spec's `constraint
/// <field> > 0` form needs.
fn emit_environments_no_header(out: &mut String, mir: &Mir) {
    for env in &mir.environments {
        for prop in &mir.properties {
            let prop_expr = match &prop.expression {
                Some(e) => e,
                None => continue,
            };
            let param_sig: String = env
                .mutates
                .iter()
                .map(|(name, ty)| format!(" (new_{} : {})", name, render_ty(ty)))
                .collect();

            let constraint_hyps: String = env
                .constraints
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let mut expr = c.0.lean.clone();
                    for (field, _) in &env.mutates {
                        expr = expr
                            .replace(&format!("s.{}", field), &format!("new_{}", field))
                            .replace(&format!("state.{}", field), &format!("new_{}", field));
                        // Bare field-name reference (e.g.
                        // `constraint interest_rate > 0`). Use word
                        // boundary so `interest_rate_pct` isn't
                        // captured by `interest_rate`.
                        let pat = format!(r"\b{}\b", regex::escape(field));
                        let re = regex::Regex::new(&pat).expect("static regex");
                        expr = re
                            .replace_all(&expr, regex::NoExpand(&format!("new_{}", field)))
                            .into_owned();
                    }
                    format!("\n    (h_c{} : {})", i, expr)
                })
                .collect();

            let with_parts: String = env
                .mutates
                .iter()
                .map(|(name, _)| format!("{} := new_{}", safe_name(name), name))
                .collect::<Vec<_>>()
                .join(", ");

            out.push_str(&format!(
                "theorem {}_under_{} (s : State){}{}\n",
                prop.name, env.name, param_sig, constraint_hyps
            ));
            out.push_str(&format!("    (h_inv : {} s) :\n", prop.name));

            let mutated_overlap = env.mutates.iter().any(|(field, _)| {
                prop_expr.lean.contains(&format!("s.{}", safe_name(field)))
                    || prop_expr.lean.contains(&format!("state.{}", field))
            });

            if !mutated_overlap {
                out.push_str(&format!(
                    "    {} {{ s with {} }} := by\n  unfold {} at h_inv \u{22A2}; dsimp; exact h_inv\n\n",
                    prop.name, with_parts, prop.name
                ));
            } else {
                out.push_str(&format!(
                    "    {} {{ s with {} }} := sorry\n\n",
                    prop.name, with_parts
                ));
            }
        }
    }
}

/// Build a Mir whose `state` is a single `StateAdt` derived from the
/// given account, whose `handlers` are filtered to those targeting
/// this account (per-handler `on_account` match, with the primary
/// account also collecting handlers that didn't qualify). Used by
/// `render_multi_account` to drive the existing single-account
/// emitters per-account.
fn scope_mir_to_account(mir: &Mir, acct: &crate::mir::AccountStateMir) -> Mir {
    let is_primary = mir
        .account_states
        .first()
        .map(|a| a.name == acct.name)
        .unwrap_or(false);

    let handlers: Vec<crate::mir::HandlerMir> = mir
        .handlers
        .iter()
        .filter(|h| match &h.on_account {
            Some(name) => name == &acct.name,
            None => is_primary,
        })
        .cloned()
        .collect();

    // Build a StateAdt for this account: variants from the ADT decl
    // (when present), else a synthetic single-variant carrying the
    // flat-record fields. lifecycle_states drives the `Status` emit.
    let state = if !acct.variants.is_empty() {
        crate::mir::StateAdt {
            variants: acct.variants.clone(),
            lifecycle_states: acct.lifecycle_states.clone(),
        }
    } else {
        crate::mir::StateAdt {
            variants: vec![crate::mir::StateVariant {
                tag: acct.name.clone(),
                fields: acct.fields.clone(),
            }],
            lifecycle_states: acct.lifecycle_states.clone(),
        }
    };

    Mir {
        name: mir.name.clone(),
        state,
        // Single-account view — scoped emitters that re-enter the
        // dispatch (none do today, but keep is_multi_account honest).
        account_states: vec![acct.clone()],
        accounts: mir.accounts.clone(),
        errors: mir.errors.clone(),
        imports: mir.imports.clone(),
        handlers,
        invariants: Vec::new(), // emit_invariants_as_comments handles
        events: mir.events.clone(),
        constants: Vec::new(),             // already emitted at top
        uninterpreted_helpers: Vec::new(), // already emitted
        ref_impls: Vec::new(),             // already emitted
        properties: Vec::new(),            // emit_properties_multi handles
        covers: Vec::new(),                // emit_covers_multi handles
        liveness_props: mir.liveness_props.clone(),
        environments: mir.environments.clone(),
        ghosts: mir.ghosts.clone(),
        records: mir.records.clone(),
        is_assembly: mir.is_assembly,
        adt_state: mir.adt_state,
    }
}

/// Rewrite bare type / function identifiers (`State`, `Status`,
/// `Operation`, `applyOp`, `applyOps`) to their per-account form
/// (`PoolState`, `PoolStatus`, `PoolOperation`, `applyPoolOp`,
/// `applyPoolOps`). Word-boundary regex protects field names that
/// happen to share a prefix.
///
/// Safe because the renamed identifiers are emitter-internal type and
/// function names: spec field names are lowercase by convention, and
/// the type names (`State`, `Status`, `Operation`) never appear as
/// values inside Lean expressions emitted by these helpers.
fn rename_state_idents(text: &str, account_name: &str) -> String {
    let renames: [(&str, String); 5] = [
        (r"\bapplyOps\b", format!("apply{}Ops", account_name)),
        (r"\bapplyOp\b", format!("apply{}Op", account_name)),
        (r"\bOperation\b", format!("{}Operation", account_name)),
        (r"\bStatus\b", format!("{}Status", account_name)),
        (r"\bState\b", format!("{}State", account_name)),
    ];

    let mut out = text.to_string();
    for (pat, replacement) in &renames {
        let re = regex::Regex::new(pat).expect("static regex");
        out = re
            .replace_all(&out, regex::NoExpand(replacement))
            .into_owned();
    }
    out
}

/// Emit declared invariants as structured comments — matches
/// `lean_gen::render_invariants_as_comments`. Multi-account variant-
/// typed invariant bodies (e.g. `forall l : Loan.Active, …`) need a
/// richer lowering pass (v3.0); comments preserve the declared name +
/// body for visibility.
fn emit_invariants_as_comments(out: &mut String, mir: &Mir) {
    for inv in &mir.invariants {
        out.push_str(&format!(
            "-- INVARIANT OBLIGATION (declared, multi-account translation deferred): {}\n",
            inv.name
        ));
        if let Some(body) = &inv.body {
            out.push_str(&format!("--   predicate body: {}\n", body.0.lean));
        }
        if !inv.doc.is_empty() {
            out.push_str(&format!("--   description: {}\n", inv.doc));
        }
        out.push_str("-- v2.14 emits this as a comment; multi-account invariant\n");
        out.push_str("-- bodies (e.g. `forall l : Loan.Active, ...`) need lowering\n");
        out.push_str("-- to typed-state-with-status-filter form. v2.15 picks it up.\n\n");
    }
}

/// Group properties by which account's fields they reference, then
/// emit each group through the per-account scoped path. Mirrors
/// `lean_gen::render_properties_multi`.
fn emit_properties_multi(out: &mut String, mir: &Mir) {
    use std::collections::BTreeMap;

    if mir.properties.is_empty() || mir.account_states.is_empty() {
        return;
    }

    let mut groups: BTreeMap<String, Vec<crate::mir::PropertyMir>> = BTreeMap::new();
    let primary_name = mir.account_states[0].name.clone();

    for prop in &mir.properties {
        let target = if let Some(expr) = &prop.expression {
            mir.account_states
                .iter()
                .find(|a| {
                    a.fields
                        .iter()
                        .any(|f| expr.lean.contains(&format!("s.{}", f.name)))
                })
                .map(|a| a.name.clone())
                .unwrap_or_else(|| primary_name.clone())
        } else {
            primary_name.clone()
        };
        groups.entry(target).or_default().push(prop.clone());
    }

    for (acct_name, props) in groups {
        let acct = mir
            .account_states
            .iter()
            .find(|a| a.name == acct_name)
            .expect("account_states contains group key");
        let mut scoped = scope_mir_to_account(mir, acct);
        scoped.properties = props;
        let mut block = String::new();
        emit_properties(&mut block, &scoped);
        out.push_str(&rename_state_idents(&block, &acct.name));
    }
}

/// Emit cover trace theorems, skipping any whose handler sequence
/// targets more than one account. The skipped traces emit a structured
/// comment so the spec author can see the obligation was dropped.
/// Matches the multi-account skip behavior of
/// `lean_gen::render_covers` (which sees `state_type = primary` and
/// the legacy multi-account skip-comment).
fn emit_covers_multi(out: &mut String, mir: &Mir, primary_scoped: &Mir) {
    if mir.covers.is_empty() {
        return;
    }

    let by_handler: std::collections::HashMap<String, Option<String>> = mir
        .handlers
        .iter()
        .map(|h| (h.name.clone(), h.on_account.clone()))
        .collect();
    let primary_name = mir.account_states.first().map(|a| a.name.clone());

    // Section header always written when any covers exist (legacy emits
    // the same header even if every trace ends up as a skip-comment).
    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Cover properties \u{2014} reachability (existential proofs)\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    let mut kept = Vec::new();
    for c in &mir.covers {
        let mut spans_multi = false;
        'outer: for trace in &c.traces {
            let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for op in trace {
                let acct = by_handler.get(op).and_then(|o| o.clone()).or_else(|| {
                    if by_handler.contains_key(op) {
                        primary_name.clone()
                    } else {
                        None
                    }
                });
                if let Some(a) = acct {
                    seen.insert(a);
                }
            }
            if seen.len() > 1 {
                spans_multi = true;
                break 'outer;
            }
        }
        if spans_multi {
            let label: String = c
                .traces
                .first()
                .map(|t| format!("[{}]", t.join(", ")))
                .unwrap_or_else(|| "[]".to_string());
            out.push_str(&format!(
                "-- cover_{}: trace {} spans multiple account types, skipped\n\n",
                c.name, label
            ));
        } else {
            kept.push(c.clone());
        }
    }

    if !kept.is_empty() {
        let mut scoped = primary_scoped.clone();
        scoped.covers = kept;
        // emit_covers writes its own section header; we've already
        // emitted it. Render to a buffer and strip the duplicate
        // header block (3 lines).
        let mut buf = String::new();
        emit_covers(&mut buf, &scoped);
        let stripped: String = buf
            .lines()
            .skip_while(|l| {
                l.starts_with("-- ===") || l.contains("Cover properties") || l.is_empty()
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !stripped.is_empty() {
            out.push_str(&stripped);
            out.push('\n');
        }
    }
}

// ----------------------------------------------------------------------
// Section emitters (called from render_single_account)
// ----------------------------------------------------------------------

fn emit_header(out: &mut String, _mir: &Mir) {
    out.push_str("import QEDGen.Solana.Account\n");
    out.push_str("import QEDGen.Solana.Cpi\n");
    out.push_str("import QEDGen.Solana.State\n");
    out.push_str("import QEDGen.Solana.Valid\n\n");
}

fn emit_namespace_open(out: &mut String, mir: &Mir) {
    out.push_str(&format!("namespace {}\n\n", mir.name));
    out.push_str("open QEDGen.Solana\n\n");
}

fn emit_namespace_close(out: &mut String, mir: &Mir) {
    out.push_str(&format!("end {}\n", mir.name));
}

/// Emit `abbrev NAME : Nat := VALUE` lines for top-level constants.
/// Mirrors `lean_gen::render_single_account` line ~1203.
fn emit_constants(out: &mut String, mir: &Mir) {
    if mir.constants.is_empty() {
        return;
    }
    for (name, val) in &mir.constants {
        out.push_str(&format!("abbrev {} : Nat := {}\n", safe_name(name), val));
    }
    out.push('\n');
}

/// Emit uninterpreted-helper declarations.
/// Mirrors `lean_gen::emit_uninterpreted_helpers`.
///
/// Each helper becomes a Lean `opaque <name> : T1 → T2 → ... → R`
/// declaration. `opaque` (not `axiom`) so transition functions stay
/// computable.
fn emit_uninterpreted_helpers(out: &mut String, mir: &Mir) {
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

/// Emit `def`-style reference-implementation declarations.
/// Mirrors `lean_gen::emit_ref_impls`.
///
/// `ref_impl` bodies are emitted as Lean `def`s. Map-indexed subscripts
/// (`m[i]`) in the Lean body get rewritten to function-application
/// form (`(m i)`) since `Map N T = Fin N → T` doesn't have a GetElem
/// instance — handled by a small rewrite pass that lean_gen.rs ships
/// as `rewrite_subscripts_lean`. For Phase 1c-4 we emit the body
/// verbatim; the subscript-rewrite port lands in a follow-up if any
/// pilot fixture trips on it.
fn emit_ref_impls(out: &mut String, mir: &Mir) {
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
        // Phase 1c-4 emits the lean_body verbatim. lean_gen.rs runs a
        // `rewrite_subscripts_lean` pass over Map-indexed expressions
        // (`m[i]` → `(m i)`); port when a pilot fixture needs it.
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

/// Map a DSL type-string to its Lean form. Mirrors
/// `lean_gen::map_type_with_compound` for the common cases used by
/// `ref_impl` parameter / return types. Unknown forms pass through
/// unchanged — Phase 1c-4 doesn't try for compound-type support
/// (`Map[N] T`, `Fin[N]`, ...). A future slice can port the legacy's
/// compound-aware mapper when a fixture demands it.
fn map_dsl_ty(s: &str) -> String {
    match s.trim() {
        "U8" | "U16" | "U32" | "U64" | "U128" => "Nat".to_string(),
        "I8" | "I16" | "I32" | "I64" | "I128" => "Int".to_string(),
        other => other.to_string(),
    }
}

fn emit_lifecycle_marker(out: &mut String, mir: &Mir) {
    // lean_gen.rs:1216 — emit `inductive Status` if the lifecycle has
    // ≥ 2 states. Issue #43: a single-state lifecycle is no
    // discriminator; emitting Status for it collides with user-declared
    // `status` fields.
    //
    // Flat-path shape: bare variant tags (no `: Status` annotation) and
    // `deriving Repr, DecidableEq, BEq` to match `lean_gen::render_single_account`.
    // Distinct from the multi-variant ADT path's `emit_status_inductive_adt`
    // — same shape (deriving order) but the flat path doesn't need to share
    // the helper since the State block itself differs.
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

/// Emit one transition function per handler. Mirrors
/// `lean_gen::render_transitions` for the pilot scope:
///
/// ```text
/// def <name>Transition (s : State) (signer : Pubkey) <params> : Option State :=
///   let <auth_alias> := signer  -- when `auth <who>` isn't a state field
///   if <require-or-abort-conjunction> then
///     some { s with <assigns>... }
///   else
///     none
/// ```
///
/// Pilot scope: lowers `Stmt::RequireOrAbort` into the if-condition;
/// `Stmt::Assign` / `CheckedAdd` / `CheckedSub` / `WrapAdd` / etc. into
/// the `{ s with ... }` record update. `TokenTransfer` and `Cpi`
/// don't affect local state — they're discharged by separate CPI
/// theorems (Phase 1c-later slice).
fn emit_transitions(out: &mut String, mir: &Mir) {
    for h in &mir.handlers {
        emit_handler_transition(out, mir, h);
    }
}

fn emit_handler_transition(out: &mut String, mir: &Mir, h: &crate::mir::HandlerMir) {
    use crate::mir::Stmt;

    let trans_name = safe_name(&format!("{}Transition", h.name));
    let param_sig = param_sig_str(&h.params);

    // Signature.
    out.push_str(&format!(
        "def {} (s : State) (signer : Pubkey){} : Option State :=\n",
        trans_name, param_sig
    ));

    let conds = build_guard_cond_parts(mir, h);

    // Auth alias: `let <who> := signer` only when `who` is NOT a state
    // field (legacy behavior; otherwise the conjunct above already
    // pins the relationship and an alias would shadow the field name).
    if let Some(who) = handler_auth_name(h) {
        let state_fields = flat_state_fields(mir);
        let who_is_state_field = state_fields.iter().any(|(n, _)| n == &who);
        if !who_is_state_field {
            out.push_str(&format!("  let {} := signer\n", safe_name(&who)));
        }
    }

    // Assign / Add / Sub family → record-update parts.
    let mut with_parts: Vec<String> = Vec::new();
    for stmt in &h.body.stmts {
        match stmt {
            Stmt::Assign { path, rhs } => {
                // Drop `<field> := <account_binding>.pubkey` — the
                // mirror behavior from lean_gen.rs:1839; account-binding
                // pubkey refs have no Lean scope.
                if is_account_pubkey_ref(&rhs.rust) {
                    continue;
                }
                // Re-qualify the RHS: a bare state-field ref (e.g.
                // `reserved := state.cap`, which upstream strips to `cap`)
                // must render as `s.cap`, while handler params and literals
                // pass through unchanged. The indexed-state path already
                // routes through this helper; the flat path used to emit
                // `rhs.lean` verbatim, producing `reserved := cap`
                // (unknown identifier `cap`) — issue #79 Bug A.
                with_parts.push(format!(
                    "{} := {}",
                    safe_name(&path_field_name(path)),
                    effect_value_to_lean_mir(&rhs.lean, &h.params)
                ));
            }
            Stmt::CheckedAdd { path, delta, .. }
            | Stmt::WrapAdd { path, delta }
            | Stmt::SatAdd { path, delta } => {
                let f = safe_name(&path_field_name(path));
                let d = effect_value_to_lean_mir(&delta.lean, &h.params);
                with_parts.push(format!("{} := s.{} + {}", f, f, d));
            }
            Stmt::CheckedSub { path, delta, .. }
            | Stmt::WrapSub { path, delta }
            | Stmt::SatSub { path, delta } => {
                let f = safe_name(&path_field_name(path));
                let d = effect_value_to_lean_mir(&delta.lean, &h.params);
                with_parts.push(format!("{} := s.{} - {}", f, f, d));
            }
            _ => {}
        }
    }

    // Lifecycle promotion: `state := .NextVariant` lowers as
    // `status := .NextVariant` on the lifecycle marker field. For
    // pilot fixtures, the transition arrow on HandlerMir.transition
    // captures this; emit the post-status set when present.
    if let Some((_, post)) = &h.transition {
        // Only emit the `status :=` part when lifecycle is real
        // (≥2 states); single-lifecycle specs skip the marker per
        // issue #43.
        if mir.state.lifecycle_states.len() >= 2 {
            with_parts.push(format!("status := .{}", safe_name(post)));
        }
    }

    // Issue #67 item 3 — ghost field updates. A ghost with an `on <this
    // handler>` clause assigns its new value alongside the normal effects;
    // ghosts without a clause are left unchanged (the `{ s with … }`
    // record update preserves unmentioned fields — the frame condition).
    for ghost in &mir.ghosts {
        if let Some(val) = ghost.updates.get(&h.name) {
            with_parts.push(format!("{} := {}", safe_name(&ghost.name), val.lean));
        }
    }

    let then_body = if with_parts.is_empty() {
        "some s".to_string()
    } else {
        format!("some {{ s with {} }}", with_parts.join(", "))
    };

    if conds.is_empty() {
        out.push_str(&format!("  {}\n\n", then_body));
    } else {
        let joined = conds
            .iter()
            .map(|c| paren_low_prec(c))
            .collect::<Vec<_>>()
            .join(" \u{2227} ");
        out.push_str(&format!("  if {} then\n", joined));
        out.push_str(&format!("    {}\n", then_body));
        out.push_str("  else none\n\n");
    }
}

/// Build the if-condition conjuncts for a handler's flat-state
/// transition function. Mirrors `lean_gen::build_guard_cond_parts`
/// exactly; emit-site users (transition body, aborts theorem proof
/// indexing) share the same conjunct list.
///
/// Order (and content) matches legacy so the resulting `if` line,
/// `cond_parts.iter().position(...)` lookups, and conjunction-
/// projection paths are byte-equivalent:
///   1. `signer = s.<who>`  (only if `who` names a state field)
///   2. `s.status = .<pre>` (lifecycle gate)
///   3. sub-effect underflow guards (`<delta> ≤ s.<field>`, unsigned only)
///   4. RequireOrAbort predicates (filtered: skip handler-account pubkey refs)
///   5. add-effect overflow guards (`s.<field> + <delta> ≤ <max>`, unsigned only)
fn build_guard_cond_parts(mir: &Mir, h: &crate::mir::HandlerMir) -> Vec<String> {
    use crate::mir::Stmt;
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
        if mir.state.lifecycle_states.len() >= 2 {
            conds.push(format!("s.status = .{}", safe_name(pre)));
        }
    }

    for stmt in &h.body.stmts {
        let (path, delta) = match stmt {
            Stmt::CheckedSub { path, delta, .. }
            | Stmt::WrapSub { path, delta }
            | Stmt::SatSub { path, delta } => (path, delta),
            _ => continue,
        };
        let field = path_field_name(path);
        if let Some(ty) = state_fields
            .iter()
            .find(|(n, _)| n == &field)
            .map(|(_, t)| t.clone())
        {
            if ty_max_const(&ty).is_some() {
                let d = effect_value_to_lean_mir(&delta.lean, &h.params);
                conds.push(format!("{} \u{2264} s.{}", d, safe_name(&field)));
            }
        }
    }

    for stmt in &h.body.stmts {
        if let Stmt::RequireOrAbort { pred, .. } = stmt {
            if mentions_handler_account_pubkey(&pred.0.lean, &h.accounts) {
                continue;
            }
            conds.push(pred.0.lean.clone());
        }
    }

    for stmt in &h.body.stmts {
        let (path, delta) = match stmt {
            Stmt::CheckedAdd { path, delta, .. }
            | Stmt::WrapAdd { path, delta }
            | Stmt::SatAdd { path, delta } => (path, delta),
            _ => continue,
        };
        let field = path_field_name(path);
        let ty = match state_fields
            .iter()
            .find(|(n, _)| n == &field)
            .map(|(_, t)| t.clone())
        {
            Some(t) => t,
            None => continue,
        };
        let max = match ty_max_const(&ty) {
            Some(m) => m,
            None => continue,
        };
        let sf = safe_name(&field);
        let needle_a = format!("s.{} + {}", sf, delta.lean);
        let needle_b = format!("{} + s.{}", delta.lean, sf);
        let already = conds
            .iter()
            .any(|c| c.contains(&needle_a) || c.contains(&needle_b));
        if already {
            continue;
        }
        conds.push(format!("s.{} + {} \u{2264} {}", sf, delta.lean, max));
    }

    conds
}

/// Union of (field-name, type) pairs across every state variant —
/// matches the flat-state `emit_state_struct` projection. Used by
/// `emit_handler_transition` to look up field types for the auth
/// gate and the auto overflow/underflow guards.
fn flat_state_fields(mir: &Mir) -> Vec<(String, crate::mir::Ty)> {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut out: Vec<(String, crate::mir::Ty)> = Vec::new();
    for v in &mir.state.variants {
        for f in &v.fields {
            if seen.insert(f.name.clone()) {
                out.push((f.name.clone(), f.ty.clone()));
            }
        }
    }
    out
}

/// Emit CPI theorems — Phase 1c-7 port of
/// `lean_gen::render_cpi_theorems`. Two halves:
///
/// 1. **Transfer-envelope theorems** — per `Stmt::TokenTransfer`,
///    emit a `def build_<handler>_transfer<suffix>` CPI constructor
///    over the SPL Token Transfer envelope (program ID, account
///    metas, discriminator) and a sibling `_correct` theorem that
///    closes by `rfl`. Authorityless transfers (no `authority`
///    clause in the `transfers` block) skip the theorem and emit a
///    tracked-obligation comment — the 3-account envelope shape
///    doesn't apply.
///
/// 2. **Call-site ensures-as-axiom theorems** — per `Stmt::Cpi`,
///    look up the callee in `Mir.imports`, then emit one theorem per
///    declared `ensures` clause. Tier-1/2 callees (those with
///    `upstream.binary_hash` non-empty AND non-empty ensures) close
///    via `<Iface>.<method>.ensures_axiom_<idx>`. Tier-0 callees
///    keep the `:= by sorry` shape — the P1 lint
///    `cpi_no_callee_ensures` surfaces them at check time.
///
/// Substitution still flows through
/// `cpi_substitute::substitute_callee_ensures_lean`, which takes a
/// `&ParsedCall`. The emitter constructs a synthetic `ParsedCall`
/// from `Stmt::Cpi`'s data on the fly — Phase 3's `cpi_substitute`
/// MIR→MIR pass will eliminate that bridge.
///
/// Returns the set of pinned interface names referenced by call sites
/// — the caller uses this to decide which sibling `<Iface>.lean`
/// modules to write and which lakefile `require` directives to inject.
fn emit_cpi_theorems(out: &mut String, mir: &Mir) -> std::collections::BTreeSet<String> {
    use crate::mir::Stmt;

    let mut pinned_interfaces: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();

    let state_field_set: std::collections::HashSet<String> = mir
        .state
        .variants
        .first()
        .map(|v| v.fields.iter().map(|f| f.name.clone()).collect())
        .unwrap_or_default();

    for h in &mir.handlers {
        // Skip handlers whose body declares no CPI activity at all.
        let has_any_cpi = h
            .body
            .stmts
            .iter()
            .any(|s| matches!(s, Stmt::TokenTransfer { .. }) || matches!(s, Stmt::Cpi { .. }));
        if !has_any_cpi {
            continue;
        }

        // ---- (1) Transfer-envelope half ----
        let transfers: Vec<&Stmt> = h
            .body
            .stmts
            .iter()
            .filter(|s| matches!(s, Stmt::TokenTransfer { .. }))
            .collect();
        for (i, ts) in transfers.iter().enumerate() {
            let Stmt::TokenTransfer {
                from,
                to,
                amount,
                authority,
            } = *ts
            else {
                continue;
            };
            let suffix = if transfers.len() > 1 {
                format!("_{}", i)
            } else {
                String::new()
            };
            let build_name = safe_name(&format!("build_{}_transfer{}", h.name, suffix));
            let theorem_name = safe_name(&format!("{}_transfer{}_correct", h.name, suffix));

            let from_label = account_ref_label(from);
            let to_label = account_ref_label(to);
            out.push_str(&format!(
                "/-- {} transfer envelope: {} \u{2192} {}",
                h.name, from_label, to_label,
            ));
            if !amount.lean.is_empty() {
                out.push_str(&format!(" amount {}", amount.lean));
            }
            if let Some(auth) = authority {
                out.push_str(&format!(" authority {}", account_ref_label(auth)));
            }
            out.push_str(".\n");
            out.push_str("    Verifies CPI shape (program ID, account list, discriminator).\n");
            out.push_str("    Amount serialization and SPL Token execution are SDK/runtime\n");
            out.push_str("    trust per VERIFICATION_SCOPE.md. -/\n");

            // Authorityless transfers don't fit the 3-account SPL Token
            // envelope. Emit a structured comment instead of a theorem
            // so the obligation is tracked without inventing a proof
            // shape that doesn't match.
            if authority.is_none() {
                out.push_str(&format!(
                    "-- {} transfer{}: no authority declared; envelope theorem skipped.\n\n",
                    h.name, suffix,
                ));
                continue;
            }

            out.push_str(&format!(
                "def {} (from_pk to_pk authority_pk : Pubkey) : CpiInstruction :=\n",
                build_name
            ));
            out.push_str("  { programId := TOKEN_PROGRAM_ID\n");
            out.push_str("  , accounts :=\n");
            out.push_str("      [ \u{27e8}from_pk, false, true\u{27e9}\n");
            out.push_str("      , \u{27e8}to_pk, false, true\u{27e9}\n");
            out.push_str("      , \u{27e8}authority_pk, true, false\u{27e9}\n");
            out.push_str("      ]\n");
            out.push_str("  , data := DISC_TRANSFER }\n\n");

            out.push_str(&format!(
                "theorem {} (from_pk to_pk authority_pk : Pubkey) :\n",
                theorem_name
            ));
            out.push_str(&format!(
                "    let cpi := {} from_pk to_pk authority_pk\n",
                build_name
            ));
            out.push_str("    targetsProgram cpi TOKEN_PROGRAM_ID \u{2227}\n");
            out.push_str("    accountAt cpi 0 from_pk false true \u{2227}\n");
            out.push_str("    accountAt cpi 1 to_pk false true \u{2227}\n");
            out.push_str("    accountAt cpi 2 authority_pk true false \u{2227}\n");
            out.push_str("    hasDiscriminator cpi DISC_TRANSFER := by\n");
            out.push_str(&format!(
                "  unfold {} targetsProgram accountAt hasDiscriminator\n",
                build_name
            ));
            out.push_str("  exact \u{27e8}rfl, rfl, rfl, rfl, rfl\u{27e9}\n\n");
        }

        // ---- (2) Call-site ensures-as-axiom half ----
        let cpi_calls: Vec<&Stmt> = h
            .body
            .stmts
            .iter()
            .filter(|s| matches!(s, Stmt::Cpi { .. }))
            .collect();

        for (call_idx, cs) in cpi_calls.iter().enumerate() {
            let Stmt::Cpi {
                target,
                method,
                args,
                state_binders,
                result_binding,
            } = *cs
            else {
                continue;
            };

            // Resolve the callee through Mir.imports.
            let resolved = mir
                .imports
                .values()
                .filter_map(|imp| {
                    imp.interfaces
                        .get(&target.0)
                        .and_then(|i| i.methods.get(&method.0).map(|m| (imp, i, m)))
                })
                .next();
            let Some((import, _iface_decl, callee)) = resolved else {
                // Unresolved interface — lint surfaces this as
                // `[shape_only_cpi]`. Skip silently here.
                continue;
            };

            let pinned = handler_is_pinned_mir(import, callee);
            if pinned {
                pinned_interfaces.insert(target.0.clone());
            }

            // Synthesize a `ParsedCall` for the substitution helper.
            // The MIR carries the same data — this is a one-line
            // marshalling step that Phase 3's `cpi_substitute` MIR→MIR
            // pass will eliminate.
            let synthetic_call =
                synthesize_parsed_call(target, method, args, state_binders, result_binding);

            // Callee params for the substitute. The type half is
            // ignored inside the helper (only the names matter for the
            // default-substitution loop), so we render an empty string
            // for the type slot.
            let callee_param_names: Vec<(String, String)> = callee
                .params
                .iter()
                .map(|(n, _)| (n.clone(), String::new()))
                .collect();

            let handler_params = param_sig_str(&h.params);

            for (ens_idx, ensures) in callee.ensures.iter().enumerate() {
                let substituted = crate::cpi_substitute::substitute_callee_ensures_lean(
                    &ensures.0.lean,
                    &synthetic_call,
                    &callee_param_names,
                    callee.result_binder.as_deref(),
                );

                // v2.27 Track A — same skip-path logic as legacy.
                let abstract_fields =
                    crate::cpi_substitute::scan_lean_abstract_fields(&ensures.0.lean);
                if !abstract_fields.is_empty() {
                    let missing = crate::cpi_substitute::missing_state_binders(
                        &abstract_fields,
                        &synthetic_call.state_binders,
                    );
                    if !missing.is_empty() {
                        if missing.len() == abstract_fields.len() {
                            out.push_str(&format!(
                                "-- `{}.{}` ensures #{} ({}): caller supplied no \
                                 `state_binders` for these abstract fields; ensures \
                                 not pulled into caller proof. Bind via \
                                 `state_binders {{ {} = state.<field> }}` to consume.\n",
                                target.0,
                                method.0,
                                ens_idx,
                                abstract_fields.join(", "),
                                abstract_fields[0],
                            ));
                        } else {
                            out.push_str(&format!(
                                "-- `{}.{}` ensures #{} ({}): caller supplied incomplete \
                                 `state_binders`; missing {}; ensures not pulled into caller proof. \
                                 Bind via `state_binders {{ {} = state.<field> }}` to consume.\n",
                                target.0,
                                method.0,
                                ens_idx,
                                abstract_fields.join(", "),
                                missing.join(", "),
                                missing[0],
                            ));
                        }
                        continue;
                    }
                }
                let prefixed = if abstract_fields.is_empty() {
                    prefix_state_fields(&substituted, &state_field_set)
                } else {
                    substituted
                };
                let theorem_name = safe_name(&format!(
                    "{}_{}_{}_call_{}_post_{}",
                    h.name, target.0, method.0, call_idx, ens_idx,
                ));

                if pinned {
                    let axiom_qualified = format!(
                        "{}.{}.ensures_axiom_{}",
                        safe_name(&target.0),
                        safe_name(&method.0),
                        ens_idx,
                    );
                    let mut apply_args: Vec<String> = Vec::new();
                    let track_a = !abstract_fields.is_empty();
                    if track_a {
                        apply_args.push("pre".to_string());
                        apply_args.push("post".to_string());
                    }
                    // Sourced from the legacy emitter's `subst` table
                    // — for each callee param, prefer the caller's
                    // substituted argument; fall back to the formal
                    // name otherwise. Parens around compound forms.
                    let subst: std::collections::HashMap<&str, &str> = args
                        .iter()
                        .map(|a| (a.name.as_str(), a.value.lean.as_str()))
                        .collect();
                    for (pn, _) in &callee.params {
                        let raw = subst
                            .get(pn.as_str())
                            .copied()
                            .unwrap_or(pn.as_str())
                            .to_string();
                        let prefixed_arg = prefix_state_fields(&raw, &state_field_set);
                        let needs_parens = prefixed_arg.chars().any(|c| {
                            c.is_whitespace()
                                || c == '+'
                                || c == '-'
                                || c == '*'
                                || c == '/'
                                || c == '<'
                                || c == '>'
                        });
                        if needs_parens {
                            apply_args.push(format!("({})", prefixed_arg));
                        } else {
                            apply_args.push(prefixed_arg);
                        }
                    }
                    if track_a {
                        for field in &abstract_fields {
                            let caller_field = state_binders
                                .iter()
                                .find(|b| &b.callee_field == field)
                                .map(|b| caller_projection_to_field(&b.caller_projection))
                                .unwrap_or_else(|| field.clone());
                            apply_args.push(format!("(\u{00B7}.{})", caller_field));
                        }
                    }
                    let stance = if import.verified_pkg_root.is_some() {
                        "stance 2: discharged via imported callee proof"
                    } else {
                        "stance 1: discharged via Tier-1 binary-hash axiom; \
                         v3.0 will replace the axiom with an imported callee proof"
                    };
                    out.push_str(&format!(
                        "/-- {}.{}.ensures @ `{}` call #{} ({}). -/\n",
                        target.0, method.0, h.name, call_idx, stance,
                    ));
                    if track_a {
                        out.push_str(&format!(
                            "theorem {} (s : State) (pre post : State){} : {} :=\n",
                            theorem_name, handler_params, prefixed,
                        ));
                    } else {
                        out.push_str(&format!(
                            "theorem {} (s : State){} : {} :=\n",
                            theorem_name, handler_params, prefixed,
                        ));
                    }
                    if apply_args.is_empty() {
                        out.push_str(&format!("  {}\n\n", axiom_qualified));
                    } else {
                        out.push_str(&format!(
                            "  {} {}\n\n",
                            axiom_qualified,
                            apply_args.join(" "),
                        ));
                    }
                } else {
                    out.push_str(&format!(
                        "/-- {}.{}.ensures @ `{}` call #{} (stance 1: axiomatized via sorry; \
                         v3.0 will close via imported callee proofs). -/\n",
                        target.0, method.0, h.name, call_idx,
                    ));
                    out.push_str(&format!(
                        "theorem {} (s : State){} : {} := by sorry\n\n",
                        theorem_name, handler_params, prefixed,
                    ));
                }
            }
        }
    }

    pinned_interfaces
}

/// True iff the callee has a non-empty `upstream.binary_hash` pin AND
/// at least one `ensures` clause. Mirrors `lean_gen::handler_is_pinned`.
fn handler_is_pinned_mir(
    import: &crate::mir::ImportedSpecMir,
    callee: &crate::mir::InterfaceMethod,
) -> bool {
    if callee.ensures.is_empty() {
        return false;
    }
    match &import.upstream {
        Some(u) => u
            .binary_hash
            .as_deref()
            .is_some_and(|h| !h.trim().is_empty()),
        None => false,
    }
}

/// Build a label for an `AccountRef` suitable for doc-comment use.
fn account_ref_label(r: &crate::mir::AccountRef) -> String {
    use crate::mir::AccountRef;
    match r {
        AccountRef::ByBinding(s) => s.clone(),
        AccountRef::SelfState => "self".to_string(),
    }
}

/// Extract the caller-side field name from a `StateBinder.caller_projection`.
/// Pilot scope: the path is always a single segment (`state.<ident>`
/// at the surface lowered to `Path::single("<ident>")`). Multi-segment
/// projections are reserved for v3.0 — pick the last segment as the
/// best approximation for the axiom-application slot.
fn caller_projection_to_field(p: &crate::mir::Path) -> String {
    p.segments.last().cloned().unwrap_or_default()
}

/// Synthesize a `ParsedCall` from `Stmt::Cpi` data so the
/// `cpi_substitute` helper (which still consumes parse-layer types)
/// can run unchanged. Phase 3 ports the substitution to MIR and
/// retires this bridge.
fn synthesize_parsed_call(
    target: &crate::mir::InterfaceRef,
    method: &crate::mir::MethodRef,
    args: &[crate::mir::CallArg],
    state_binders: &[crate::mir::StateBinder],
    result_binding: &Option<crate::mir::Symbol>,
) -> crate::check::ParsedCall {
    crate::check::ParsedCall {
        target_interface: target.0.clone(),
        target_handler: method.0.clone(),
        args: args
            .iter()
            .map(|a| crate::check::ParsedCallArg {
                name: a.name.clone(),
                lean_expr: a.value.lean.clone(),
                rust_expr: a.value.rust.clone(),
                rust_expr_pod: a.value.rust_pod.clone(),
            })
            .collect(),
        result_binding: result_binding.clone(),
        state_binders: state_binders
            .iter()
            .map(|b| crate::check::ParsedStateBinder {
                callee_field: b.callee_field.clone(),
                caller_field: caller_projection_to_field(&b.caller_projection),
            })
            .collect(),
    }
}

/// Emit property declarations + preservation theorems. Mirrors
/// `lean_gen::render_properties_inner` for the structural shape:
///
/// ```text
/// def <name> (s : State) : Prop := <body>
///
/// theorem <name>_preserved_by_<handler> (s s' : State) (signer : Pubkey) <params>
///     (h_inv : <name> s) (h : <handler>Transition s signer <args> = some s') :
///     <name> s' := sorry
///
/// /-- <name> is preserved by every operation. Auto-proven by case split. -/
/// theorem <name>_inductive (s s' : State) (signer : Pubkey) (op : Operation)
///     (h_inv : <name> s) (h : applyOp s signer op = some s') :
///     <name> s' := sorry
/// ```
///
/// Phase 1c-5: emits the theorem statements with `sorry` bodies for
/// every preservation sub-lemma. lean_gen.rs's `preservation_proof_script`
/// generates discharged proofs via `if_neg` / `dsimp + omega`
/// projection; that's a follow-up. Properties with no
/// `expression` body emit a structured comment only.
fn emit_properties(out: &mut String, mir: &Mir) {
    if mir.properties.is_empty() {
        return;
    }

    for prop in &mir.properties {
        // Predicate def (when body is present).
        if let Some(expr) = &prop.expression {
            // lean_gen.rs:2716-2737 strips a leading `∀ s : State,`
            // binder since the surrounding def already introduces
            // `(s : State)`. Mirror that — but only when the binder
            // ident is exactly `s`.
            let body = strip_state_forall(&expr.lean);
            out.push_str(&format!(
                "def {} (s : State) : Prop := {}\n\n",
                safe_name(&prop.name),
                body
            ));
        } else {
            out.push_str(&format!(
                "-- PROPERTY OBLIGATION (declared, no predicate body): {}\n\n",
                prop.name
            ));
            continue;
        }

        // Per-handler preservation sub-lemmas.
        let covered: Vec<&crate::mir::HandlerMir> = mir
            .handlers
            .iter()
            .filter(|h| prop.preserved_by.contains(&h.name))
            .collect();
        for h in &covered {
            let trans_name = safe_name(&format!("{}Transition", h.name));
            let param_sig = param_sig_str(&h.params);
            let param_args = param_args_str(&h.params);
            let sub_lemma = safe_name(&format!("{}_preserved_by_{}", prop.name, h.name));
            out.push_str(&format!(
                "theorem {} (s s' : State) (signer : Pubkey){}\n",
                sub_lemma, param_sig
            ));
            out.push_str(&format!(
                "    (h_inv : {} s) (h : {} s signer{} = some s') :\n",
                safe_name(&prop.name),
                trans_name,
                param_args
            ));
            let proof_tail = preservation_proof_script(mir, h, prop);
            out.push_str(&format!("    {} s'{}", safe_name(&prop.name), proof_tail));
        }

        // Master theorem: preserved by every Operation case.
        if !covered.is_empty() {
            out.push_str(&format!(
                "/-- {} is preserved by every operation. Auto-proven by case split. -/\n",
                prop.name
            ));
            out.push_str(&format!(
                "theorem {}_inductive (s s' : State) (signer : Pubkey) (op : Operation)\n",
                safe_name(&prop.name)
            ));
            out.push_str(&format!(
                "    (h_inv : {} s) (h : applyOp s signer op = some s') : {} s'",
                safe_name(&prop.name),
                safe_name(&prop.name)
            ));
            let master_proof = master_inductive_proof_script(mir, prop);
            out.push_str(&master_proof);
        }
    }
}

/// Mechanical proof body for `<prop>_preserved_by_<handler>`. Mirrors
/// `lean_gen::preservation_proof_script`.
///
/// Strategy depends on:
///   1. Quantified-property check (∀/∃ in the property body) — fall back
///      to a `sorry` stub with a structured TODO comment.
///   2. Whether the handler body touches any field the property reads
///      (`touches_prop_field`). When it does, `unfold` the property in
///      both `h_inv` and the goal, then `dsimp; omega`. When it doesn't,
///      `exact h_inv` after equating `s'` with `s`.
///   3. Whether the transition body has an `if … then … else none`
///      guard (`has_cond`). With a guard, `split at h` + `next hg =>`
///      handles the success branch; the `else` branch closes by
///      `contradiction` (since `h` is `some s'`, not `none`). Without
///      a guard, drop straight into `cases h`.
fn preservation_proof_script(
    mir: &Mir,
    h: &crate::mir::HandlerMir,
    prop: &crate::mir::PropertyMir,
) -> String {
    use crate::mir::Stmt;
    let trans_name = safe_name(&format!("{}Transition", h.name));

    let body_lean = prop.expression.as_ref().map(|e| e.lean.clone());
    let has_quantifier = body_lean
        .as_deref()
        .map(|e| e.contains('\u{2200}') || e.contains('\u{2203}'))
        .unwrap_or(false);
    if has_quantifier {
        return format!(
            " := by\n  unfold {} at h\n  sorry -- quantified property: fill with intro + cases or Leanstral\n\n",
            trans_name
        );
    }

    let prop_fields: Vec<String> = body_lean
        .as_deref()
        .map(fields_referenced_in_expr_owned)
        .unwrap_or_default();

    // Handler touches a property field when (a) any effect mutates a
    // field the property reads, or (b) the handler's lifecycle arrow
    // updates `status` and the property mentions `status`.
    let touches_prop_field = h.body.stmts.iter().any(|s| match s {
        Stmt::Assign { path, .. }
        | Stmt::CheckedAdd { path, .. }
        | Stmt::CheckedSub { path, .. }
        | Stmt::WrapAdd { path, .. }
        | Stmt::WrapSub { path, .. }
        | Stmt::SatAdd { path, .. }
        | Stmt::SatSub { path, .. } => prop_fields.iter().any(|f| f == &path_field_name(path)),
        _ => false,
    }) || (h.transition.is_some()
        && prop_fields.iter().any(|f| f == "status"));

    let has_cond = !build_guard_cond_parts(mir, h).is_empty();

    let prop_name = safe_name(&prop.name);

    if has_cond {
        if touches_prop_field {
            format!(
                " := by\n  unfold {} at h; split at h\n  \
                 \u{B7} next hg => cases h; unfold {} at h_inv \u{22A2}; dsimp; omega\n  \
                 \u{B7} contradiction\n\n",
                trans_name, prop_name
            )
        } else {
            format!(
                " := by\n  unfold {} at h; split at h\n  \
                 \u{B7} cases h; exact h_inv\n  \
                 \u{B7} contradiction\n\n",
                trans_name
            )
        }
    } else if touches_prop_field {
        format!(
            " := by\n  unfold {} at h; cases h; \
             unfold {} at h_inv \u{22A2}; dsimp; omega\n\n",
            trans_name, prop_name
        )
    } else {
        format!(
            " := by\n  unfold {} at h; cases h; exact h_inv\n\n",
            trans_name
        )
    }
}

/// Auto-proof body for the master `<prop>_inductive` theorem. Mirrors
/// `lean_gen::render_properties_inner`'s `cases op with` block.
///
/// For each Operation constructor:
///   - When the handler is in `preserved_by`: delegate to the per-
///     handler sub-lemma (`exact <prop>_preserved_by_<op> s s' signer
///     <params> h_inv h`).
///   - When NOT in `preserved_by`: attempt inline auto-proof. The
///     property is trivially preserved if the handler touches no
///     property field; otherwise discharge via `unfold + dsimp; omega`
///     under the transition's split structure.
fn master_inductive_proof_script(mir: &Mir, prop: &crate::mir::PropertyMir) -> String {
    use crate::mir::Stmt;
    let mut proof = String::from(" := by\n  cases op with\n");

    let body_lean = prop.expression.as_ref().map(|e| e.lean.clone());
    let prop_fields: Vec<String> = body_lean
        .as_deref()
        .map(fields_referenced_in_expr_owned)
        .unwrap_or_default();
    let prop_name = safe_name(&prop.name);

    for h in &mir.handlers {
        let ctor = safe_name(&h.name);
        let param_names: Vec<String> = h.params.iter().map(|(n, _)| n.clone()).collect();
        let param_bind = if param_names.is_empty() {
            String::new()
        } else {
            format!(" {}", param_names.join(" "))
        };

        if prop.preserved_by.contains(&h.name) {
            let ref_name = safe_name(&format!("{}_preserved_by_{}", prop.name, h.name));
            proof.push_str(&format!(
                "  | {}{} => exact {} s s' signer{} h_inv h\n",
                ctor, param_bind, ref_name, param_bind
            ));
        } else {
            let trans_name = safe_name(&format!("{}Transition", h.name));
            let touches_prop_field = h.body.stmts.iter().any(|s| match s {
                Stmt::Assign { path, .. }
                | Stmt::CheckedAdd { path, .. }
                | Stmt::CheckedSub { path, .. }
                | Stmt::WrapAdd { path, .. }
                | Stmt::WrapSub { path, .. }
                | Stmt::SatAdd { path, .. }
                | Stmt::SatSub { path, .. } => {
                    prop_fields.iter().any(|f| f == &path_field_name(path))
                }
                _ => false,
            });
            if !touches_prop_field {
                proof.push_str(&format!(
                    "  | {}{} =>\n    simp [applyOp, {}] at h\n    obtain \u{27E8}_, h_eq\u{27E9} := h\n    subst h_eq; exact h_inv\n",
                    ctor, param_bind, trans_name
                ));
            } else {
                let has_cond = !build_guard_cond_parts(mir, h).is_empty();
                if has_cond {
                    proof.push_str(&format!(
                        "  | {}{} =>\n    simp [applyOp] at h\n    unfold {} at h; split at h\n    \u{B7} next hg => cases h; unfold {} at h_inv \u{22A2}; dsimp; omega\n    \u{B7} contradiction\n",
                        ctor, param_bind, trans_name, prop_name
                    ));
                } else {
                    proof.push_str(&format!(
                        "  | {}{} =>\n    simp [applyOp] at h\n    unfold {} at h; cases h; unfold {} at h_inv \u{22A2}; dsimp; omega\n",
                        ctor, param_bind, trans_name, prop_name
                    ));
                }
            }
        }
    }
    proof.push('\n');
    proof
}

/// Owned-string variant of `lean_gen::fields_referenced_in_expr`.
/// Scans `s.<ident>` occurrences and returns each unique field name.
fn fields_referenced_in_expr_owned(expr: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, _) in expr.match_indices("s.") {
        let rest = &expr[i + 2..];
        let end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        if end == 0 {
            continue;
        }
        let field = rest[..end].to_string();
        if !out.contains(&field) {
            out.push(field);
        }
    }
    out
}

/// If `expr` starts with `∀ s : T,` or `forall s : T,`, strip the
/// quantifier prefix and return the body — the surrounding `def
/// <prop> (s : State)` already binds `s`. Other quantified bodies
/// (value binders) pass through unchanged. Mirrors lean_gen.rs:2716.
fn strip_state_forall(expr: &str) -> String {
    let trimmed = expr.trim();
    let rest = trimmed
        .strip_prefix('\u{2200}')
        .or_else(|| trimmed.strip_prefix("forall"));
    if let Some(rest) = rest {
        let rest_trim = rest.trim_start();
        // Only strip if the quantified binder is literally `s`.
        if rest_trim.starts_with("s ") || rest_trim.starts_with("s:") {
            if let Some(comma_pos) = rest.find(',') {
                return rest[comma_pos + 1..].trim().to_string();
            }
        }
    }
    trimmed.to_string()
}

/// Emit invariant theorems. Mirrors `lean_gen::render_invariants_theorem_form`.
///
/// Per invariant with a predicate body:
/// ```text
/// /-- Invariant: <name> — <doc> -/
/// theorem <name> (s : State) : <prefixed-pred> := by sorry
/// ```
///
/// Bare bodies (description-only invariants from pre-v2.14) emit a
/// structured comment instead — no `theorem ... := True` tautology.
/// The `bare_invariant` lint surfaces these as P3.
fn emit_invariants(out: &mut String, mir: &Mir) {
    if mir.invariants.is_empty() {
        return;
    }

    // Collect all state-field names across every variant for the
    // `prefix_state_fields` regex pass.
    let field_set: std::collections::HashSet<String> = mir
        .state
        .variants
        .iter()
        .flat_map(|v| v.fields.iter().map(|f| f.name.clone()))
        .collect();

    for inv in &mir.invariants {
        match &inv.body {
            Some(pred) => {
                let prefixed = prefix_state_fields(&pred.0.lean, &field_set);
                out.push_str(&format!(
                    "/-- Invariant: {}{} -/\n",
                    inv.name,
                    if inv.doc.is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", inv.doc)
                    }
                ));
                out.push_str(&format!(
                    "theorem {} (s : State) : {} := by sorry\n\n",
                    inv.name, prefixed
                ));
            }
            None => {
                out.push_str(&format!(
                    "-- INVARIANT OBLIGATION (declared, no predicate body): {}\n",
                    inv.name
                ));
                if !inv.doc.is_empty() {
                    out.push_str(&format!("--   description: {}\n", inv.doc));
                }
                out.push_str("-- The spec declared this name but didn't supply a predicate body\n");
                out.push_str(
                    "-- (`invariant <name> : <expr>`). The codegen has no goal to lower —\n",
                );
                out.push_str("-- pre-v2.14 emitted `theorem <name> : True := trivial`, which\n");
                out.push_str("-- was tautological. To verify this invariant, give it a body in\n");
                out.push_str("-- the spec.\n\n");
            }
        }
    }
}

/// Emit frame condition theorems. Mirrors
/// `lean_gen::render_frame_conditions`.
///
/// For each handler with a `modifies` clause, emit a theorem proving
/// that every field NOT in `modifies` stays equal across the
/// transition. Lifecycle-transitioning handlers implicitly modify the
/// `status` field.
/// ADT-shape frame condition emitter. Mirrors
/// `lean_gen::render_frame_conditions_adt`: emits per-handler
/// theorems with a `True := by sorry` placeholder body, since the
/// inductive State requires variant-aware case analysis the flat-
/// shape `s'.f = s.f` form can't express. The per-pre-variant
/// reasoning is on the v3.0 roadmap.
fn emit_frame_conditions_adt(out: &mut String, mir: &Mir) {
    let has_modifies = mir.handlers.iter().any(|h| h.modifies.is_some());
    if !has_modifies {
        return;
    }

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Frame conditions (modifies)\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    for h in &mir.handlers {
        if h.modifies.is_none() {
            continue;
        }
        let trans_name = safe_name(&format!("{}Transition", h.name));
        let param_sig = param_sig_str(&h.params);
        let param_args = param_args_str(&h.params);
        let theorem_name = safe_name(&format!("{}_frame", h.name));
        out.push_str(&format!(
            "theorem {} (s s' : State) (signer : Pubkey){}\n",
            theorem_name, param_sig
        ));
        out.push_str(&format!(
            "    (h : {} s signer{} = some s') :\n",
            trans_name, param_args
        ));
        out.push_str("    -- todo!(): inductive-State frame condition. Statement needs\n");
        out.push_str("    -- per-pre-variant case analysis to express which payload\n");
        out.push_str("    -- fields are preserved. Stated as `True` until that lands;\n");
        out.push_str("    -- the honest placeholder proof is `trivial`, not `sorry`.\n");
        out.push_str("    True := trivial\n\n");
    }
}

fn emit_frame_conditions(out: &mut String, mir: &Mir) {
    let has_modifies = mir.handlers.iter().any(|h| h.modifies.is_some());
    if !has_modifies {
        return;
    }

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Frame conditions (modifies)\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    // All declared state-field names across every variant.
    let all_fields: Vec<String> = {
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut out_fields = Vec::new();
        for v in &mir.state.variants {
            for f in &v.fields {
                if seen.insert(f.name.clone()) {
                    out_fields.push(f.name.clone());
                }
            }
        }
        out_fields
    };

    for h in &mir.handlers {
        let Some(modified) = &h.modifies else {
            continue;
        };
        let modified_set: std::collections::HashSet<String> = modified
            .iter()
            .map(|p| p.segments.last().cloned().unwrap_or_default())
            .collect();

        let status_modified = h.transition.is_some();
        let unchanged: Vec<&String> = all_fields
            .iter()
            .filter(|f| !(modified_set.contains(*f) || (*f == "status" && status_modified)))
            .collect();
        if unchanged.is_empty() {
            continue;
        }

        let trans_name = safe_name(&format!("{}Transition", h.name));
        let param_sig = param_sig_str(&h.params);
        let param_args = param_args_str(&h.params);
        let theorem_name = safe_name(&format!("{}_frame", h.name));

        out.push_str(&format!(
            "theorem {} (s s' : State) (signer : Pubkey){}\n",
            theorem_name, param_sig
        ));
        out.push_str(&format!(
            "    (h : {} s signer{} = some s') :\n",
            trans_name, param_args
        ));
        let conjuncts: Vec<String> = unchanged
            .iter()
            .map(|f| format!("s'.{} = s.{}", safe_name(f), safe_name(f)))
            .collect();
        out.push_str(&format!(
            "    {} := sorry\n\n",
            conjuncts.join(" \u{2227} ")
        ));
    }
}

/// Prefix every state-field identifier in `expr` with `s.`. Word-boundary
/// regex avoids touching substrings of other identifiers (e.g., `amount`
/// shouldn't become `s.amount` inside `taker_amount`). Skips fields
/// already prefixed.
fn prefix_state_fields(expr: &str, fields: &std::collections::HashSet<String>) -> String {
    let mut out = expr.to_string();
    for field in fields {
        let pattern = format!(r"\b{}\b", regex::escape(field));
        let re = regex::Regex::new(&pattern).expect("regex compiles for state-field name");
        let replacement = format!("s.{}", field);
        // Skip if already prefixed somewhere — avoid double-prefix on
        // re-passes. The simple way: check `s.<field>` literal presence
        // before applying.
        if out.contains(&replacement) {
            // Already partly prefixed — fall back to a non-greedy
            // single-pass apply that won't double-prefix because the
            // `\b` regex doesn't match after `.` (word boundary
            // already broken by the dot).
            out = re
                .replace_all(&out, regex::NoExpand(&replacement))
                .into_owned();
        } else {
            out = re
                .replace_all(&out, regex::NoExpand(&replacement))
                .into_owned();
        }
    }
    out
}

/// Emit abort theorems. Mirrors `lean_gen::render_aborts_if`.
///
/// For each handler with abort surface (`aborts_if` clauses or
/// `requires X else Err`), emits per-clause theorems:
///
/// ```text
/// theorem <h>_aborts_if_<Err> (s : State) (signer : Pubkey) <params>
///     (h : <pred>) : <h>Transition s signer <args> = none := sorry
/// ```
///
/// For `requires X else Err` the hypothesis is negated form
/// `¬(<requires-expr>)`. When `aborts_total` is set on the handler,
/// emits a single `<h>_aborts_iff` theorem with the disjunction of
/// every abort condition.
///
/// Phase 1c approximation: emits the theorem statements with
/// `:= sorry` bodies for every case. lean_gen.rs has a finer
/// `abort_requires_proof` path that auto-discharges via `if_neg`
/// projection on requires-derived aborts; porting that lands later.
fn emit_aborts_if(out: &mut String, mir: &Mir) {
    emit_aborts_if_with_sorry(out, mir, "sorry");
}

/// Count top-level `∧` conjuncts in a Lean expression. Respects
/// parenthesis nesting (`(a ∧ b) ∧ c` returns 2, not 3). Mirrors
/// `lean_gen::count_top_level_conjuncts`.
fn count_top_level_conjuncts(expr: &str) -> usize {
    let mut depth: i32 = 0;
    let mut count = 0usize;
    for ch in expr.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            '\u{2227}' if depth == 0 => count += 1, // ∧
            _ => {}
        }
    }
    count + 1
}

/// Projection path into a right-associative `∧` chain. Mirrors
/// `lean_gen::conjunction_projection`.
fn conjunction_projection(flat_index: usize, total_atoms: usize) -> String {
    let mut path = String::from("hg");
    for _ in 0..flat_index {
        path.push_str(".2");
    }
    if flat_index < total_atoms - 1 {
        path.push_str(".1");
    }
    path
}

/// Build the tactic body for a flat-path requires-based abort theorem.
/// Mirrors `lean_gen::abort_requires_proof`: the hypothesis
/// `h : ¬(req_lean_expr)` contradicts the matching conjuncts in the
/// transition's guard, so the proof closes by `if_neg` projection.
fn abort_requires_proof(
    trans_name: &str,
    cond_parts: &[String],
    req_index_in_cond_parts: usize,
) -> String {
    let atoms_per: Vec<usize> = cond_parts
        .iter()
        .map(|p| count_top_level_conjuncts(p))
        .collect();
    let total_atoms: usize = atoms_per.iter().sum();
    let flat_start: usize = atoms_per[..req_index_in_cond_parts].iter().sum();
    let target_atoms = atoms_per[req_index_in_cond_parts];

    if total_atoms == 1 {
        return format!(" := by\n  unfold {}\n  rw [if_neg h]\n", trans_name);
    }

    let projections: Vec<String> = (0..target_atoms)
        .map(|i| conjunction_projection(flat_start + i, total_atoms))
        .collect();
    let extraction = if projections.len() == 1 {
        projections[0].clone()
    } else {
        format!("\u{27E8}{}\u{27E9}", projections.join(", "))
    };

    format!(
        " := by\n  unfold {}\n  rw [if_neg (fun hg => h {})]\n",
        trans_name, extraction
    )
}

/// ADT-shape variant — emits `:= by sorry` for every clause, matching
/// `lean_gen::render_aborts_if_adt`. The structural difference is
/// confined to the proof body's tactic / term form; the theorem
/// statements are identical.
fn emit_aborts_if_adt(out: &mut String, mir: &Mir) {
    emit_aborts_if_with_sorry(out, mir, "by sorry");
}

fn emit_aborts_if_with_sorry(out: &mut String, mir: &Mir, sorry_form: &str) {
    let has_aborts = mir
        .handlers
        .iter()
        .any(|h| !h.aborts_if.is_empty() || !h.requires_or_abort.is_empty());
    if !has_aborts {
        return;
    }

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Abort conditions — operations must reject under specified conditions\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    for h in &mir.handlers {
        if h.aborts_if.is_empty() && h.requires_or_abort.is_empty() {
            continue;
        }
        let trans_name = safe_name(&format!("{}Transition", h.name));
        let param_sig = param_sig_str(&h.params);
        let param_args = param_args_str(&h.params);

        // `aborts_total` collapses all abort conditions into a single
        // iff theorem. Mirror lean_gen.rs:4396.
        let all_abort_lean: Vec<String> = h
            .aborts_if
            .iter()
            .map(|a| a.pred.0.lean.clone())
            .chain(
                h.requires_or_abort
                    .iter()
                    .map(|r| format!("\u{00AC}({})", r.pred.0.lean)),
            )
            .collect();

        if h.aborts_total && !all_abort_lean.is_empty() {
            let theorem_name = safe_name(&format!("{}_aborts_iff", h.name));
            out.push_str(&format!(
                "theorem {} (s : State) (signer : Pubkey){} :\n",
                theorem_name, param_sig
            ));
            out.push_str(&format!(
                "    {} s signer{} = none \u{2194}\n",
                trans_name, param_args
            ));
            let disjunction = all_abort_lean.join(" \u{2228} ");
            out.push_str(&format!("    ({}) := {}\n\n", disjunction, sorry_form));
            continue;
        }

        // Per-clause theorems. Disambiguate when the same error name
        // appears multiple times on a single handler (issue #8 #3).
        let mut error_total: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for a in &h.aborts_if {
            *error_total.entry(a.err.clone()).or_insert(0) += 1;
        }
        for r in &h.requires_or_abort {
            *error_total.entry(r.err.clone()).or_insert(0) += 1;
        }
        let mut error_seen: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let theorem_name_for =
            |err: &str, seen: &mut std::collections::HashMap<String, usize>| -> String {
                let total = error_total.get(err).copied().unwrap_or(0);
                let idx = {
                    let entry = seen.entry(err.to_string()).or_insert(0);
                    let cur = *entry;
                    *entry += 1;
                    cur
                };
                if total > 1 {
                    safe_name(&format!("{}_aborts_if_{}_{}", h.name, err, idx))
                } else {
                    safe_name(&format!("{}_aborts_if_{}", h.name, err))
                }
            };

        // Legacy aborts_if clauses: hypothesis IS the predicate.
        for a in &h.aborts_if {
            let theorem_name = theorem_name_for(&a.err, &mut error_seen);
            out.push_str(&format!(
                "theorem {} (s : State) (signer : Pubkey){}\n",
                theorem_name, param_sig
            ));
            out.push_str(&format!(
                "    (h : {}) : {} s signer{} = none := {}\n\n",
                a.pred.0.lean, trans_name, param_args, sorry_form
            ));
        }

        // requires-else clauses: hypothesis is ¬(predicate). Skip
        // clauses that reference a handler account's `.pubkey` /
        // `.key()` — those identifiers aren't in Lean scope, so the
        // theorem would mention free variables. Mirrors
        // lean_gen.rs:4467. Skipping here keeps the Lean compilable;
        // the runtime-side check still fires in Rust.
        //
        // For the flat path (`sorry_form == "sorry"`), the proof
        // mechanically closes via `if_neg`-with-projection — mirror
        // `lean_gen::abort_requires_proof`. The ADT path
        // (`"by sorry"`) keeps the sorry placeholder; per-variant
        // pattern matching makes the same script ill-formed and the
        // legacy ADT renderer is itself sorry-bodied today.
        let cond_parts = build_guard_cond_parts(mir, h);
        let flat_path = sorry_form == "sorry";
        for r in &h.requires_or_abort {
            if mentions_handler_account_pubkey(&r.pred.0.lean, &h.accounts) {
                continue;
            }
            let theorem_name = theorem_name_for(&r.err, &mut error_seen);
            out.push_str(&format!(
                "theorem {} (s : State) (signer : Pubkey){}\n",
                theorem_name, param_sig
            ));
            let req_pos = cond_parts.iter().position(|c| c == &r.pred.0.lean);
            if flat_path {
                if let Some(pos) = req_pos {
                    let proof = abort_requires_proof(&trans_name, &cond_parts, pos);
                    out.push_str(&format!(
                        "    (h : \u{00AC}({})) : {} s signer{} = none{}\n",
                        r.pred.0.lean, trans_name, param_args, proof
                    ));
                    continue;
                }
            } else if req_pos.is_some() {
                // ADT path: the transition matches on the `State`
                // variant *before* the guard `if`, so the flat
                // `rw [if_neg]` script is ill-formed. `cases s <;>
                // simp_all` discharges every variant uniformly: the
                // active variant reduces via `h` (¬guard ⇒ else-branch
                // ⇒ `none`), the other variants hit the catch-all
                // `_ => none`. Gated on the predicate being an actual
                // guard conjunct (`req_pos.is_some()`) — otherwise `h`
                // wouldn't contradict the guard and the goal isn't
                // provable, so we fall through to the `sorry`
                // placeholder and keep the obligation honest.
                let proof = format!(" := by\n  unfold {}\n  cases s <;> simp_all\n", trans_name);
                out.push_str(&format!(
                    "    (h : \u{00AC}({})) : {} s signer{} = none{}\n",
                    r.pred.0.lean, trans_name, param_args, proof
                ));
                continue;
            }
            out.push_str(&format!(
                "    (h : \u{00AC}({})) : {} s signer{} = none := {}\n\n",
                r.pred.0.lean, trans_name, param_args, sorry_form
            ));
        }
    }
}

/// Emit ensures theorems. Mirrors `lean_gen::render_ensures`.
///
/// Per handler, one theorem per `ensures` clause:
///
/// ```text
/// theorem <h>_ensures_<i> (s s' : State) (signer : Pubkey) <params>
///     (h : <h>Transition s signer <args> = some s') :
///     <ensures-lean-expr> := sorry
/// ```
fn emit_ensures(out: &mut String, mir: &Mir) {
    let has_ensures = mir.handlers.iter().any(|h| !h.post.is_empty());
    if !has_ensures {
        return;
    }

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Post-conditions (ensures)\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    for h in &mir.handlers {
        let trans_name = safe_name(&format!("{}Transition", h.name));
        let param_sig = param_sig_str(&h.params);
        let param_args = param_args_str(&h.params);
        for (i, ens) in h.post.iter().enumerate() {
            let theorem_name = safe_name(&format!("{}_ensures_{}", h.name, i));
            out.push_str(&format!(
                "theorem {} (s s' : State) (signer : Pubkey){}\n",
                theorem_name, param_sig
            ));
            out.push_str(&format!(
                "    (h : {} s signer{} = some s') :\n",
                trans_name, param_args
            ));
            out.push_str(&format!("    {} := sorry\n\n", ens.0.lean));
        }
    }
}

/// Detect whether an expression references a handler-account's
/// `.pubkey` or `.key()`. Account-binding pubkey refs aren't in Lean
/// scope; emitting theorems that mention them yields unprovable
/// statements with free identifiers. Mirrors
/// `lean_gen::mentions_handler_account_pubkey`.
fn mentions_handler_account_pubkey(
    expr: &str,
    accounts: &[crate::mir::AccountBindingShape],
) -> bool {
    accounts.iter().any(|a| {
        let needle_pubkey = format!("{}.pubkey", a.name);
        let needle_key = format!("{}.key()", a.name);
        expr.contains(&needle_pubkey) || expr.contains(&needle_key)
    })
}

/// Build the call-side argument string for transition function
/// invocations: `" p1 p2 ..."`. Empty when `params` is empty.
fn param_args_str(params: &[(crate::mir::Symbol, crate::mir::Ty)]) -> String {
    if params.is_empty() {
        return String::new();
    }
    format!(
        " {}",
        params
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    )
}

/// Emit the `inductive Operation` enum + `def applyOp` dispatcher.
/// Mirrors `lean_gen::render_operation_inductive`.
fn emit_operation_inductive(out: &mut String, mir: &Mir) {
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

    // applyOp dispatcher.
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

fn emit_state_struct(out: &mut String, mir: &Mir) {
    // For multi-variant ADTs (e.g., State | Uninitialized | Open of
    // {...} | Closed), the flat-state form unions every variant's
    // fields into one struct, keyed by name. The status field carries
    // the lifecycle discriminator. Mirrors lean_gen.rs's flat-state
    // shape — variants don't get separate constructors here; that's
    // the `render_single_account_adt` (multi-variant ADT) path landing
    // later in Phase 1c.
    if mir.state.variants.is_empty() {
        return;
    }

    let has_lifecycle = mir.state.lifecycle_states.len() >= 2;

    // Union fields across all variants, preserving declaration order
    // and de-duping by name.
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut unique_fields: Vec<&crate::mir::FieldDecl> = Vec::new();
    for v in &mir.state.variants {
        for f in &v.fields {
            if seen.insert(f.name.clone()) {
                unique_fields.push(f);
            }
        }
    }

    out.push_str("structure State where\n");
    for field in &unique_fields {
        out.push_str(&format!(
            "  {} : {}\n",
            safe_name(&field.name),
            render_ty(&field.ty)
        ));
    }
    if has_lifecycle {
        out.push_str("  status : Status\n");
    }
    // Issue #67 item 3 — ghost (spec-only) fields, appended LAST so the
    // non-ghost field prefix keeps a stable order for any positional
    // `⟨…⟩` State construction (e.g. cover witnesses).
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
// Cover-witness machinery — port of `lean_gen::WitnessState` + helpers
//
// Builds concrete state witnesses for cover-trace proofs by symbolically
// evaluating each handler in a trace. Used by `emit_covers` to replace
// `:= sorry` with a real `exact ⟨…, by decide, …⟩` discharge when every
// step of the trace is symbolically computable.
// ----------------------------------------------------------------------

/// Concrete state used for cover-trace proof synthesis. Field values
/// are strings rather than typed Lean terms — Pubkey fields hold `"pk"`
/// (the binding the proof scope introduces), Bool fields hold
/// `"false"`, numeric fields hold a numeric string.
struct WitnessState {
    fields: Vec<(String, String)>,
    status: Option<String>,
}

impl WitnessState {
    fn new(state: &crate::mir::StateAdt) -> Self {
        // For multi-variant ADT specs, the union of all variant fields
        // forms the witness's flat-field view. The mirrors of
        // `lean_gen.rs::WitnessState::new` plus `spec.state_fields`'s
        // de-duplicated union behavior — first variant defines the order
        // and any new fields from later variants append.
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut fields: Vec<(String, String)> = Vec::new();
        for v in &state.variants {
            for f in &v.fields {
                if seen.insert(f.name.clone()) {
                    let val = match &f.ty {
                        crate::mir::Ty::Pubkey => "pk".to_string(),
                        crate::mir::Ty::Bool => "false".to_string(),
                        _ => "0".to_string(),
                    };
                    fields.push((f.name.clone(), val));
                }
            }
        }
        WitnessState {
            fields,
            status: state.lifecycle_states.first().cloned(),
        }
    }

    /// Render as a positional struct literal `⟨pk, pk, 0, …, .Status⟩`
    /// — flat-state shape. Multi-variant ADT specs route through
    /// `witness_state_to_adt` instead.
    fn to_lean(&self) -> String {
        let mut parts: Vec<String> = self.fields.iter().map(|(_, v)| v.clone()).collect();
        if let Some(ref s) = self.status {
            parts.push(format!(".{}", s));
        }
        format!("\u{27E8}{}\u{27E9}", parts.join(", "))
    }

    /// Walk `handler.body.stmts` and update field values + lifecycle
    /// status. Mirrors `lean_gen::WitnessState::apply` against MIR's
    /// `Stmt` shape. Saturating arithmetic for sub so witnesses don't
    /// underflow when the proof has unsatisfiable conditions.
    fn apply(
        &mut self,
        h: &crate::mir::HandlerMir,
        params: &[(String, String)],
        constants: &[(crate::mir::Symbol, String)],
        mir: &Mir,
    ) {
        use crate::mir::Stmt;
        for stmt in &h.body.stmts {
            match stmt {
                Stmt::Assign { path, rhs } => {
                    if is_account_pubkey_ref(&rhs.rust) {
                        continue;
                    }
                    let key = strip_variant_prefix(path, mir);
                    let resolved = self.resolve_value(&rhs.rust, params, constants);
                    if let Some(f) = self.fields.iter_mut().find(|(n, _)| n == &key) {
                        f.1 = resolved;
                    }
                }
                Stmt::CheckedAdd { path, delta, .. }
                | Stmt::WrapAdd { path, delta }
                | Stmt::SatAdd { path, delta } => {
                    let key = strip_variant_prefix(path, mir);
                    let resolved = self.resolve_value(&delta.rust, params, constants);
                    if let Some(f) = self.fields.iter_mut().find(|(n, _)| n == &key) {
                        let cur: u128 = f.1.parse().unwrap_or(0);
                        let add: u128 = resolved.parse().unwrap_or(0);
                        f.1 = cur.saturating_add(add).to_string();
                    }
                }
                Stmt::CheckedSub { path, delta, .. }
                | Stmt::WrapSub { path, delta }
                | Stmt::SatSub { path, delta } => {
                    let key = strip_variant_prefix(path, mir);
                    let resolved = self.resolve_value(&delta.rust, params, constants);
                    if let Some(f) = self.fields.iter_mut().find(|(n, _)| n == &key) {
                        let cur: u128 = f.1.parse().unwrap_or(0);
                        let sub: u128 = resolved.parse().unwrap_or(0);
                        f.1 = cur.saturating_sub(sub).to_string();
                    }
                }
                _ => {}
            }
        }
        if let Some((_, post)) = &h.transition {
            self.status = Some(post.clone());
        }
    }

    /// Resolve a value reference: caller-bound parameter → numeric
    /// literal → spec-constant lookup → self-field lookup → fallback.
    fn resolve_value(
        &self,
        value: &str,
        params: &[(String, String)],
        constants: &[(crate::mir::Symbol, String)],
    ) -> String {
        let v = value.trim();
        if let Some((_, x)) = params.iter().find(|(n, _)| n == v) {
            return x.clone();
        }
        if v.parse::<u128>().is_ok() {
            return v.to_string();
        }
        if let Some(f) = self.fields.iter().find(|(n, _)| n == v) {
            return f.1.clone();
        }
        if let Some((_, x)) = constants.iter().find(|(n, _)| n == v) {
            return x.clone();
        }
        "1".to_string()
    }
}

/// Multi-variant ADT counterpart of `WitnessState::to_lean` — emits a
/// `(.Variant arg0 arg1 … : State)` constructor term using the current
/// witness status to pick the variant.
fn witness_state_to_adt(
    ws: &WitnessState,
    variants: &[crate::mir::StateVariant],
) -> Option<String> {
    let status = ws.status.as_deref()?;
    let variant = variants.iter().find(|v| v.tag == status)?;
    if variant.fields.is_empty() {
        return Some(format!("(.{} : State)", variant.tag));
    }
    let args: Vec<String> = variant
        .fields
        .iter()
        .map(|f| {
            ws.fields
                .iter()
                .find(|(n, _)| n == &f.name)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| "0".to_string())
        })
        .collect();
    Some(format!("(.{} {} : State)", variant.tag, args.join(" ")))
}

/// Choose concrete witness values for a handler's parameters.
/// Mirrors `lean_gen::choose_param_values`:
///   * Pubkey  → `pk`
///   * Bool    → `false`
///   * Index-like numeric params (`param < s.X` without bound) → `0`
///   * Otherwise → `1` (satisfies common `> 0` / `≤ N` guards)
fn choose_param_values(h: &crate::mir::HandlerMir) -> Vec<(String, String)> {
    let mut all_exprs: Vec<&str> = Vec::new();
    for p in &h.pre {
        all_exprs.push(&p.0.lean);
    }
    for r in &h.requires_or_abort {
        all_exprs.push(&r.pred.0.lean);
    }
    let combined = all_exprs.join(" ");
    h.params
        .iter()
        .map(|(name, ty)| {
            let val = match ty {
                crate::mir::Ty::Pubkey => "pk".to_string(),
                crate::mir::Ty::Bool => "false".to_string(),
                _ => {
                    let is_index_like = combined.contains(&format!("{} < s.", name))
                        && !combined.contains(&format!("{} > 0", name))
                        && !combined.contains(&format!("{} \u{2265}", name))
                        && !combined.contains(&format!("\u{2264} {}", name));
                    if is_index_like {
                        "0".to_string()
                    } else {
                        "1".to_string()
                    }
                }
            };
            (name.clone(), val)
        })
        .collect()
}

/// Build the auto-proof script for a cover trace theorem. Symbolically
/// evaluates each handler in `trace` against a witness state, then
/// emits `let s0, s1, …` declarations and an `exact ⟨…, by decide,
/// …⟩` term. Returns `None` if any handler in the trace doesn't
/// resolve — the caller falls back to `:= sorry`.
///
/// `adt_form` switches the witness rendering between the flat-state
/// struct literal and the ADT variant-constructor form. Mirrors
/// `lean_gen::cover_trace_proof` (flat) and `cover_trace_proof_adt`
/// (ADT).
fn cover_trace_proof(mir: &Mir, trace: &[crate::mir::Symbol], adt_form: bool) -> Option<String> {
    if trace.is_empty() {
        return None;
    }

    let mut state = WitnessState::new(&mir.state);
    type CoverStep = (String, Vec<(String, String)>, WitnessState);
    let mut steps: Vec<CoverStep> = Vec::new();

    for op_name in trace {
        let handler = mir.handlers.iter().find(|h| &h.name == op_name)?;
        let param_values = choose_param_values(handler);
        let state_before = WitnessState {
            fields: state.fields.clone(),
            status: state.status.clone(),
        };
        state.apply(handler, &param_values, &mir.constants, mir);
        steps.push((op_name.clone(), param_values, state_before));
    }

    let render_witness = |ws: &WitnessState| -> Option<String> {
        if adt_form {
            witness_state_to_adt(ws, &mir.state.variants)
        } else {
            Some(ws.to_lean())
        }
    };

    let mut proof = String::new();
    proof.push_str(" := by\n");
    proof.push_str("  let pk : Pubkey := \u{27E8}0, 0, 0, 0\u{27E9}\n");

    if let Some((_, _, ref s0)) = steps.first() {
        let s0_lean = render_witness(s0)?;
        proof.push_str(&format!("  let s0 : State := {}\n", s0_lean));
    }

    for (i, _) in steps.iter().enumerate() {
        if i < steps.len() - 1 {
            let mut s = WitnessState::new(&mir.state);
            for step in steps.iter().take(i + 1) {
                let h = mir.handlers.iter().find(|x| x.name == step.0)?;
                s.apply(h, &step.1, &mir.constants, mir);
            }
            let s_lean = render_witness(&s)?;
            proof.push_str(&format!("  let s{} : State := {}\n", i + 1, s_lean));
        }
    }

    let mut exact_parts: Vec<String> = Vec::new();
    exact_parts.push("s0".to_string());
    exact_parts.push("pk".to_string());
    for (i, (_, param_values, _)) in steps.iter().enumerate() {
        for (_, val) in param_values {
            exact_parts.push(val.clone());
        }
        if i < steps.len() - 1 {
            exact_parts.push(format!("s{}", i + 1));
            exact_parts.push("by decide".to_string());
        } else {
            exact_parts.push("by decide".to_string());
        }
    }
    proof.push_str(&format!(
        "  exact \u{27E8}{}\u{27E9}\n",
        exact_parts.join(", ")
    ));
    Some(proof)
}

/// Emit cover theorems — reachability obligations over a sequence of
/// handler invocations. Each `cover <name> [op_1, ..., op_n]` lowers
/// to a nested existential asserting the trace runs to completion;
/// each `reachable when <expr>` entry lowers to one theorem per
/// `(op, when)` pair.
///
/// Trace theorems try `cover_trace_proof` first (witness construction
/// with `by decide` on each step); they fall back to `:= sorry` when
/// the witness machinery can't synthesize a discharge. `reachable
/// when` entries always emit `:= sorry` — no witness chain is
/// available.
fn emit_covers(out: &mut String, mir: &Mir) {
    emit_covers_inner(out, mir, false);
}

/// ADT-shape cover emitter — same trace structure but witness terms
/// are rendered as variant constructors via `witness_state_to_adt`.
fn emit_covers_adt(out: &mut String, mir: &Mir) {
    emit_covers_inner(out, mir, true);
}

fn emit_covers_inner(out: &mut String, mir: &Mir, adt_form: bool) {
    if mir.covers.is_empty() {
        return;
    }
    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Cover properties \u{2014} reachability (existential proofs)\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    for cover in &mir.covers {
        for (trace_idx, trace) in cover.traces.iter().enumerate() {
            let suffix = if cover.traces.len() > 1 {
                format!("_{}", trace_idx)
            } else {
                String::new()
            };

            out.push_str(&format!(
                "/-- {} \u{2014} trace [{}] is reachable. -/\n",
                cover.name,
                trace.join(", ")
            ));
            out.push_str(&format!(
                "theorem cover_{}{} : \u{2203} (s0 : State) (signer : Pubkey),\n",
                cover.name, suffix
            ));

            // Build the nested `∃ s_{j+1}, <trans> s_j signer args =
            // some s_{j+1} ∧ ...` chain. The terminal step uses
            // `≠ none` to keep with the legacy emission shape.
            let mut indent = "    ".to_string();
            for (j, op_name) in trace.iter().enumerate() {
                let handler = mir.handlers.iter().find(|h| h.name == *op_name);
                let trans = safe_name(&format!("{}Transition", op_name));
                let param_args = handler
                    .map(|h| param_args_str(&h.params))
                    .unwrap_or_default();
                let extra_exists = handler
                    .map(|h| {
                        h.params
                            .iter()
                            .enumerate()
                            .map(|(k, (_, t))| format!("(v{}_{} : {})", j, k, render_ty(t)))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();

                // Rename param refs in the call site to the existentially-
                // bound `v{j}_{k}` form. We keep `param_args` as the
                // declared names because the legacy emits raw names too,
                // and the binders match by position once `extra_exists`
                // shadows them in the inner scope. For the MIR pilot we
                // simply reuse the declared names without renaming —
                // mirrors `render_covers` line ~3699.
                let _ = param_args;
                let positional_args = handler
                    .map(|h| {
                        h.params
                            .iter()
                            .enumerate()
                            .map(|(k, _)| format!("v{}_{}", j, k))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();

                let s_var = if j == 0 {
                    "s0".to_string()
                } else {
                    format!("s{}", j)
                };

                if !extra_exists.is_empty() {
                    out.push_str(&format!("{}\u{2203} {}, ", indent, extra_exists));
                }

                if j < trace.len() - 1 {
                    let s_next = format!("s{}", j + 1);
                    let arg_str = if positional_args.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", positional_args)
                    };
                    out.push_str(&format!(
                        "\u{2203} ({} : State), {} {} signer{} = some {} \u{2227}\n",
                        s_next, trans, s_var, arg_str, s_next
                    ));
                    indent.push_str("  ");
                } else {
                    let arg_str = if positional_args.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", positional_args)
                    };
                    // Try witness construction; fall back to `:= sorry`
                    // when the witness machinery can't synthesize a
                    // closed term (handler not found, unsupported
                    // effect shape, etc.).
                    let proof_script = cover_trace_proof(mir, trace, adt_form);
                    match proof_script {
                        Some(script) => {
                            out.push_str(&format!(
                                "{} {} signer{} \u{2260} none{}\n",
                                trans, s_var, arg_str, script
                            ));
                        }
                        None => {
                            out.push_str(&format!(
                                "{} {} signer{} \u{2260} none := sorry\n\n",
                                trans, s_var, arg_str
                            ));
                        }
                    }
                }
            }
        }

        for (op_name, when_pred) in &cover.reachable {
            let handler = mir.handlers.iter().find(|h| h.name == *op_name);
            let trans = safe_name(&format!("{}Transition", op_name));
            let param_exists = handler
                .map(|h| {
                    h.params
                        .iter()
                        .map(|(n, t)| format!("({} : {})", n, render_ty(t)))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let param_args = handler
                .map(|h| param_args_str(&h.params))
                .unwrap_or_default();

            out.push_str(&format!(
                "/-- {} \u{2014} {} is reachable",
                cover.name, op_name
            ));
            if let Some(p) = when_pred {
                out.push_str(&format!(" when {}. -/\n", p.0.lean));
            } else {
                out.push_str(". -/\n");
            }
            out.push_str(&format!(
                "theorem cover_{}_{} : \u{2203} (s : State) (signer : Pubkey),\n",
                cover.name,
                safe_name(op_name)
            ));
            if let Some(p) = when_pred {
                out.push_str(&format!("    {} \u{2227} ", p.0.lean));
            } else {
                out.push_str("    ");
            }
            if !param_exists.is_empty() {
                out.push_str(&format!("\u{2203} {}, ", param_exists));
            }
            out.push_str(&format!(
                "{} s signer{} \u{2260} none := sorry\n\n",
                trans, param_args
            ));
        }
    }
}

/// Emit `liveness` (bounded leads-to) theorems. Mirrors
/// `lean_gen::render_liveness` in shape but always emits the
/// existential `∃ ops s', ... := by sorry` form. The legacy emits
/// a stronger universal-form theorem when the lifecycle-graph walk
/// auto-discovers a path; porting `find_liveness_path` +
/// `liveness_proof_script` is a separately-tracked deferred item.
fn emit_liveness(out: &mut String, mir: &Mir) {
    emit_liveness_inner(out, mir, false);
}

/// ADT-shape liveness emitter. Mirrors `lean_gen::render_liveness_adt`
/// — the theorem statement uses `∃ ops, ... ∧ ∀ s', applyOps … = some
/// s' → s'.status = .Target` (existence of a bounded-length sequence
/// such that any successful evaluation reaches the target), whereas
/// the flat-state form uses `∃ ops s', ... ∧ applyOps … = some s' ∧
/// s'.status = ...` (existential over both the ops sequence and the
/// resulting state). Both are valid liveness statements; the legacy
/// split is preserved so MIR + legacy output stays byte-identical.
fn emit_liveness_adt(out: &mut String, mir: &Mir) {
    emit_liveness_inner(out, mir, true);
}

fn emit_liveness_inner(out: &mut String, mir: &Mir, adt_form: bool) {
    if mir.liveness_props.is_empty() {
        return;
    }
    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Liveness properties \u{2014} bounded reachability (leads-to)\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    // Emit one shared `applyOps` helper (matches the legacy single-
    // account / single-operation-type pilot scope). Indexed and
    // multi-account variants are deferred.
    let needs_helper = mir.handlers.iter().any(|_| true);
    if needs_helper {
        out.push_str(
            "def applyOps (s : State) (signer : Pubkey) : List Operation \u{2192} Option State\n",
        );
        out.push_str("  | [] => some s\n");
        out.push_str("  | op :: ops => match applyOp s signer op with\n");
        out.push_str("    | some s' => applyOps s' signer ops\n");
        out.push_str("    | none => none\n\n");
    }

    for liveness in &mir.liveness_props {
        let bound = liveness.within_steps.unwrap_or(10);
        out.push_str(&format!(
            "/-- {} \u{2014} from {} leads to {} within {} steps via [{}]. -/\n",
            liveness.name,
            liveness.from_state,
            liveness.leads_to_state,
            bound,
            liveness.via_ops.join(", ")
        ));
        out.push_str(&format!(
            "theorem liveness_{} (s : State) (signer : Pubkey)\n",
            liveness.name
        ));
        out.push_str(&format!(
            "    (h : s.status = .{}) :\n",
            liveness.from_state
        ));
        if adt_form {
            // ADT-shape liveness: keep the universal-implication form
            // with `by sorry`; the auto-discharge script pattern-matches
            // on flat-state if-guards and isn't valid against the
            // per-variant pattern-match transitions.
            out.push_str(&format!(
                "    \u{2203} ops, ops.length \u{2264} {} \u{2227} \u{2200} s', applyOps s signer ops = some s' \u{2192} s'.status = .{} := by sorry\n\n",
                bound, liveness.leads_to_state
            ));
            continue;
        }

        // Flat-state path: try to find a concrete via-op path through
        // the lifecycle. When one exists, emit the universal-
        // implication form + auto-proof script (legacy
        // `render_liveness` ~line 3994). Otherwise fall back to the
        // existential form with sorry (non-vacuous obligation, issue
        // #38).
        let path = find_liveness_path(
            &liveness.from_state,
            &liveness.leads_to_state,
            &liveness.via_ops,
            &mir.handlers,
        );

        if let Some(ref ops_path) = path {
            let proof = liveness_proof_script(ops_path, &mir.handlers);
            out.push_str(&format!(
                "    \u{2203} ops, ops.length \u{2264} {} \u{2227} \u{2200} s', applyOps s signer ops = some s' \u{2192} s'.status = .{}{}\n",
                bound, liveness.leads_to_state, proof
            ));
        } else {
            out.push_str(&format!(
                "    \u{2203} ops s', ops.length \u{2264} {} \u{2227} applyOps s signer ops = some s' \u{2227} s'.status = .{} := by sorry\n\n",
                bound, liveness.leads_to_state
            ));
        }
    }
}

/// BFS through the lifecycle graph defined by `via_ops`'
/// `(pre_status, post_status)` arrows, returning the first sequence
/// that gets from `from_state` to `to_state`. Mirrors
/// `lean_gen::find_liveness_path` byte-for-byte (single-step shortcut
/// + bounded BFS by `via_ops.len()`).
fn find_liveness_path(
    from_state: &str,
    to_state: &str,
    via_ops: &[String],
    handlers: &[crate::mir::HandlerMir],
) -> Option<Vec<String>> {
    for op_name in via_ops {
        if let Some(h) = handlers.iter().find(|h| h.name == *op_name) {
            if let Some((pre, post)) = &h.transition {
                if pre == from_state && post == to_state {
                    return Some(vec![op_name.clone()]);
                }
            }
        }
    }

    let mut queue: Vec<(String, Vec<String>)> = vec![(from_state.to_string(), Vec::new())];
    let max_depth = via_ops.len();

    while let Some((current, path)) = queue.first().cloned() {
        queue.remove(0);
        if path.len() >= max_depth {
            continue;
        }
        for op_name in via_ops {
            if let Some(h) = handlers.iter().find(|h| h.name == *op_name) {
                if let Some((pre, post)) = &h.transition {
                    if pre == &current && !post.is_empty() {
                        let mut new_path = path.clone();
                        new_path.push(op_name.clone());
                        if post == to_state {
                            return Some(new_path);
                        }
                        queue.push((post.clone(), new_path));
                    }
                }
            }
        }
    }
    None
}

/// Generate the Lean tactic body for a liveness theorem along an
/// already-found `ops_path`. Mirrors `lean_gen::liveness_proof_script`;
/// shape:
///   1. Optional `let pk : Pubkey := ⟨0,0,0,0⟩` when any constructor
///      takes a Pubkey witness.
///   2. `refine ⟨[<ops>], by decide, fun s' h_apply => ?_⟩`
///   3. `simp only [applyOps, applyOp, …]` then `split at h_apply` /
///      `subst` / `rfl` mechanics (one nest per step, single-step is
///      special-cased).
///
/// `needs_split[i]` is true when handler i's if-guard is non-trivial
/// (has a `who`, a requires clause, or a lifecycle gate). MIR's
/// proxy: `build_guard_cond_parts` produces a non-empty conjunct list.
fn liveness_proof_script(ops_path: &[String], handlers: &[crate::mir::HandlerMir]) -> String {
    let n = ops_path.len();

    // Build the ops list literal: `[.op1 arg1, .op2, ...]`. Each
    // constructor needs a witness arg per `params`; bare `.op` would
    // mistype handlers whose Operation constructor takes parameters.
    let mut needs_pk_binding = false;
    let ops_list: Vec<String> = ops_path
        .iter()
        .map(|name| {
            let handler = handlers.iter().find(|h| &h.name == name);
            let args: Vec<String> = match handler {
                Some(h) => h
                    .params
                    .iter()
                    .map(|(_, ty)| match ty {
                        crate::mir::Ty::Pubkey => {
                            needs_pk_binding = true;
                            "pk".to_string()
                        }
                        crate::mir::Ty::Bool => "false".to_string(),
                        _ => "0".to_string(),
                    })
                    .collect(),
                None => Vec::new(),
            };
            if args.is_empty() {
                format!(".{}", safe_name(name))
            } else {
                format!(".{} {}", safe_name(name), args.join(" "))
            }
        })
        .collect();
    let ops_literal = format!("[{}]", ops_list.join(", "));

    // Per-step "guard is non-trivial" flag. Mirrors legacy
    // `h.who.is_some() || h.guard_str.is_some() || !h.requires.is_empty()`.
    // For MIR pilot scope: `auth.is_some() || any RequireOrAbort ||
    // transition lifecycle gate present`.
    let needs_split: Vec<bool> = ops_path
        .iter()
        .map(|name| {
            handlers
                .iter()
                .find(|h| &h.name == name)
                .map(|h| {
                    handler_auth_name(h).is_some()
                        || !h.requires_or_abort.is_empty()
                        || h.transition.is_some()
                        || !h.pre.is_empty()
                })
                .unwrap_or(false)
        })
        .collect();

    let trans_names: Vec<String> = ops_path
        .iter()
        .map(|name| safe_name(&format!("{}Transition", name)))
        .collect();

    let mut proof = String::new();
    proof.push_str(" := by\n");
    if needs_pk_binding {
        proof.push_str("  let pk : Pubkey := \u{27E8}0, 0, 0, 0\u{27E9}\n");
    }
    proof.push_str(&format!(
        "  refine \u{27E8}{}, by decide, fun s' h_apply => ?\u{5F}\u{27E9}\n",
        ops_literal
    ));

    if n == 1 {
        let trans = &trans_names[0];
        if needs_split[0] {
            proof.push_str(&format!(
                "  simp only [applyOps, applyOp, {}] at h_apply\n",
                trans
            ));
            proof.push_str("  split at h_apply\n");
            proof.push_str("  \u{B7} next heq =>\n");
            proof.push_str("    split at heq\n");
            proof.push_str(
                "    \u{B7} next hg => simp at heq h_apply; subst heq; subst h_apply; rfl\n",
            );
            proof.push_str("    \u{B7} simp at heq\n");
            proof.push_str("  \u{B7} simp at h_apply\n");
        } else {
            proof.push_str(&format!(
                "  simp only [applyOps, applyOp, {}, h, \u{2193}reduceIte] at h_apply\n",
                trans
            ));
            proof.push_str("  cases h_apply; rfl\n");
        }
    } else {
        proof.push_str("  simp only [applyOps, applyOp] at h_apply\n");
        liveness_multi_step_proof(&mut proof, &trans_names, &needs_split, 0, "  ");
    }

    proof
}

/// Recursive nested-split builder for multi-step liveness. Mirrors
/// `lean_gen::liveness_multi_step_proof`. Indentation grows by two
/// spaces per nesting depth so the emitted Lean is readable.
#[allow(clippy::only_used_in_recursion)]
fn liveness_multi_step_proof(
    proof: &mut String,
    trans_names: &[String],
    needs_split: &[bool],
    step: usize,
    indent: &str,
) {
    if step >= trans_names.len() {
        return;
    }
    let trans = &trans_names[step];
    let is_last = step == trans_names.len() - 1;

    proof.push_str(&format!("{}simp only [{}] at h_apply\n", indent, trans));
    proof.push_str(&format!("{}split at h_apply\n", indent));

    if is_last {
        if needs_split[step] {
            proof.push_str(&format!("{}\u{B7} next heq =>\n", indent));
            let inner = format!("{}  ", indent);
            proof.push_str(&format!("{}split at heq\n", inner));
            proof.push_str(&format!(
                "{}\u{B7} next hg => simp at heq h_apply; subst heq; subst h_apply; rfl\n",
                inner
            ));
            proof.push_str(&format!("{}\u{B7} simp at heq\n", inner));
        } else {
            proof.push_str(&format!("{}\u{B7} cases h_apply; rfl\n", indent));
        }
    } else if needs_split[step] {
        proof.push_str(&format!("{}\u{B7} next heq =>\n", indent));
        let inner = format!("{}  ", indent);
        proof.push_str(&format!("{}split at heq\n", inner));
        proof.push_str(&format!("{}\u{B7} next hg =>\n", inner));
        let inner2 = format!("{}  ", inner);
        proof.push_str(&format!("{}simp at heq\n", inner2));
        proof.push_str(&format!("{}subst heq\n", inner2));
        liveness_multi_step_proof(proof, trans_names, needs_split, step + 1, &inner2);
        proof.push_str(&format!("{}\u{B7} simp at heq\n", inner));
    } else {
        proof.push_str(&format!("{}\u{B7}\n", indent));
        let next_indent = format!("{}  ", indent);
        liveness_multi_step_proof(proof, trans_names, needs_split, step + 1, &next_indent);
    }
}

/// Emit `environment` preservation theorems. Mirrors
/// `lean_gen::render_environments` for the pilot scope. For each
/// (property × environment) pair, emit
/// `theorem <prop>_under_<env> (s : State) <new-field params>
///     <constraint hyps> (h_inv : <prop> s) :
///     <prop> { s with <field := new_field>... } := <proof>`.
///
/// Proof body auto-discharges with `unfold <prop> at h_inv ⊢; dsimp;
/// exact h_inv` when the mutated fields don't appear in the property
/// expression (legacy trivial-preservation shortcut). Otherwise
/// emits `:= sorry`.
fn emit_environments(out: &mut String, mir: &Mir) {
    if mir.environments.is_empty() {
        return;
    }
    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Environment \u{2014} properties hold under external state changes\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    for env in &mir.environments {
        for prop in &mir.properties {
            let prop_expr = match &prop.expression {
                Some(e) => e,
                None => continue,
            };

            // Build new_<field> param signature.
            let param_sig: String = env
                .mutates
                .iter()
                .map(|(name, ty)| format!(" (new_{} : {})", name, render_ty(ty)))
                .collect();

            // Rewrite `s.<field>` / `state.<field>` in each constraint
            // to refer to the new value. Mirrors legacy field-by-field
            // substitution at lean_gen.rs:4296.
            let constraint_hyps: String = env
                .constraints
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let mut expr = c.0.lean.clone();
                    for (field, _) in &env.mutates {
                        expr = expr
                            .replace(&format!("s.{}", field), &format!("new_{}", field))
                            .replace(&format!("state.{}", field), &format!("new_{}", field));
                    }
                    format!("\n    (h_c{} : {})", i, expr)
                })
                .collect();

            let with_parts: String = env
                .mutates
                .iter()
                .map(|(name, _)| format!("{} := new_{}", safe_name(name), name))
                .collect::<Vec<_>>()
                .join(", ");

            out.push_str(&format!(
                "theorem {}_under_{} (s : State){}{}\n",
                prop.name, env.name, param_sig, constraint_hyps
            ));
            out.push_str(&format!("    (h_inv : {} s) :\n", prop.name));

            // Trivial-preservation shortcut: if no mutated field
            // appears in the property's lean expression, the property
            // holds by reflexivity after the struct update.
            let mutated_overlap = env.mutates.iter().any(|(field, _)| {
                prop_expr.lean.contains(&format!("s.{}", safe_name(field)))
                    || prop_expr.lean.contains(&format!("state.{}", field))
            });

            if !mutated_overlap {
                out.push_str(&format!(
                    "    {} {{ s with {} }} := by\n  unfold {} at h_inv \u{22A2}; dsimp; exact h_inv\n\n",
                    prop.name, with_parts, prop.name
                ));
            } else {
                out.push_str(&format!(
                    "    {} {{ s with {} }} := sorry\n\n",
                    prop.name, with_parts
                ));
            }
        }
    }
}

/// Emit overflow-safety obligations. Mirrors
/// `lean_gen::render_overflow_obligations` for the pilot scope.
///
/// For every handler whose body issues `CheckedAdd` (an `add` effect
/// in the legacy spec model), emit a theorem stating that all numeric
/// state fields stay within their declared type bounds across the
/// transition. The pre-condition asserts inbound `valid_<T>` on each
/// numeric field; the post-condition asserts the same after.
///
/// Flat-state proofs auto-discharge via `unfold + split + cases +
/// refine + simp/omega` — same shape as `lean_gen::overflow_proof_script`
/// (see Phase 1c-10 handoff). ADT-shape proofs remain `:= by sorry`
/// until the pattern-match scrutinee form lands.
fn emit_overflow(out: &mut String, mir: &Mir) {
    emit_overflow_inner(out, mir, /* adt_form = */ false);
}

/// ADT-shape variant — closes overflow theorems with `:= by sorry`
/// matching `lean_gen::render_overflow_obligations_adt`. The
/// statement is identical to the flat shape; the difference is the
/// proof body's tactic vs term form.
fn emit_overflow_adt(out: &mut String, mir: &Mir) {
    emit_overflow_inner(out, mir, /* adt_form = */ true);
}

fn emit_overflow_inner(out: &mut String, mir: &Mir, adt_form: bool) {
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

    // Collect numeric state-field names + their MIR Ty, unioned across
    // every variant in declaration order. Mirrors the union pass in
    // `emit_state_struct`.
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut numeric_fields: Vec<(String, Ty)> = Vec::new();
    for v in &mir.state.variants {
        for f in &v.fields {
            if !matches!(
                f.ty,
                Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::U128 | Ty::I64 | Ty::I128
            ) {
                continue;
            }
            if seen.insert(f.name.clone()) {
                numeric_fields.push((f.name.clone(), f.ty.clone()));
            }
        }
    }
    if numeric_fields.is_empty() {
        return;
    }

    let valid_fn = |ty: &Ty| -> &'static str {
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
    };

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
            .map(|(n, t)| format!("{} s.{}", valid_fn(t), safe_name(n)))
            .collect();
        let post_parts: Vec<String> = numeric_fields
            .iter()
            .map(|(n, t)| format!("{} s'.{}", valid_fn(t), safe_name(n)))
            .collect();

        // Unary invariant hypotheses: properties this handler is
        // declared to preserve. Binary properties carry an `s s'`
        // shape and don't fit as single-state hypotheses; the MIR
        // lift doesn't yet carry the binary/unary tag (it lives on
        // `ParsedProperty.class`), so we conservatively include every
        // preserved-by entry whose expression exists. Pilot fixtures
        // don't tickle the binary case in their overflow theorems
        // today; revisit when that distinction lands on PropertyMir.
        let inv_hyps: Vec<&str> = mir
            .properties
            .iter()
            .filter(|p| p.expression.is_some() && p.preserved_by.contains(&h.name))
            .map(|p| p.name.as_str())
            .collect();

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

/// Generate the mechanical proof script for a flat-state overflow
/// theorem. Mirrors `lean_gen::overflow_proof_script`.
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
fn overflow_proof_script(
    mir: &Mir,
    h: &crate::mir::HandlerMir,
    numeric_fields: &[(String, crate::mir::Ty)],
) -> String {
    use crate::mir::{Stmt, Ty};

    let trans_name = safe_name(&format!("{}Transition", h.name));

    // Determine per-field whether this handler does an `add` on it
    // (only `CheckedAdd` qualifies; `WrapAdd` / `SatAdd` don't fire an
    // overflow obligation — matches legacy semantics, which keys on the
    // `"add"` op-kind only).
    let is_add_field = |field: &str| -> bool {
        h.body.stmts.iter().any(|s| match s {
            Stmt::CheckedAdd { path, .. } => path_field_name(path) == field,
            _ => false,
        })
    };

    let n = numeric_fields.len();

    // Build refine tuple parts: `h_valid` projections for unchanged
    // fields, `?_` placeholder for changed ones. Collect the changed
    // field types in order to emit one `simp; omega` line each.
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

    let mut proof = String::new();
    if has_cond {
        proof.push_str(&format!(
            " := by\n  unfold {} at h; split at h\n",
            trans_name
        ));
        proof.push_str("  · next hg =>\n    cases h\n");
        emit_body(&mut proof, "    ");
        proof.push_str("  · contradiction\n\n");
    } else {
        proof.push_str(&format!(" := by\n  unfold {} at h; cases h\n", trans_name));
        emit_body(&mut proof, "  ");
        proof.push('\n');
    }
    proof
}

/// h_valid projection path for position `i` in `n` numeric fields.
/// Mirrors `lean_gen::h_valid_projection` — right-associative ∧ chain
/// with `.2` for "drop the head" and `.1` for "take the head of the
/// remainder" (except the last position).
fn h_valid_projection_mir(i: usize, n: usize) -> String {
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
fn valid_fn_for(ty: &crate::mir::Ty) -> &'static str {
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
fn valid_module_for(ty: &crate::mir::Ty) -> &'static str {
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
fn valid_max_for(ty: &crate::mir::Ty) -> &'static str {
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

/// Render a MIR `Ty` to its Lean form. Mirrors the encoding used by
/// `lean_gen::render_ty_for_field` for compatibility — every numeric
/// type widens to `Nat` since proofs run in Nat, and Pubkey is an
/// opaque Lean abbreviation.
fn render_ty(ty: &crate::mir::Ty) -> String {
    use crate::mir::Ty;
    match ty {
        Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::U128 => "Nat".to_string(),
        Ty::I64 | Ty::I128 => "Int".to_string(),
        Ty::Bool => "Bool".to_string(),
        Ty::Pubkey => "Pubkey".to_string(),
        Ty::Custom(name) => name.clone(),
        Ty::Map { capacity: _, value } => {
            // Phase 1 stub: indexed-state has its own renderer; this
            // codepath shouldn't fire for single-account pilot specs.
            format!("Map /* {} */", render_ty(value))
        }
    }
}

/// Build a parameter signature string for transition function
/// declarations: `" (p1 : T1) (p2 : T2) ..."`. Empty string when
/// `params` is empty. Mirrors `lean_gen::param_sig_str` for the
/// MIR-typed parameter list.
fn param_sig_str(params: &[(crate::mir::Symbol, crate::mir::Ty)]) -> String {
    if params.is_empty() {
        return String::new();
    }
    params
        .iter()
        .map(|(n, t)| format!(" ({} : {})", n, render_ty(t)))
        .collect::<Vec<_>>()
        .join("")
}

/// Extract the auth-account name for the alias-let, if any. For the
/// pilot scope, `auth <name>` lowers to `Some(AccountOrField::Account(ByBinding(name)))`.
/// Returns `None` for permissionless handlers and dotted-auth shapes
/// (which were desugared into a synthetic `requires` clause upstream
/// and don't need a separate alias).
fn handler_auth_name(h: &crate::mir::HandlerMir) -> Option<crate::mir::Symbol> {
    use crate::mir::{AccountOrField, AccountRef};
    match &h.auth {
        Some(AccountOrField::Account(AccountRef::ByBinding(name))) => Some(name.clone()),
        _ => None,
    }
}

/// Extract the field name from a Path. For Phase 1c we accept dotted
/// paths but emit only the trailing segment (matches lean_gen.rs's
/// `strip_variant_prefix_for_flat_state` behavior on the flat-state
/// path).
fn path_field_name(path: &crate::mir::Path) -> String {
    path.segments
        .last()
        .cloned()
        .unwrap_or_else(|| "?".to_string())
}

/// Detect whether the RHS string references an account-binding's
/// `.pubkey` field — `lean_gen.rs:1839` drops these from the record
/// update since they have no Lean scope.
fn is_account_pubkey_ref(rust: &str) -> bool {
    // Heuristic matching lean_gen.rs::is_account_binding_pubkey_ref:
    // the RHS is exactly `<identifier>.pubkey`.
    let trimmed = rust.trim();
    trimmed
        .strip_suffix(".pubkey")
        .map(|head| !head.is_empty() && head.chars().all(|c| c.is_alphanumeric() || c == '_'))
        .unwrap_or(false)
}

/// Wrap an expression in parens if it contains low-precedence operators
/// that would re-group when joined under `∧`. Mirrors
/// `lean_gen::paren_if_low_prec` — defensive parens at concat sites
/// (the mitigation for divergence class C3 in
/// `docs/design/codegen-divergence.md`).
fn paren_low_prec(expr: &str) -> String {
    let trimmed = expr.trim();
    // Already-parenthesized at the top level: leave alone.
    if trimmed.starts_with('(') && trimmed.ends_with(')') {
        // Check the parens actually match (could be `(a) ∧ (b)`).
        let mut depth = 0i32;
        let mut top_level_seen = false;
        for c in trimmed.chars() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {
                    if depth == 0 {
                        top_level_seen = true;
                        break;
                    }
                }
            }
        }
        if !top_level_seen {
            return trimmed.to_string();
        }
    }
    // Look for low-precedence ops (or / and) at the top level.
    if has_top_level_op(trimmed, &[" or ", " ∨ ", " || "]) {
        format!("({})", trimmed)
    } else {
        trimmed.to_string()
    }
}

fn has_top_level_op(expr: &str, ops: &[&str]) -> bool {
    let mut depth = 0i32;
    for (i, c) in expr.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ if depth == 0 => {
                for op in ops {
                    if expr[i..].starts_with(op) {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

/// Quote Lean reserved names. Today minimal — extend as fixtures
/// surface collisions. `lean_gen.rs::safe_name` uses `«name»` quoting
/// for the same purpose; keep the contract identical.
fn safe_name(name: &str) -> String {
    // Lean reserved words / keywords that collide with common qedspec
    // identifiers (notably `initialize`, used as a canonical handler
    // name across the pilot fixtures). Kept byte-identical to
    // `lean_gen.rs::safe_name` so MIR + legacy emit the same `«name»`
    // quoting.
    const LEAN_RESERVED: &[&str] = &[
        "open",
        "close",
        "initialize",
        "import",
        "namespace",
        "end",
        "where",
        "with",
        "do",
        "let",
        "if",
        "then",
        "else",
        "match",
        "return",
        "in",
        "for",
    ];
    if LEAN_RESERVED.contains(&name) {
        format!("\u{00AB}{}\u{00BB}", name)
    } else {
        name.to_string()
    }
}

// ----------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path as FsPath;

    /// sBPF Lean codegen regression gate. The 5 bundled `examples/sbpf/*`
    /// specs use modern `handler` syntax with no `instruction` blocks, so
    /// they only exercise `render_sbpf`'s header path. The old-syntax
    /// `Dropset` fixture exercises the full renderer: per-instruction
    /// namespaces, offset/`ea_*` lemmas, guard theorem stubs (`==`, `>=`,
    /// field-vs-field RHS, no-`checks`), the completeness `structure
    /// Spec`, and property `True := trivial` stubs.
    ///
    /// The golden was captured from the v2.32 port, which was proven
    /// byte-identical to the (now-deleted) legacy `lean_gen::render_sbpf`
    /// before deletion. Regenerate intentionally if `render_sbpf` changes:
    /// `UPDATE_SBPF_GOLDEN=1 cargo test sbpf_render_matches_golden`.
    const DROPSET_SBPF_SPEC: &str = include_str!("../tests/fixtures/dropset_sbpf.qedspec");
    const DROPSET_SBPF_GOLDEN: &str =
        include_str!("../tests/fixtures/dropset_sbpf.Spec.lean.golden");

    #[test]
    fn sbpf_render_matches_golden() {
        let parsed = crate::chumsky_adapter::parse_str(DROPSET_SBPF_SPEC)
            .expect("parse dropset sBPF fixture");
        assert!(
            parsed.is_assembly_target(),
            "dropset fixture should be an assembly target"
        );
        let ported = render_sbpf(&parsed);

        if std::env::var("UPDATE_SBPF_GOLDEN").is_ok() {
            std::fs::write(
                format!(
                    "{}/tests/fixtures/dropset_sbpf.Spec.lean.golden",
                    std::env::var("CARGO_MANIFEST_DIR").unwrap()
                ),
                &ported,
            )
            .unwrap();
            return;
        }

        // Guard against a vacuous golden: the full renderer must fire.
        for marker in [
            "namespace RegisterMarket",
            "@[simp] theorem ea_",
            "theorem rejects_invalid_discriminant",
            "structure Spec (progAt",
            "theorem memory_safety : True := trivial",
        ] {
            assert!(
                ported.contains(marker),
                "ported sBPF output missing `{marker}`:\n{ported}"
            );
        }

        assert_eq!(
            DROPSET_SBPF_GOLDEN, ported,
            "render_sbpf output drifted from the golden — \
             regenerate with UPDATE_SBPF_GOLDEN=1 if intentional"
        );
    }

    fn lower_fixture(rel_path: &str) -> Mir {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let workspace_root = FsPath::new(&manifest_dir)
            .ancestors()
            .nth(2)
            .expect("workspace root above crates/qedgen");
        let spec_path = workspace_root.join(rel_path);
        let parsed = crate::check::parse_spec_file(&spec_path)
            .unwrap_or_else(|e| panic!("parse {}: {e}", spec_path.display()));
        crate::mir::lower(&parsed)
    }

    /// v2.33 — the inductive multi-variant State representation is opted
    /// into via `pragma state_repr = adt`, decoupled from the incidental
    /// `WrongState` error variant that keyed it pre-v2.33 (the footgun:
    /// adding/removing a lifecycle error silently flipped flat↔ADT).
    /// Same State shape + same `WrongState` error ⇒ flat by default,
    /// `inductive State` only when the pragma is present.
    #[test]
    fn state_repr_pragma_dispatches_inductive_vs_flat() {
        // No effect bodies → no `Variant.field`-vs-bare effect-syntax
        // dependence; the dispatch keys only on the pragma + shape.
        let body = "\n\
            program_id \"11111111111111111111111111111111\"\n\
            \n\
            type State\n\
            \x20 | Uninitialized\n\
            \x20 | Active of { balance : U64 }\n\
            \x20 | Closed\n\
            \n\
            type Error\n\
            \x20 | InvalidAmount\n\
            \x20 | WrongState\n\
            \n\
            handler open (amount : U64) : State.Uninitialized -> State.Active {\n\
            \x20 auth owner\n\
            \x20 accounts { owner : signer, writable }\n\
            \x20 requires amount > 0 else InvalidAmount\n\
            }\n";

        let flat = crate::chumsky_adapter::parse_str(&format!("spec Flat\n{body}"))
            .expect("parse flat spec");
        let flat_lean = render(&crate::mir::lower(&flat));
        assert!(
            flat_lean.contains("structure State where"),
            "default (no pragma) must lower to the flat struct"
        );
        assert!(
            !flat_lean.contains("inductive State where"),
            "default must NOT take the inductive ADT path"
        );

        let adt = crate::chumsky_adapter::parse_str(&format!(
            "spec Adt\npragma state_repr = adt\n{body}"
        ))
        .expect("parse adt spec");
        let adt_mir = crate::mir::lower(&adt);
        assert!(adt_mir.adt_state, "pragma must lift to Mir::adt_state");
        let adt_lean = render(&adt_mir);
        assert!(
            adt_lean.contains("inductive State where"),
            "pragma state_repr = adt must route to render_single_account_adt"
        );
    }

    #[test]
    fn flat_effect_rhs_field_ref_is_qualified() {
        // Issue #79 Bug A: a `set` effect copying one state field into
        // another (`reserved := state.cap`) is stored bare (`cap`) by the
        // adapter. The flat-state transition emitter used to emit it
        // verbatim — `reserved := cap` — which is an unknown identifier in
        // Lean. It must re-qualify to `s.cap` (as the indexed path and the
        // proptest backend already do), while a handler param stays bare.
        let src = "spec BudgetRepro\n\
            type State\n\
            \x20 | Uninitialized\n\
            \x20 | Active of { cap : U64, used : U64, reserved : U64 }\n\
            type Error\n\
            \x20 | Zero\n\
            handler open (c : U64) : State.Uninitialized -> State.Active {\n\
            \x20 permissionless\n\
            \x20 accounts { payer : signer, writable }\n\
            \x20 requires c > 0 else Zero\n\
            \x20 effect { cap := c\n\
            \x20          used := 0\n\
            \x20          reserved := 0 }\n\
            }\n\
            handler reset : State.Active -> State.Active {\n\
            \x20 permissionless\n\
            \x20 accounts { caller : signer }\n\
            \x20 effect { used := 0\n\
            \x20          reserved := state.cap }\n\
            }\n";
        let parsed = crate::chumsky_adapter::parse_str(src).expect("parse budget spec");
        let out = render(&crate::mir::lower(&parsed));
        assert!(
            out.contains("reserved := s.cap"),
            "state-field RHS must render qualified `s.cap`:\n{out}"
        );
        assert!(
            !out.contains("reserved := cap,") && !out.contains("reserved := cap "),
            "bare `cap` (unknown identifier) must not appear:\n{out}"
        );
        // A handler param assigned to a field stays bare (it IS in scope).
        assert!(
            out.contains("cap := c"),
            "handler-param RHS must stay bare `c`:\n{out}"
        );
    }

    #[test]
    fn render_emits_header_namespace_state() {
        let mir = lower_fixture("examples/rust/escrow/escrow.qedspec");
        let out = render(&mir);

        // Header imports present.
        assert!(out.contains("import QEDGen.Solana.Account"));
        assert!(out.contains("import QEDGen.Solana.State"));

        // Namespace matches the spec name.
        assert!(out.contains("namespace Escrow"));
        assert!(out.contains("end Escrow"));

        // open QEDGen.Solana follows the namespace.
        assert!(out.contains("open QEDGen.Solana"));
    }

    #[test]
    fn render_lifecycle_marker_threshold() {
        // escrow has 3 lifecycle states (Uninitialized | Open | Closed)
        // so Status inductive is emitted.
        let mir = lower_fixture("examples/rust/escrow/escrow.qedspec");
        let out = render(&mir);
        // Lifecycle is the lifecycle_states vec on StateAdt; for the
        // multi-variant ADT path of escrow, lifecycle_states is
        // populated by `lower_state`. If the count is < 2, no Status
        // marker — verify the boundary.
        let lifecycle_count = mir.state.lifecycle_states.len();
        if lifecycle_count >= 2 {
            assert!(out.contains("inductive Status"));
        } else {
            assert!(!out.contains("inductive Status"));
        }
    }

    #[test]
    fn render_aborts_if_clauses() {
        let mir = lower_fixture("examples/rust/escrow/escrow.qedspec");
        let out = render(&mir);

        // escrow declares `requires deposit_amount > 0 and receive_amount > 0
        // else InvalidAmount` on initialize — should produce an
        // `initialize_aborts_if_InvalidAmount` theorem with the
        // negated predicate as hypothesis.
        assert!(
            out.contains("theorem initialize_aborts_if_InvalidAmount"),
            "expected initialize_aborts_if_InvalidAmount theorem:\n{}",
            &out[..out.len().min(2000)]
        );
        // The hypothesis should be the negation of the requires
        // predicate.
        assert!(
            out.contains("¬(deposit_amount > 0"),
            "expected negated requires hypothesis"
        );
        // The abort-conditions header should appear.
        assert!(out.contains("Abort conditions"));
    }

    #[test]
    fn render_skips_account_pubkey_aborts() {
        // exchange + cancel both have `requires initializer_ta.pubkey == ...`
        // — these reference a handler-account's .pubkey field which isn't
        // in Lean scope. The filter should skip them.
        let mir = lower_fixture("examples/rust/escrow/escrow.qedspec");
        let out = render(&mir);
        // No theorem should reference `initializer_ta.pubkey` in its
        // hypothesis — it's filtered out.
        assert!(
            !out.contains("(h : ¬(initializer_ta.pubkey"),
            "account-pubkey requires should be filtered from abort theorems:\n{}",
            out
        );
    }

    #[test]
    fn render_emits_constants() {
        // Percolator declares MAX_ACCOUNTS, MAX_VAULT_TVL,
        // POS_SCALE, MAX_ACCOUNT_NOTIONAL.
        let mir = lower_fixture("examples/rust/percolator/percolator.qedspec");
        let out = render(&mir);
        assert!(
            out.contains("abbrev MAX_ACCOUNTS : Nat := 1024"),
            "expected MAX_ACCOUNTS abbrev"
        );
        assert!(out.contains("abbrev POS_SCALE : Nat := 1000000"));
    }

    #[test]
    fn render_emits_uninterpreted_helpers() {
        // Synthetic MIR with an uninterpreted helper. Pilot fixtures
        // don't declare helpers explicitly (check.rs infers them when
        // an undeclared fn is referenced), so build the MIR by hand
        // to exercise the emit path.
        let mir = Mir {
            name: "T".to_string(),
            state: crate::mir::StateAdt::default(),
            account_states: vec![],
            accounts: crate::mir::AccountTable::default(),
            errors: crate::mir::ErrorEnum::default(),
            imports: std::collections::BTreeMap::new(),
            handlers: vec![],
            invariants: vec![],
            events: vec![],
            constants: vec![],
            uninterpreted_helpers: vec![crate::mir::UninterpretedHelper {
                name: "is_valid".to_string(),
                arg_types: vec!["Nat".to_string()],
                return_type: "Bool".to_string(),
            }],
            ref_impls: vec![],
            properties: vec![],
            covers: vec![],
            liveness_props: vec![],
            environments: vec![],
            ghosts: vec![],
            records: vec![],
            is_assembly: false,
            adt_state: false,
        };
        let out = render(&mir);
        assert!(
            out.contains("opaque is_valid : Nat \u{2192} Bool"),
            "expected opaque is_valid : Nat → Bool in:\n{}",
            out
        );
        assert!(out.contains("Uninterpreted helpers"));
    }

    #[test]
    fn render_emits_ref_impls() {
        let mir = Mir {
            name: "T".to_string(),
            state: crate::mir::StateAdt::default(),
            account_states: vec![],
            accounts: crate::mir::AccountTable::default(),
            errors: crate::mir::ErrorEnum::default(),
            imports: std::collections::BTreeMap::new(),
            handlers: vec![],
            invariants: vec![],
            events: vec![],
            constants: vec![],
            uninterpreted_helpers: vec![],
            ref_impls: vec![crate::mir::RefImpl {
                name: "scale".to_string(),
                doc: None,
                params: vec![
                    ("a".to_string(), "U64".to_string()),
                    ("b".to_string(), "U64".to_string()),
                ],
                return_type: "U64".to_string(),
                lean_body: "a * b".to_string(),
                rust_body: "a * b".to_string(),
            }],
            properties: vec![],
            covers: vec![],
            liveness_props: vec![],
            environments: vec![],
            ghosts: vec![],
            records: vec![],
            is_assembly: false,
            adt_state: false,
        };
        let out = render(&mir);
        assert!(
            out.contains("def scale (a : Nat) (b : Nat) : Nat := a * b"),
            "expected ref_impl scale lowered:\n{}",
            out
        );
    }

    #[test]
    fn render_emits_properties_with_preservation() {
        // Lending declares `property pool_solvency : ...` and names
        // handlers it's preserved by. v2.30 Phase 2: lending is
        // multi-account (Pool + Loan), so the property predicate and
        // master theorem both bind to `PoolState` / `PoolOperation`
        // (the property's fields live on the Pool account).
        let mir = lower_fixture("examples/rust/lending/lending.qedspec");
        let out = render(&mir);

        assert!(
            out.contains("def pool_solvency (s : PoolState) : Prop :="),
            "expected pool_solvency predicate def on PoolState:\n{}",
            &out[..out.len().min(3000)]
        );

        assert!(
            out.contains("theorem pool_solvency_inductive"),
            "expected pool_solvency_inductive master theorem"
        );
        assert!(out.contains("(op : PoolOperation)"));
    }

    #[test]
    fn render_emits_invariant_theorems() {
        // Lending declares `invariant collateral_backing`. v2.30
        // Phase 2: multi-account specs emit invariants as structured
        // comments (matches legacy `render_invariants_as_comments`);
        // variant-typed binder lowering is a v3.0 item.
        let mir = lower_fixture("examples/rust/lending/lending.qedspec");
        let out = render(&mir);
        assert!(
            out.contains("-- INVARIANT OBLIGATION (declared, multi-account translation deferred): collateral_backing"),
            "expected collateral_backing invariant comment"
        );
        assert!(
            out.contains("--   predicate body:"),
            "expected predicate body line in invariant comment"
        );
    }

    #[test]
    fn prefix_state_fields_word_boundary() {
        let mut fields = std::collections::HashSet::new();
        fields.insert("amount".to_string());
        fields.insert("taker".to_string());

        // Bare field references get prefixed.
        let out = prefix_state_fields("amount > 0", &fields);
        assert_eq!(out, "s.amount > 0");

        // Substrings inside longer identifiers are NOT prefixed
        // (word-boundary regex). Tricky: `taker_amount` contains both
        // `taker` and `amount` as substrings but neither as a whole
        // word.
        let out = prefix_state_fields("taker_amount > 0", &fields);
        assert_eq!(out, "taker_amount > 0");
    }

    #[test]
    fn render_emits_cover_theorems() {
        // Lending declares two cover blocks whose traces span both
        // accounts (Pool init_pool/deposit + Loan borrow/repay or
        // borrow/liquidate). v2.30 Phase 2 emits skip-comments for
        // cross-account traces and still writes the cover section
        // header (matches legacy multi-account behavior). Cover-
        // theorem auto-discharge for single-account traces is
        // covered by the escrow snapshot.
        let mir = lower_fixture("examples/rust/lending/lending.qedspec");
        let out = render(&mir);
        assert!(
            out.contains("-- Cover properties"),
            "expected cover section header even when all skipped"
        );
        assert!(
            out.contains(
                "-- cover_borrow_repay_cycle: trace [init_pool, deposit, borrow, repay] spans multiple account types, skipped"
            ),
            "expected borrow_repay_cycle skip-comment"
        );
        assert!(
            out.contains(
                "-- cover_liquidation_path: trace [init_pool, deposit, borrow, liquidate] spans multiple account types, skipped"
            ),
            "expected liquidation_path skip-comment"
        );
    }

    #[test]
    fn render_emits_liveness_theorems() {
        // Lending: `liveness loan_settles : Loan.Active ~> Loan.Empty
        // via [repay] within 1`. v2.30 Phase 2 resolves the per-
        // liveness state type from `via_ops[0].on_account` → the
        // `repay` handler is qualified `Loan.Active -> Loan.Empty` so
        // the theorem binds to `LoanState` + `applyLoanOps` /
        // `applyLoanOp` / `LoanOperation`. The legacy auto-discharge
        // script fires when `find_liveness_path` succeeds, yielding
        // the universal-implication form with a closed proof — no
        // trailing `sorry` for the lending pilot.
        let mir = lower_fixture("examples/rust/lending/lending.qedspec");
        let out = render(&mir);
        assert!(
            out.contains("-- Liveness properties"),
            "expected liveness section header"
        );
        assert!(
            out.contains("def applyLoanOps (s : LoanState)"),
            "expected applyLoanOps helper bound to LoanState"
        );
        assert!(
            out.contains("theorem liveness_loan_settles (s : LoanState)"),
            "expected liveness_loan_settles theorem on LoanState"
        );
        assert!(
            out.contains("ops.length \u{2264} 1"),
            "expected within-step bound of 1"
        );
        assert!(
            out.contains("\u{2200} s', applyLoanOps s signer ops = some s'"),
            "expected auto-discharged universal-implication form"
        );
    }

    #[test]
    fn render_emits_environment_theorems() {
        // Lending declares
        //   environment interest_rate_change { mutates interest_rate :
        //   U64; constraint interest_rate > 0 }
        // and `property pool_solvency` — cross product emits one
        // `pool_solvency_under_interest_rate_change` theorem.
        let mir = lower_fixture("examples/rust/lending/lending.qedspec");
        let out = render(&mir);
        assert!(
            out.contains("-- Environment"),
            "expected environment section header"
        );
        assert!(
            out.contains("theorem pool_solvency_under_interest_rate_change"),
            "expected pool_solvency_under_interest_rate_change theorem"
        );
        assert!(
            out.contains("new_interest_rate : Nat"),
            "expected new_<field> param of MIR-rendered type"
        );
        assert!(
            out.contains("(h_inv : pool_solvency s)"),
            "expected (h_inv : <prop> s) hypothesis"
        );
        assert!(
            out.contains("{ s with interest_rate := new_interest_rate }"),
            "expected struct-update with mutated field"
        );
    }

    #[test]
    fn render_emits_overflow_theorems() {
        // Lending: `deposit` issues a `+=` effect against numeric
        // pool fields, which MIR lowers to `CheckedAdd` (the default
        // arithmetic mode post-v2.7 G3). The overflow emitter picks
        // it up and produces a `deposit_overflow_safe` theorem.
        let mir = lower_fixture("examples/rust/lending/lending.qedspec");
        let out = render(&mir);
        assert!(
            out.contains("-- Overflow safety obligations"),
            "expected overflow section header"
        );
        assert!(
            out.contains("theorem deposit_overflow_safe"),
            "expected deposit_overflow_safe theorem"
        );
        // Pre-condition asserts `valid_<T>` on each numeric field.
        assert!(out.contains("valid_u64"), "expected valid_u64 in pre/post");
        assert!(
            out.contains("= some s'"),
            "expected `= some s'` hypothesis on transition"
        );
        // Overflow theorem now auto-discharges via the ported
        // `overflow_proof_script`: `unfold + split + cases + refine +
        // simp/omega`. The `:= sorry` form is reserved for the ADT
        // path (`emit_overflow_adt`) until that variant lands.
        assert!(
            out.contains("simp only [valid_u64, Valid.valid_u64, Valid.U64_MAX]; omega"),
            "expected overflow proof to discharge the changed-field obligation via `simp; omega`"
        );
        assert!(
            out.contains("unfold depositTransition at h; split at h"),
            "expected overflow proof to unfold the transition and split the guard"
        );
    }

    #[test]
    fn render_pilot_fixtures_no_panic() {
        for fixture in &[
            "examples/rust/escrow/escrow.qedspec",
            "examples/rust/lending/lending.qedspec",
            "examples/rust/multisig/multisig.qedspec",
            "examples/rust/bundled-stdlib-demo/pool.qedspec",
        ] {
            let mir = lower_fixture(fixture);
            let out = render(&mir);
            assert!(out.contains("namespace "), "{}", fixture);
            assert!(out.contains("end "), "{}", fixture);
        }
    }
}
