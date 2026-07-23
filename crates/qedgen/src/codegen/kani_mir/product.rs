//! #324 — product-state lowering for multi-account file-level features.
//!
//! Covers, liveness, and environment obligations are spec-global: a trace
//! may step handlers routed to different accounts, so per-account
//! duplication would change its semantics. This module lowers them ONCE,
//! over a `ProductState` that holds one component per emitted account
//! module and delegates every transition to the per-account transition
//! fn — the product model adds no second copy of transition semantics.
//!
//! Shapes the lowering cannot resolve to modeled components stay
//! `unsupported(kani_multi_account_file_level)` in the obligation
//! manifest, with a structured comment in the artifact — a requested
//! obligation never disappears silently (#332 contract).
//!
//! Deliberate scope limits (each records unsupported, never guesses):
//!   * handlers needing a symbolic account env (their env structs live
//!     inside the account module and the binding emitter is unqualified);
//!   * handler params typed as spec records / sums (those types are
//!     emitted per account module and are not nameable here);
//!   * properties that resolve to zero or multiple account components;
//!   * ghost-reading properties and ghost-mutating environments — the
//!     product ghost component lands with #331.

use super::*;
use crate::obligations::{ObligationKind, ObligationRecorder, UnsupportedReason};

/// One emitted per-account module, as seen by the product lowering.
/// `scoped_handlers` is captured from the scoped spec the account module
/// was ACTUALLY emitted from, so routing here can never drift from what
/// `scope_parsed_to_account` decided.
pub(crate) struct ProductComponent {
    pub acct: crate::check::ParsedAccountType,
    pub mod_name: String,
    pub scoped_handlers: Vec<crate::check::ParsedHandler>,
}

impl ProductComponent {
    /// Component state fields as the account module's `State` declares
    /// them: account fields plus spec ghosts (the structural emitter
    /// chains ghosts into every per-account State).
    fn state_fields(&self, parsed: &ParsedSpec) -> Vec<(String, String)> {
        self.acct
            .fields
            .iter()
            .cloned()
            .chain(parsed.ghosts.iter().map(|g| (g.name.clone(), g.ty.clone())))
            .collect()
    }

    fn has_lifecycle(&self) -> bool {
        self.acct.lifecycle.len() >= 2
    }
}

/// A handler the product module can wrap: routed to an emitted component
/// and free of the shapes listed in the module doc.
struct WrappedHandler<'a> {
    op: &'a crate::check::ParsedHandler,
    component_mod: &'a str,
}

pub(crate) fn emit_product_module(
    out: &mut String,
    mir: &Mir,
    parsed: &ParsedSpec,
    components: &[ProductComponent],
    progress: bool,
    rec: &mut ObligationRecorder,
) -> Result<()> {
    if parsed.covers.is_empty()
        && parsed.liveness_props.is_empty()
        && parsed.environments.is_empty()
    {
        return Ok(());
    }
    if progress {
        eprintln!("Rendering Kani product-state module (file-level features)");
    }

    let wrappable = resolve_wrappable(parsed, components);
    let cover_plans = plan_covers(parsed, &wrappable);
    let liveness_plans = plan_liveness(parsed, components, &wrappable);
    let env_plans = plan_environments(parsed, components);

    // Wrappers are emitted only for handlers a lowered harness calls.
    let mut wrapper_names: Vec<&str> = Vec::new();
    for plan in &cover_plans {
        if let CoverPlan::Lower { trace_ops, .. } = plan {
            wrapper_names.extend(trace_ops.iter().map(|w| w.op.name.as_str()));
        }
    }
    for plan in &liveness_plans {
        if let LivenessPlan::Lower { via, .. } = plan {
            wrapper_names.extend(via.iter().map(|w| w.op.name.as_str()));
        }
    }
    wrapper_names.sort_unstable();
    wrapper_names.dedup();

    let any_product_harness = !wrapper_names.is_empty();
    let any_env_harness = env_plans.iter().any(|p| p.owner.is_ok());
    let any_lowered = any_product_harness || any_env_harness;

    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Product state (#324) — file-level covers / liveness / environment\n");
    out.push_str("// lowered once over the per-account components; transitions delegate\n");
    out.push_str("// to the account modules so there is no second copy of the semantics.\n");
    out.push_str(
        "// ============================================================================\n\n",
    );
    out.push_str("mod product {\n");
    if any_lowered {
        out.push_str("    use super::*;\n\n");
    }

    if any_product_harness {
        emit_product_state_struct(out, components);
        emit_wrappers(out, parsed, &wrapper_names, &wrappable)?;
    }

    emit_covers(out, parsed, components, &cover_plans, rec)?;
    emit_liveness(out, parsed, components, &liveness_plans, rec)?;
    emit_environments(out, mir, parsed, components, &env_plans, rec)?;

    out.push_str("} // mod product\n\n");
    Ok(())
}

// ── Resolution ──────────────────────────────────────────────────────

/// Collect the wrappable handlers. A handler is wrappable when it is
/// routed to an emitted component, needs no symbolic account env, and
/// every param type renders without naming a module-scoped record / sum.
fn resolve_wrappable<'a>(
    parsed: &'a ParsedSpec,
    components: &'a [ProductComponent],
) -> Vec<WrappedHandler<'a>> {
    use crate::codegen_shared::map_type;
    use crate::rust_codegen_util as util;

    let module_scoped_types: Vec<&str> = parsed
        .records
        .iter()
        .map(|r| r.name.as_str())
        .chain(parsed.sum_types.iter().map(|s| s.name.as_str()))
        .collect();

    let mut out = Vec::new();
    for comp in components {
        for op in &comp.scoped_handlers {
            if util::handler_needs_account_env(op) {
                continue;
            }
            let param_types_ok = op
                .takes_params
                .iter()
                .chain(op.abstract_binders.iter())
                .all(|(_, t)| {
                    map_type(t, parsed).is_ok_and(|rust_ty| {
                        !module_scoped_types
                            .iter()
                            .any(|name| rust_type_mentions(&rust_ty, name))
                    })
                });
            if !param_types_ok {
                continue;
            }
            out.push(WrappedHandler {
                op,
                component_mod: &comp.mod_name,
            });
        }
    }
    out
}

/// Whether a rendered Rust type contains `name` as an identifier. The
/// product module is a sibling of each account module, so record/sum names
/// nested under `Option`, `Vec`, arrays, or aliases are still unavailable
/// unless qualified through their owning module.
fn rust_type_mentions(rust_ty: &str, name: &str) -> bool {
    rust_ty
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|ident| ident == name)
}

fn wrapped<'a, 'b>(
    wrappable: &'b [WrappedHandler<'a>],
    name: &str,
) -> Option<&'b WrappedHandler<'a>> {
    wrappable.iter().find(|w| w.op.name == name)
}

enum CoverPlan<'a> {
    Lower {
        cover_name: &'a str,
        trace_index: usize,
        trace_ops: Vec<&'a WrappedHandler<'a>>,
        multi_trace: bool,
    },
    Unsupported {
        cover_name: &'a str,
        trace_index: usize,
        why: String,
    },
}

fn plan_covers<'a>(
    parsed: &'a ParsedSpec,
    wrappable: &'a [WrappedHandler<'a>],
) -> Vec<CoverPlan<'a>> {
    let mut plans = Vec::new();
    for cover in &parsed.covers {
        for (i, trace) in cover.traces.iter().enumerate() {
            let mut ops = Vec::with_capacity(trace.len());
            let mut missing: Option<String> = None;
            for op_name in trace {
                match wrapped(wrappable, op_name) {
                    Some(w) => ops.push(w),
                    None => {
                        missing = Some(op_name.clone());
                        break;
                    }
                }
            }
            plans.push(match missing {
                None => CoverPlan::Lower {
                    cover_name: &cover.name,
                    trace_index: i,
                    trace_ops: ops,
                    multi_trace: cover.traces.len() > 1,
                },
                Some(op_name) => CoverPlan::Unsupported {
                    cover_name: &cover.name,
                    trace_index: i,
                    why: format!("trace op `{}` has no product wrapper", op_name),
                },
            });
        }
    }
    plans
}

enum LivenessPlan<'a> {
    Lower {
        liveness: &'a crate::check::ParsedLiveness,
        owner_index: usize,
        via: Vec<&'a WrappedHandler<'a>>,
    },
    Unsupported {
        liveness: &'a crate::check::ParsedLiveness,
        why: String,
    },
}

fn plan_liveness<'a>(
    parsed: &'a ParsedSpec,
    components: &'a [ProductComponent],
    wrappable: &'a [WrappedHandler<'a>],
) -> Vec<LivenessPlan<'a>> {
    let mut plans = Vec::new();
    for liveness in &parsed.liveness_props {
        // Owner = the unique component whose lifecycle declares BOTH
        // endpoint states. The adapter strips `Loan.Active` to `Active`,
        // so ambiguity across components is detectable, not resolvable.
        let owners: Vec<usize> = components
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                c.has_lifecycle()
                    && c.acct.lifecycle.contains(&liveness.from_state)
                    && c.acct.lifecycle.contains(&liveness.leads_to_state)
            })
            .map(|(i, _)| i)
            .collect();
        let plan = match owners.as_slice() {
            [owner_index] => {
                let mut via = Vec::with_capacity(liveness.via_ops.len());
                let mut missing: Option<String> = None;
                for op_name in &liveness.via_ops {
                    match wrapped(wrappable, op_name) {
                        Some(w) => via.push(w),
                        None => {
                            missing = Some(op_name.clone());
                            break;
                        }
                    }
                }
                match missing {
                    None => LivenessPlan::Lower {
                        liveness,
                        owner_index: *owner_index,
                        via,
                    },
                    Some(op_name) => LivenessPlan::Unsupported {
                        liveness,
                        why: format!("via op `{}` has no product wrapper", op_name),
                    },
                }
            }
            [] => LivenessPlan::Unsupported {
                liveness,
                why: format!(
                    "no modeled account lifecycle declares both `{}` and `{}`",
                    liveness.from_state, liveness.leads_to_state
                ),
            },
            _ => LivenessPlan::Unsupported {
                liveness,
                why: format!(
                    "states `{}` ~> `{}` are ambiguous across account lifecycles",
                    liveness.from_state, liveness.leads_to_state
                ),
            },
        };
        plans.push(plan);
    }
    plans
}

struct EnvPlan<'a> {
    env: &'a crate::check::ParsedEnvironment,
    prop: &'a crate::check::ParsedProperty,
    owner: std::result::Result<usize, String>,
}

fn plan_environments<'a>(
    parsed: &'a ParsedSpec,
    components: &'a [ProductComponent],
) -> Vec<EnvPlan<'a>> {
    let mut plans = Vec::new();
    for env in &parsed.environments {
        for prop in &parsed.properties {
            if prop.expression.is_none() {
                continue;
            }
            plans.push(EnvPlan {
                env,
                prop,
                owner: resolve_env_owner(parsed, components, env, prop),
            });
        }
    }
    plans
}

/// The environment harness runs over ONE component: the property and every
/// mutated field must resolve to the same account, and nothing may touch a
/// ghost (#331 owns the product ghost component).
fn resolve_env_owner(
    parsed: &ParsedSpec,
    components: &[ProductComponent],
    env: &crate::check::ParsedEnvironment,
    prop: &crate::check::ParsedProperty,
) -> std::result::Result<usize, String> {
    if prop.class == crate::check::PropertyClass::Binary {
        return Err("binary (old/new) properties are not lowered over a component".into());
    }
    let Some(rust) = prop.rust_expression.as_deref() else {
        return Err("property has no Rust-renderable body".into());
    };
    if parsed
        .ghosts
        .iter()
        .any(|g| references_state_field(rust, &g.name))
    {
        return Err("property reads a spec-global ghost (#331)".into());
    }

    let owners: Vec<usize> = components
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            c.acct
                .fields
                .iter()
                .any(|(f, _)| references_state_field(rust, f))
        })
        .map(|(i, _)| i)
        .collect();
    let [owner_index] = owners.as_slice() else {
        return Err(if owners.is_empty() {
            "property references no modeled account field".to_string()
        } else {
            "property references fields of more than one account".to_string()
        });
    };
    let owner = &components[*owner_index];

    // The per-account module only emits predicates for properties its
    // scoping filter kept — require membership so `<mod>::<prop>` exists.
    let scoped_has_prop = prop
        .expression
        .as_deref()
        .map(|expr| {
            owner
                .acct
                .fields
                .iter()
                .any(|(f, _)| expr.contains(f.as_str()))
        })
        .unwrap_or(false);
    if !scoped_has_prop {
        return Err(format!(
            "property predicate is not emitted in mod {}",
            owner.mod_name
        ));
    }

    for (field, _) in &env.mutates {
        if parsed.ghosts.iter().any(|g| &g.name == field) {
            return Err("environment mutates a spec-global ghost (#331)".into());
        }
        if !owner.acct.fields.iter().any(|(f, _)| f == field) {
            return Err(format!(
                "environment mutates `{}`, which is not a field of the property's account",
                field
            ));
        }
    }
    Ok(*owner_index)
}

// ── Structural emission ─────────────────────────────────────────────

fn emit_product_state_struct(out: &mut String, components: &[ProductComponent]) {
    out.push_str("    struct ProductState {\n");
    for comp in components {
        out.push_str(&format!(
            "        {}: {}::State,\n",
            comp.mod_name, comp.mod_name
        ));
    }
    out.push_str("    }\n\n");
}

fn emit_wrappers(
    out: &mut String,
    parsed: &ParsedSpec,
    wrapper_names: &[&str],
    wrappable: &[WrappedHandler],
) -> Result<()> {
    use crate::codegen_shared::map_type;

    out.push_str("    // Transition wrappers — delegate to the owning account module.\n");
    for name in wrapper_names {
        let w = wrapped(wrappable, name).expect("wrapper_names built from wrappable");
        let mut params = String::new();
        let mut args = String::new();
        for (n, t) in w.op.takes_params.iter().chain(w.op.abstract_binders.iter()) {
            params.push_str(&format!(", {}: {}", n, map_type(t, parsed)?));
            args.push_str(&format!(", {}", n));
        }
        out.push_str(&format!(
            "    fn {}(s: &mut ProductState{}) -> bool {{\n",
            w.op.name, params
        ));
        out.push_str(&format!(
            "        {}::{}(&mut s.{}{})\n",
            w.component_mod, w.op.name, w.component_mod, args
        ));
        out.push_str("    }\n\n");
    }
    Ok(())
}

/// `let mut s = ProductState { <comp>: <comp>::State { … kani::any() … }, … };`
fn emit_product_state_init_symbolic(
    out: &mut String,
    parsed: &ParsedSpec,
    components: &[ProductComponent],
) {
    use crate::rust_codegen_util as util;

    out.push_str("        let mut s = ProductState {\n");
    for comp in components {
        out.push_str(&format!(
            "            {}: {}::State {{\n",
            comp.mod_name, comp.mod_name
        ));
        let fields = comp.state_fields(parsed);
        for (fname, _) in util::field_refs(&fields) {
            out.push_str(&format!("                {}: kani::any(),\n", fname));
        }
        if comp.has_lifecycle() && !fields.iter().any(|(n, _)| n == "status") {
            out.push_str("                status: kani::any(),\n");
        }
        out.push_str("            },\n");
    }
    out.push_str("        };\n");
}

/// `let mut s = <mod>::State { … kani::any() … };` — the single-component
/// symbolic init used by environment harnesses, whose property + mutated
/// fields all resolve to one account.
fn emit_component_state_init_symbolic(
    out: &mut String,
    parsed: &ParsedSpec,
    comp: &ProductComponent,
) {
    use crate::rust_codegen_util as util;

    out.push_str(&format!(
        "        let mut s = {}::State {{\n",
        comp.mod_name
    ));
    let fields = comp.state_fields(parsed);
    for (fname, _) in util::field_refs(&fields) {
        out.push_str(&format!("            {}: kani::any(),\n", fname));
    }
    if comp.has_lifecycle() && !fields.iter().any(|(n, _)| n == "status") {
        out.push_str("            status: kani::any(),\n");
    }
    out.push_str("        };\n");
}

fn emit_harness_attrs(out: &mut String, name: &str, unwind: usize) {
    out.push_str("    #[kani::proof]\n");
    out.push_str(&format!("    #[kani::unwind({})]\n", unwind));
    out.push_str("    #[kani::solver(cadical)]\n");
    out.push_str(&format!("    fn {}() {{\n", name));
}

// ── Covers ──────────────────────────────────────────────────────────

fn emit_covers(
    out: &mut String,
    parsed: &ParsedSpec,
    components: &[ProductComponent],
    plans: &[CoverPlan],
    rec: &mut ObligationRecorder,
) -> Result<()> {
    use crate::codegen_shared::map_type;

    for plan in plans {
        match plan {
            CoverPlan::Unsupported {
                cover_name,
                trace_index,
                why,
            } => {
                rec.unsupported(
                    ObligationKind::Cover,
                    "file",
                    &format!("{}::{}", cover_name, trace_index),
                    UnsupportedReason::KaniMultiAccountFileLevel,
                );
                out.push_str(&format!(
                    "    // cover {} trace {}: not lowered — {}\n\n",
                    cover_name, trace_index, why
                ));
            }
            CoverPlan::Lower {
                cover_name,
                trace_index,
                trace_ops,
                multi_trace,
            } => {
                let suffix = if *multi_trace {
                    format!("_{}", trace_index)
                } else {
                    String::new()
                };
                let harness = format!("cover_{}{}", cover_name, suffix);
                rec.emitted(
                    ObligationKind::Cover,
                    "file",
                    &format!("{}::{}", cover_name, trace_index),
                    &harness,
                );
                emit_harness_attrs(out, &harness, trace_ops.len() + 1);
                emit_product_state_init_symbolic(out, parsed, components);

                let mut indent = "        ".to_string();
                for (j, w) in trace_ops.iter().enumerate() {
                    for (pname, ptype) in
                        w.op.takes_params.iter().chain(w.op.abstract_binders.iter())
                    {
                        out.push_str(&format!(
                            "{}let {}_{}: {} = kani::any();\n",
                            indent,
                            pname,
                            j,
                            map_type(ptype, parsed)?
                        ));
                    }
                    let args: String =
                        w.op.takes_params
                            .iter()
                            .chain(w.op.abstract_binders.iter())
                            .map(|(n, _)| format!(", {}_{}", n, j))
                            .collect();
                    if j < trace_ops.len() - 1 {
                        out.push_str(&format!("{}if {}(&mut s{}) {{\n", indent, w.op.name, args));
                        indent.push_str("    ");
                    } else {
                        out.push_str(&format!(
                            "{}kani::cover!({}(&mut s{}), \"{} trace is reachable\");\n",
                            indent, w.op.name, args, cover_name
                        ));
                    }
                }
                for _ in 0..trace_ops.len().saturating_sub(1) {
                    indent = indent[..indent.len() - 4].to_string();
                    out.push_str(&format!("{}}}\n", indent));
                }
                out.push_str("    }\n\n");
            }
        }
    }
    Ok(())
}

// ── Liveness ────────────────────────────────────────────────────────

fn emit_liveness(
    out: &mut String,
    parsed: &ParsedSpec,
    components: &[ProductComponent],
    plans: &[LivenessPlan],
    rec: &mut ObligationRecorder,
) -> Result<()> {
    use crate::codegen_shared::map_type;

    for plan in plans {
        match plan {
            LivenessPlan::Unsupported { liveness, why } => {
                rec.unsupported(
                    ObligationKind::Liveness,
                    "file",
                    &liveness.name,
                    UnsupportedReason::KaniMultiAccountFileLevel,
                );
                out.push_str(&format!(
                    "    // liveness {}: not lowered — {}\n\n",
                    liveness.name, why
                ));
            }
            LivenessPlan::Lower {
                liveness,
                owner_index,
                via,
            } => {
                let owner = &components[*owner_index];
                let bound = liveness.within_steps.unwrap_or(10) as usize;
                let harness = format!("verify_liveness_{}", liveness.name);
                rec.emitted(ObligationKind::Liveness, "file", &liveness.name, &harness);
                emit_harness_attrs(out, &harness, bound + 1);
                emit_product_state_init_symbolic(out, parsed, components);
                out.push_str(&format!(
                    "        kani::assume(s.{}.status == {}::Status::{});\n",
                    owner.mod_name, owner.mod_name, liveness.from_state
                ));
                out.push_str(&format!("        for _ in 0..{} {{\n", bound));
                out.push_str("            let op: u8 = kani::any();\n");
                out.push_str("            match op {\n");
                for (i, w) in via.iter().enumerate() {
                    out.push_str(&format!("                {} => {{\n", i));
                    for (n, t) in w.op.takes_params.iter().chain(w.op.abstract_binders.iter()) {
                        out.push_str(&format!(
                            "                    let {}: {} = kani::any();\n",
                            n,
                            map_type(t, parsed)?
                        ));
                    }
                    let args: String =
                        w.op.takes_params
                            .iter()
                            .chain(w.op.abstract_binders.iter())
                            .map(|(n, _)| format!(", {}", n))
                            .collect();
                    out.push_str(&format!(
                        "                    {}(&mut s{});\n",
                        w.op.name, args
                    ));
                    out.push_str("                }\n");
                }
                out.push_str("                _ => {}\n");
                out.push_str("            }\n");
                out.push_str("        }\n");
                out.push_str(&format!(
                    "        kani::cover!(s.{}.status == {}::Status::{}, \"{} reaches {} within {} steps\");\n",
                    owner.mod_name,
                    owner.mod_name,
                    liveness.leads_to_state,
                    liveness.name,
                    liveness.leads_to_state,
                    bound
                ));
                out.push_str("    }\n\n");
            }
        }
    }
    Ok(())
}

// ── Environments ────────────────────────────────────────────────────

fn emit_environments(
    out: &mut String,
    mir: &Mir,
    parsed: &ParsedSpec,
    components: &[ProductComponent],
    plans: &[EnvPlan],
    rec: &mut ObligationRecorder,
) -> Result<()> {
    for plan in plans {
        let key = format!("{}::{}", plan.prop.name, plan.env.name);
        match &plan.owner {
            Err(why) => {
                rec.unsupported(
                    ObligationKind::Environment,
                    "file",
                    &key,
                    UnsupportedReason::KaniMultiAccountFileLevel,
                );
                out.push_str(&format!(
                    "    // environment {} × property {}: not lowered — {}\n\n",
                    plan.env.name, plan.prop.name, why
                ));
            }
            Ok(owner_index) => {
                let owner = &components[*owner_index];
                let mir_env = mir
                    .environments
                    .iter()
                    .find(|candidate| candidate.name == plan.env.name);
                let (rust_constraints, needs_pre, needs_post) =
                    super::render_environment_constraints(
                        mir_env,
                        !plan.env.external_fields.is_empty(),
                    );
                let harness = format!("verify_{}_under_{}", plan.prop.name, plan.env.name);
                rec.emitted(ObligationKind::Environment, "file", &key, &harness);
                emit_harness_attrs(out, &harness, 2);
                emit_component_state_init_symbolic(out, parsed, owner);
                out.push_str(&format!(
                    "        kani::assume({}::{}(&s));\n",
                    owner.mod_name, plan.prop.name
                ));
                if needs_pre {
                    out.push_str("        let pre = s.clone();\n");
                }
                for (object, field, field_type) in &plan.env.external_fields {
                    let rust_type = crate::codegen_shared::map_type(field_type, parsed)?;
                    out.push_str(&format!(
                        "        let pre_{}_{}: {} = kani::any();\n",
                        object, field, rust_type
                    ));
                    out.push_str(&format!(
                        "        let post_{}_{}: {} = kani::any();\n",
                        object, field, rust_type
                    ));
                }
                for (field, _ftype) in &plan.env.mutates {
                    out.push_str(&format!("        s.{} = kani::any();\n", field));
                }
                if needs_post {
                    out.push_str("        let post = &s;\n");
                }
                for constraint in &rust_constraints {
                    out.push_str(&format!("        kani::assume({});\n", constraint));
                }
                out.push_str(&format!(
                    "        assert!({}::{}(&s),\n",
                    owner.mod_name, plan.prop.name
                ));
                out.push_str(&format!(
                    "            \"{} must hold after {}\");\n",
                    plan.prop.name, plan.env.name
                ));
                out.push_str("    }\n\n");
            }
        }
    }
    Ok(())
}

/// Word-bounded `s.<name>` reference check (mirrors the proptest-side
/// helper): `s.total` must not match `s.total_supply`.
fn references_state_field(rust: &str, name: &str) -> bool {
    let needle = format!("s.{}", name);
    let mut start = 0;
    while let Some(pos) = rust[start..].find(&needle) {
        let end = start + pos + needle.len();
        let boundary = rust[end..]
            .chars()
            .next()
            .map(|c| !c.is_alphanumeric() && c != '_')
            .unwrap_or(true);
        if boundary {
            return true;
        }
        start = end;
    }
    false
}
