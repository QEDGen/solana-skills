//! Generate Lean 4 source from a `ParsedSpec`.
//!
//! Replaces the Lean elaborator as the source of truth when using `.qedspec` files.
//! Produces the same structures: State, Status, transitions, Operation inductive,
//! applyOp, CPI theorems, property predicates, and inductive preservation theorems.

use anyhow::Result;
use std::path::Path;

use crate::check::ParsedSpec;

/// Emit a Lean `inductive Foo where | A | B …` block for a lifecycle.
/// Same shape used by single-account (Status) and multi-account
/// (PoolStatus, EscrowStatus, …) renderers.
fn emit_status_inductive(out: &mut String, name: &str, lifecycle: &[String]) {
    out.push_str(&format!("inductive {} where\n", name));
    for s in lifecycle {
        out.push_str(&format!("  | {}\n", s));
    }
    out.push_str("  deriving Repr, DecidableEq, BEq\n\n");
}

/// Emit a Lean `structure Foo where field : Type …` block for a state.
/// Pass `status_name` when the state carries a lifecycle field.
fn emit_state_struct(
    out: &mut String,
    name: &str,
    fields: &[(String, String)],
    status_name: Option<&str>,
) {
    out.push_str(&format!("structure {} where\n", name));
    for (fname, ftype) in fields {
        out.push_str(&format!("  {} : {}\n", safe_name(fname), map_type(ftype)));
    }
    if let Some(sn) = status_name {
        out.push_str(&format!("  status : {}\n", sn));
    }
    out.push_str("  deriving Repr, DecidableEq, BEq\n\n");
}

/// Build a Lean type name from an account name, avoiding double-suffix.
/// "Pool" → "PoolState", "Pool" → "PoolStatus"
/// "State" → "State" (not "StateState"), "State" → "Status" (not "StateStatus")
fn lean_state_name(acct: &str) -> String {
    if acct == "State" {
        "State".to_string()
    } else {
        format!("{}State", acct)
    }
}

fn lean_status_name(acct: &str) -> String {
    if acct == "State" {
        "Status".to_string()
    } else {
        format!("{}Status", acct)
    }
}

/// Generate a Lean file from a `ParsedSpec` and write it to `output_path`.
pub fn generate(spec: &ParsedSpec, output_path: &Path) -> Result<()> {
    let content = render(spec);
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, &content)?;
    eprintln!("  wrote {}", output_path.display());
    Ok(())
}

/// Render a `ParsedSpec` into a complete Lean 4 source string.
pub fn render(spec: &ParsedSpec) -> String {
    // sBPF mode: inferred from `pragma sbpf { ... }` presence (or the
    // legacy fallback signal — see ParsedSpec::is_assembly_target).
    if spec.is_assembly_target() {
        return render_sbpf(spec);
    }

    // New DSL mode: spec declares record types or uses Map[N] T fields.
    // Routes to an indexed-state renderer that emits Fin-backed Maps and
    // Mathlib sum/forall properties with sorry-stubbed preservation proofs.
    if is_indexed_spec(spec) {
        return render_indexed_state(spec);
    }

    let is_multi_account = spec.account_types.len() > 1;

    if is_multi_account {
        render_multi_account(spec)
    } else {
        render_single_account(spec)
    }
}

/// Detect whether `spec` uses the new DSL (records or Map-typed fields).
fn is_indexed_spec(spec: &ParsedSpec) -> bool {
    if !spec.records.is_empty() {
        return true;
    }
    spec.account_types.iter().any(|a| {
        a.fields
            .iter()
            .any(|(_, t)| t.trim_start().starts_with("Map"))
    })
}

/// Single-account rendering — original path, backward-compatible output.
fn render_single_account(spec: &ParsedSpec) -> String {
    let mut out = String::new();

    // Header
    out.push_str("import QEDGen.Solana.Account\n");
    out.push_str("import QEDGen.Solana.Cpi\n");
    out.push_str("import QEDGen.Solana.State\n");
    out.push_str("import QEDGen.Solana.Valid\n\n");

    let name = &spec.program_name;

    out.push_str(&format!("namespace {}\n\n", name));
    out.push_str("open QEDGen.Solana\n\n");

    // Status inductive (if lifecycle states exist)
    let has_lifecycle = !spec.lifecycle_states.is_empty();
    if has_lifecycle {
        emit_status_inductive(&mut out, "Status", &spec.lifecycle_states);
    }

    // State structure
    emit_state_struct(
        &mut out,
        "State",
        &spec.state_fields,
        if has_lifecycle { Some("Status") } else { None },
    );

    // Transition functions
    let ops_refs: Vec<&crate::check::ParsedHandler> = spec.handlers.iter().collect();
    render_transitions(
        &mut out,
        spec,
        &ops_refs,
        &spec.state_fields,
        "State",
        "Status",
    );

    // CPI theorems
    render_cpi_theorems(&mut out, &ops_refs);

    // Invariants
    for (inv_name, _desc) in &spec.invariants {
        out.push_str(&format!(
            "/-- Invariant: {}. -/\ntheorem {} : True := trivial\n\n",
            inv_name, inv_name
        ));
    }

    // Operation inductive + applyOp
    render_operation_inductive(&mut out, &ops_refs, "State");

    // Property predicates and inductive theorems
    render_properties(
        &mut out,
        &spec.properties,
        &ops_refs,
        &spec.state_fields,
        "State",
    );

    // Abort theorems (aborts_if clauses)
    render_aborts_if(
        &mut out,
        &ops_refs,
        &spec.state_fields,
        &spec.state_fields,
        "State",
    );

    // Post-condition theorems (ensures clauses)
    render_ensures(&mut out, &ops_refs, "State");

    // Frame condition theorems (modifies clauses)
    render_frame_conditions(&mut out, &ops_refs, &spec.state_fields, "State");

    // Cover properties (reachability)
    render_covers(&mut out, spec, "State");

    // Liveness properties (leads-to)
    render_liveness(&mut out, spec, "State");

    // Environment blocks (external state)
    render_environments(&mut out, spec, "State");

    // Overflow obligations for operations with add effects
    render_overflow_obligations(&mut out, spec, &ops_refs, &spec.state_fields, "State");

    out.push_str(&format!("end {}\n", name));
    out
}

/// Multi-account rendering — per-account sections with scoped types.
fn render_multi_account(spec: &ParsedSpec) -> String {
    let mut out = String::new();

    // Header
    out.push_str("import QEDGen.Solana.Account\n");
    out.push_str("import QEDGen.Solana.Cpi\n");
    out.push_str("import QEDGen.Solana.State\n");
    out.push_str("import QEDGen.Solana.Valid\n\n");

    let name = &spec.program_name;

    out.push_str(&format!("namespace {}\n\n", name));
    out.push_str("open QEDGen.Solana\n\n");

    // Per-account sections
    for acct in &spec.account_types {
        let acct_name = &acct.name;
        let status_name = lean_status_name(acct_name);
        let state_name = lean_state_name(acct_name);

        // Status inductive
        let has_lifecycle = !acct.lifecycle.is_empty();
        if has_lifecycle {
            emit_status_inductive(&mut out, &status_name, &acct.lifecycle);
        }

        // State structure
        emit_state_struct(
            &mut out,
            &state_name,
            &acct.fields,
            if has_lifecycle {
                Some(&status_name)
            } else {
                None
            },
        );

        // Operations targeting this account
        let ops: Vec<&crate::check::ParsedHandler> = spec
            .handlers
            .iter()
            .filter(|op| {
                op.on_account.as_deref() == Some(acct_name.as_str())
                    || (op.on_account.is_none() && acct_name == &spec.account_types[0].name)
            })
            .collect();

        if ops.is_empty() {
            continue;
        }

        // Transition functions
        render_transitions(
            &mut out,
            spec,
            &ops,
            &acct.fields,
            &state_name,
            &status_name,
        );

        // CPI theorems
        render_cpi_theorems(&mut out, &ops);

        // Operation inductive + applyOp per account
        render_operation_inductive(&mut out, &ops, &state_name);
    }

    // Invariants
    for (inv_name, _desc) in &spec.invariants {
        out.push_str(&format!(
            "/-- Invariant: {}. -/\ntheorem {} : True := trivial\n\n",
            inv_name, inv_name
        ));
    }

    // Properties — for multi-account, reference the state type from the first account
    // that has matching fields. Properties using `state.X` bind to the account whose
    // fields contain X.
    render_properties_multi(&mut out, spec);

    // v2.0 features: aborts_if, covers, liveness, environments, overflow
    // Per-account: aborts_if and overflow need the ops for each account
    for acct in &spec.account_types {
        let state_name = lean_state_name(&acct.name);
        let ops: Vec<&crate::check::ParsedHandler> = spec
            .handlers
            .iter()
            .filter(|op| {
                op.on_account.as_deref() == Some(acct.name.as_str())
                    || (op.on_account.is_none() && acct.name == spec.account_types[0].name)
            })
            .collect();
        if ops.is_empty() {
            continue;
        }
        render_aborts_if(
            &mut out,
            &ops,
            &acct.fields,
            &spec.state_fields,
            &state_name,
        );
        render_ensures(&mut out, &ops, &state_name);
        render_frame_conditions(&mut out, &ops, &acct.fields, &state_name);
        render_overflow_obligations(&mut out, spec, &ops, &acct.fields, &state_name);
    }

    // Spec-level: covers, liveness, environments use the first account's state type
    let primary_state = if spec.account_types.is_empty() {
        "State".to_string()
    } else {
        format!("{}State", spec.account_types[0].name)
    };
    render_covers(&mut out, spec, &primary_state);
    render_liveness(&mut out, spec, &primary_state);
    render_environments(&mut out, spec, &primary_state);

    out.push_str(&format!("end {}\n", name));
    out
}

/// Render transition functions for a set of handlers.
/// Build the guard condition parts for a handler's transition function.
///
/// Returns the list of conjuncts that form the `if` condition. Each entry is a
/// single proposition string; entries may contain internal `∧` (e.g., from a
/// compound `requires` expression). The caller joins them with ` ∧ `.
fn build_guard_cond_parts(
    op: &crate::check::ParsedHandler,
    fields: &[(String, String)],
    fallback_fields: &[(String, String)],
) -> Vec<String> {
    let mut cond_parts: Vec<String> = Vec::new();
    if let Some(ref who) = op.who {
        cond_parts.push(format!("signer = s.{}", safe_name(who)));
    }
    if let Some(ref pre) = op.pre_status {
        cond_parts.push(format!("s.status = .{}", pre));
    }
    // Auto-guards for sub effects (underflow prevention)
    for (field, op_kind, _value) in &op.effects {
        if op_kind == "sub" {
            let ftype = fields
                .iter()
                .find(|(n, _)| n == field)
                .or_else(|| fallback_fields.iter().find(|(n, _)| n == field))
                .map(|(_, t)| t.as_str())
                .unwrap_or("");
            if map_type(ftype) != "Int" {
                let val = &op
                    .effects
                    .iter()
                    .find(|(f, o, _)| f == field && o == "sub")
                    .unwrap()
                    .2;
                cond_parts.push(format!("{} \u{2264} s.{}", val, safe_name(field)));
            }
        }
    }
    if let Some(ref guard) = op.guard_str {
        cond_parts.push(guard.clone());
    }
    // Requires clauses contribute their positive form as guard conditions
    for req in &op.requires {
        cond_parts.push(req.lean_expr.clone());
    }
    // Auto-guards for add effects (overflow prevention, type-aware).
    for (field, op_kind, value) in &op.effects {
        if op_kind == "add" {
            let ftype = fields
                .iter()
                .find(|(n, _)| n == field)
                .or_else(|| fallback_fields.iter().find(|(n, _)| n == field))
                .map(|(_, t)| t.as_str())
                .unwrap_or("");
            if let Some(max_const) = type_max_const(ftype) {
                let sf = safe_name(field);
                let already_guarded = cond_parts.iter().any(|c| {
                    c.contains(&format!("s.{} + {}", sf, value))
                        || c.contains(&format!("{} + s.{}", value, sf))
                });
                if !already_guarded {
                    cond_parts.push(format!("s.{} + {} \u{2264} {}", sf, value, max_const));
                }
            }
        }
    }
    cond_parts
}

/// Wrap `expr` in parens iff it contains a top-level binary operator of
/// lower precedence than `∧` — namely `∨`, `→`, or `↔`. Used before
/// `∧`-joining a list of conjunct atoms so one atom's disjunction can't
/// extend past its boundary at Lean parse time. Without this, a cond_part
/// like `side = 0 ∨ side = 1` joined into `A ∧ B ∧ side = 0 ∨ side = 1`
/// parses as `((A ∧ B) ∧ side = 0) ∨ side = 1`.
///
/// Depth-aware: an already-parenthesized `∨` (`(A ∨ B)`) doesn't trigger
/// a second wrap. Atoms containing only `∧` / `=` / `≤` etc. (higher or
/// equal precedence than `∧`) pass through unchanged, so existing
/// projection paths via `count_top_level_conjuncts` stay valid.
fn paren_if_low_prec(expr: &str) -> String {
    let mut depth: i32 = 0;
    for ch in expr.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            // ∨ (U+2228), → (U+2192), ↔ (U+2194)
            '\u{2228}' | '\u{2192}' | '\u{2194}' if depth == 0 => {
                return format!("({})", expr);
            }
            _ => {}
        }
    }
    expr.to_string()
}

/// Count the number of top-level `∧` conjuncts in a Lean expression.
///
/// Respects parenthesis nesting: `(a ∧ b) ∧ c` has 2 top-level conjuncts,
/// not 3. Used for computing projection paths into right-associative `∧` chains.
fn count_top_level_conjuncts(expr: &str) -> usize {
    let mut depth: i32 = 0;
    let mut count = 0;
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

/// Generate a projection path into a right-associative `∧` chain.
///
/// For `A ∧ (B ∧ (C ∧ (D ∧ E)))` with 5 total atoms:
/// - Index 0 → `hg.1`
/// - Index 1 → `hg.2.1`
/// - Index 3 → `hg.2.2.2.1`
/// - Index 4 → `hg.2.2.2.2` (last element: no trailing `.1`)
fn conjunction_projection(flat_index: usize, total_atoms: usize) -> String {
    let mut path = "hg".to_string();
    for _ in 0..flat_index {
        path.push_str(".2");
    }
    if flat_index < total_atoms - 1 {
        path.push_str(".1");
    }
    path
}

/// Generate proof script for a requires-based abort theorem.
///
/// The `requires` expression appears as a conjunct (possibly compound) in the
/// guard. The abort hypothesis `h : ¬(expr)` contradicts the extracted guard
/// conjuncts, so the proof uses `if_neg` with a projection lambda.
fn abort_requires_proof(
    trans_name: &str,
    cond_parts: &[String],
    req_index_in_cond_parts: usize,
) -> String {
    // Count atoms per cond_part and compute totals
    let atoms_per: Vec<usize> = cond_parts
        .iter()
        .map(|p| count_top_level_conjuncts(p))
        .collect();
    let total_atoms: usize = atoms_per.iter().sum();
    let flat_start: usize = atoms_per[..req_index_in_cond_parts].iter().sum();
    let target_atoms = atoms_per[req_index_in_cond_parts];

    // Special case: requires is the entire guard (single part)
    if total_atoms == 1 {
        return format!(" := by\n  unfold {}\n  rw [if_neg h]\n", trans_name);
    }

    // Build projections for each atom in this requires expression
    let projections: Vec<String> = (0..target_atoms)
        .map(|i| conjunction_projection(flat_start + i, total_atoms))
        .collect();

    let extraction = if projections.len() == 1 {
        projections[0].clone()
    } else {
        format!("\u{27E8}{}\u{27E9}", projections.join(", ")) // ⟨...⟩
    };

    format!(
        " := by\n  unfold {}\n  rw [if_neg (fun hg => h {})]\n",
        trans_name, extraction
    )
}

fn render_transitions(
    out: &mut String,
    spec: &ParsedSpec,
    ops: &[&crate::check::ParsedHandler],
    fields: &[(String, String)],
    state_type: &str,
    _status_type: &str,
) {
    for op in ops {
        let trans_name = safe_name(&format!("{}Transition", op.name));
        let param_sig = param_sig_str(&op.takes_params);

        let cond_parts = build_guard_cond_parts(op, fields, &spec.state_fields);

        let has_cond = !cond_parts.is_empty();
        let if_cond = cond_parts
            .iter()
            .map(|p| paren_if_low_prec(p))
            .collect::<Vec<_>>()
            .join(" \u{2227} "); // ∧

        // Build state update
        let mut with_parts: Vec<String> = Vec::new();
        for (field, op_kind, value) in &op.effects {
            let sf = safe_name(field);
            match op_kind.as_str() {
                "add" => with_parts.push(format!("{} := s.{} + {}", sf, sf, value)),
                "sub" => with_parts.push(format!("{} := s.{} - {}", sf, sf, value)),
                "set" => with_parts.push(format!("{} := {}", sf, value)),
                _ => {}
            }
        }
        if let Some(ref post) = op.post_status {
            with_parts.push(format!("status := .{}", post));
        }

        let then_body = if with_parts.is_empty() {
            "some s".to_string()
        } else {
            format!("some {{ s with {} }}", with_parts.join(", "))
        };

        out.push_str(&format!(
            "def {} (s : {}) (signer : Pubkey){} : Option {} :=\n",
            trans_name, state_type, param_sig, state_type
        ));

        // Emit let bindings before the if condition
        for (binding_name, lean_expr, _rust_expr) in &op.let_bindings {
            out.push_str(&format!(
                "  let {} := {}\n",
                safe_name(binding_name),
                lean_expr
            ));
        }

        if has_cond {
            out.push_str(&format!("  if {} then\n", if_cond));
            out.push_str(&format!("    {}\n", then_body));
            out.push_str("  else none\n\n");
        } else {
            out.push_str(&format!("  {}\n\n", then_body));
        }
    }
}

/// Render transfer correctness theorems from handler transfers blocks.
fn render_cpi_theorems(out: &mut String, ops: &[&crate::check::ParsedHandler]) {
    for op in ops {
        if !op.has_calls() {
            continue;
        }

        for (i, transfer) in op.transfers.iter().enumerate() {
            let suffix = if op.transfers.len() > 1 {
                format!("_{}", i)
            } else {
                String::new()
            };
            let theorem_name = safe_name(&format!("{}_transfer{}_correct", op.name, suffix));

            out.push_str(&format!(
                "/-- {} transfer: {} → {}",
                op.name, transfer.from, transfer.to
            ));
            if let Some(ref amt) = transfer.amount {
                out.push_str(&format!(" amount {}", amt));
            }
            if let Some(ref auth) = transfer.authority {
                out.push_str(&format!(" authority {}", auth));
            }
            out.push_str(". -/\n");
            out.push_str(&format!("theorem {} : True := trivial\n\n", theorem_name));
        }
    }
}

/// Render Operation inductive and applyOp dispatcher.
fn render_operation_inductive(
    out: &mut String,
    ops: &[&crate::check::ParsedHandler],
    state_type: &str,
) {
    if ops.is_empty() {
        return;
    }

    // For multi-account, prefix with account name to avoid name collisions
    let prefix = if state_type != "State" {
        // e.g., "PoolState" -> "Pool"
        state_type.strip_suffix("State").unwrap_or(state_type)
    } else {
        ""
    };
    let op_type = if prefix.is_empty() {
        "Operation".to_string()
    } else {
        format!("{}Operation", prefix)
    };
    let apply_name = if prefix.is_empty() {
        "applyOp".to_string()
    } else {
        format!("apply{}Op", prefix)
    };

    out.push_str(&format!("inductive {} where\n", op_type));
    for op in ops {
        let ctor = safe_name(&op.name);
        if op.takes_params.is_empty() {
            out.push_str(&format!("  | {}\n", ctor));
        } else {
            let params: Vec<String> = op
                .takes_params
                .iter()
                .map(|(pn, pt)| format!("({} : {})", pn, map_type(pt)))
                .collect();
            out.push_str(&format!("  | {} {}\n", ctor, params.join(" ")));
        }
    }
    out.push_str("  deriving Repr, DecidableEq, BEq\n\n");

    // applyOp dispatcher
    out.push_str(&format!(
        "def {} (s : {}) (signer : Pubkey) : {} \u{2192} Option {}\n",
        apply_name, state_type, op_type, state_type
    ));
    for op in ops {
        let ctor = safe_name(&op.name);
        let trans = safe_name(&format!("{}Transition", op.name));
        let param_names: Vec<String> = op.takes_params.iter().map(|(n, _)| n.clone()).collect();
        let param_args = if param_names.is_empty() {
            String::new()
        } else {
            format!(" {}", param_names.join(" "))
        };
        let call_args = if param_names.is_empty() {
            String::new()
        } else {
            format!(" {}", param_names.join(" "))
        };
        out.push_str(&format!(
            "  | .{}{} => {} s signer{}\n",
            ctor, param_args, trans, call_args
        ));
    }
    out.push('\n');
}

/// Render properties for single-account specs.
fn render_properties(
    out: &mut String,
    properties: &[crate::check::ParsedProperty],
    ops: &[&crate::check::ParsedHandler],
    fields: &[(String, String)],
    state_type: &str,
) {
    render_properties_inner(
        out,
        properties,
        ops,
        fields,
        state_type,
        "Operation",
        "applyOp",
    );
}

/// Render properties for multi-account specs.
fn render_properties_multi(out: &mut String, spec: &ParsedSpec) {
    // Group properties by target account, then delegate to render_properties_inner.
    // Heuristic: look at the expression's `s.field` references and match against account fields.

    // Collect properties by target account
    let mut groups: std::collections::HashMap<String, Vec<&crate::check::ParsedProperty>> =
        std::collections::HashMap::new();
    let mut acct_for_prop: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for prop in &spec.properties {
        let target_name = if let Some(ref expr) = prop.expression {
            spec.account_types
                .iter()
                .find(|a| {
                    a.fields
                        .iter()
                        .any(|(f, _)| expr.contains(&format!("s.{}", f)))
                })
                .map(|a| a.name.clone())
                .unwrap_or_else(|| spec.account_types[0].name.clone())
        } else {
            spec.account_types[0].name.clone()
        };
        acct_for_prop.insert(prop.name.clone(), target_name.clone());
        groups.entry(target_name).or_default().push(prop);
    }

    for (acct_name, props) in &groups {
        let state_type = lean_state_name(acct_name);
        let op_type = format!("{}Operation", acct_name);
        let apply_name = format!("apply{}Op", acct_name);

        let acct_ops: Vec<&crate::check::ParsedHandler> = spec
            .handlers
            .iter()
            .filter(|op| {
                op.on_account.as_deref() == Some(acct_name.as_str())
                    || (op.on_account.is_none() && acct_name == &spec.account_types[0].name)
            })
            .collect();

        // Convert &[&ParsedProperty] to &[ParsedProperty] by cloning
        let owned_props: Vec<crate::check::ParsedProperty> = props
            .iter()
            .map(|p| crate::check::ParsedProperty {
                name: p.name.clone(),
                expression: p.expression.clone(),
                rust_expression: p.rust_expression.clone(),
                preserved_by: p.preserved_by.clone(),
            })
            .collect();

        // Resolve fields for this account
        let acct_fields: Vec<(String, String)> = spec
            .account_types
            .iter()
            .find(|a| a.name == *acct_name)
            .map(|a| a.fields.clone())
            .unwrap_or_default();

        render_properties_inner(
            out,
            &owned_props,
            &acct_ops,
            &acct_fields,
            &state_type,
            &op_type,
            &apply_name,
        );
    }
}

/// Check whether a handler's transition function has an `if` guard.
///
/// Mirrors the condition-building logic in `render_transitions` — if any
/// condition source is present, the transition has an `if ... then ... else none`.
fn handler_has_condition(op: &crate::check::ParsedHandler, fields: &[(String, String)]) -> bool {
    if op.who.is_some()
        || op.pre_status.is_some()
        || op.guard_str.is_some()
        || !op.requires.is_empty()
    {
        return true;
    }
    for (field, op_kind, _) in &op.effects {
        if op_kind == "sub" {
            let ftype = fields
                .iter()
                .find(|(n, _)| n == field)
                .map(|(_, t)| t.as_str())
                .unwrap_or("");
            if map_type(ftype) != "Int" {
                return true;
            }
        }
        if op_kind == "add" {
            let ftype = fields
                .iter()
                .find(|(n, _)| n == field)
                .map(|(_, t)| t.as_str())
                .unwrap_or("");
            if type_max_const(ftype).is_some() {
                return true;
            }
        }
    }
    false
}

/// Generate a mechanical proof script for a preservation sub-lemma.
///
/// The proof strategy depends on whether the handler modifies fields
/// referenced in the property expression:
///
/// - **No overlap**: After `cases h`, the property on `s'` is definitionally
///   equal to the property on `s`, so `exact h_inv` works.
///
/// - **Field overlap**: Need to unfold the property in both hypothesis and
///   goal, reduce struct field access with `dsimp`, and discharge with `omega`
///   (which can destructure the guard conjunction for needed arithmetic facts).
fn preservation_proof_script(
    op: &crate::check::ParsedHandler,
    prop: &crate::check::ParsedProperty,
    fields: &[(String, String)],
) -> String {
    let trans_name = safe_name(&format!("{}Transition", op.name));
    let has_cond = handler_has_condition(op, fields);

    // Determine which property fields this handler touches
    let prop_fields: Vec<&str> = if let Some(ref expr) = prop.expression {
        fields_referenced_in_expr(expr)
    } else {
        Vec::new()
    };
    let touches_prop_field = op
        .effects
        .iter()
        .any(|(f, _, _)| prop_fields.contains(&f.as_str()))
        || (op.post_status.is_some() && prop_fields.contains(&"status"));

    if has_cond {
        if touches_prop_field {
            // Handler modifies property fields — need omega with guard facts
            format!(
                " := by\n  unfold {} at h; split at h\n  \
                 · next hg => cases h; unfold {} at h_inv ⊢; dsimp; omega\n  \
                 · contradiction\n",
                trans_name, prop.name
            )
        } else {
            // Handler doesn't modify property fields — trivially preserved
            format!(
                " := by\n  unfold {} at h; split at h\n  \
                 · cases h; exact h_inv\n  \
                 · contradiction\n",
                trans_name
            )
        }
    } else {
        // Unconditional handler (no if guard)
        if touches_prop_field {
            format!(
                " := by\n  unfold {} at h; cases h; \
                 unfold {} at h_inv ⊢; dsimp; omega\n",
                trans_name, prop.name
            )
        } else {
            format!(
                " := by\n  unfold {} at h; cases h; exact h_inv\n",
                trans_name
            )
        }
    }
}

/// Inner helper for property rendering.
///
/// Emits per-operation sub-lemmas with auto-generated proof scripts and a
/// master theorem that is auto-proven by case split over the Operation type.
fn render_properties_inner(
    out: &mut String,
    properties: &[crate::check::ParsedProperty],
    ops: &[&crate::check::ParsedHandler],
    fields: &[(String, String)],
    state_type: &str,
    op_type: &str,
    apply_name: &str,
) {
    for prop in properties {
        if let Some(ref expr) = prop.expression {
            // Strip leading ∀/forall quantifier if present, since the def already binds `s`
            // e.g., "∀ s : Pool.Active, s.total_deposits ≥ s.total_borrows"
            //     → "s.total_deposits ≥ s.total_borrows"
            let body = if let Some(rest) = expr
                .strip_prefix('\u{2200}')
                .or_else(|| expr.strip_prefix("forall"))
            {
                // Skip past "var : Type, " to get the body
                if let Some(comma_pos) = rest.find(',') {
                    rest[comma_pos + 1..].trim().to_string()
                } else {
                    expr.clone()
                }
            } else {
                expr.clone()
            };
            out.push_str(&format!(
                "def {} (s : {}) : Prop := {}\n\n",
                prop.name, state_type, body
            ));
        }

        // Determine which operations this property covers
        let covered_ops: Vec<&&crate::check::ParsedHandler> = ops
            .iter()
            .filter(|op| prop.preserved_by.contains(&op.name))
            .collect();

        // Emit per-operation sub-lemmas with auto-generated proofs
        for op in &covered_ops {
            let trans_name = safe_name(&format!("{}Transition", op.name));
            let param_sig = param_sig_str(&op.takes_params);

            let sub_lemma_name = safe_name(&format!("{}_preserved_by_{}", prop.name, op.name));
            out.push_str(&format!(
                "theorem {} (s s' : {}) (signer : Pubkey){}\n",
                sub_lemma_name, state_type, param_sig
            ));
            out.push_str(&format!(
                "    (h_inv : {} s) (h : {} s signer{} = some s') :\n",
                prop.name,
                trans_name,
                param_args_str(&op.takes_params)
            ));
            let proof = preservation_proof_script(op, prop, fields);
            out.push_str(&format!("    {} s'{}\n", prop.name, proof));
        }

        // Emit master theorem auto-proven by case split
        out.push_str(&format!(
            "/-- {} is preserved by every operation. Auto-proven by case split. -/\n",
            prop.name
        ));
        out.push_str(&format!(
            "theorem {}_inductive (s s' : {}) (signer : Pubkey) (op : {})\n    (h_inv : {} s) (h : {} s signer op = some s') : {} s' := by\n",
            prop.name, state_type, op_type, prop.name, apply_name, prop.name
        ));
        out.push_str("  cases op with\n");
        for op in ops {
            let ctor = safe_name(&op.name);
            let param_names: Vec<String> = op.takes_params.iter().map(|(n, _)| n.clone()).collect();
            let param_bind = if param_names.is_empty() {
                String::new()
            } else {
                format!(" {}", param_names.join(" "))
            };

            if prop.preserved_by.contains(&op.name) {
                let ref_name = safe_name(&format!("{}_preserved_by_{}", prop.name, op.name));
                out.push_str(&format!(
                    "  | {}{} => exact {} s s' signer{} h_inv h\n",
                    ctor, param_bind, ref_name, param_bind
                ));
            } else {
                // Operation not in preserved_by — attempt inline proof if trivial.
                // Collect field names referenced in the property expression.
                let prop_fields: Vec<&str> = if let Some(ref expr) = prop.expression {
                    fields_referenced_in_expr(expr)
                } else {
                    Vec::new()
                };
                // Check if the operation touches any field the property references.
                let touches_prop_field = op
                    .effects
                    .iter()
                    .any(|(f, _, _)| prop_fields.contains(&f.as_str()));
                let trans_name = safe_name(&format!("{}Transition", op.name));
                if !touches_prop_field {
                    // Operation doesn't modify any field in the property → trivially preserved.
                    out.push_str(&format!(
                        "  | {}{} =>\n    simp [applyOp, {}] at h\n    obtain \u{27E8}_, h_eq\u{27E9} := h\n    subst h_eq; exact h_inv\n",
                        ctor, param_bind, trans_name
                    ));
                } else {
                    // Operation modifies property fields but isn't in preserved_by.
                    // Still attempt auto-proof: omega can often derive the property
                    // from guard conditions (e.g., sub-effects preserve upper bounds).
                    // Must first `simp [applyOp]` to unfold the dispatch, then
                    // `unfold transition` to expose the if guard.
                    let has_cond = handler_has_condition(op, fields);
                    if has_cond {
                        out.push_str(&format!(
                            "  | {}{} =>\n    simp [applyOp] at h\n    unfold {} at h; split at h\n    \u{B7} next hg => cases h; unfold {} at h_inv \u{22A2}; dsimp; omega\n    \u{B7} contradiction\n",
                            ctor, param_bind, trans_name, prop.name
                        ));
                    } else {
                        out.push_str(&format!(
                            "  | {}{} =>\n    simp [applyOp] at h\n    unfold {} at h; cases h; unfold {} at h_inv \u{22A2}; dsimp; omega\n",
                            ctor, param_bind, trans_name, prop.name
                        ));
                    }
                }
            }
        }
        out.push('\n');
    }
}

/// Build " param1 param2" string for calling a transition function.
fn param_args_str(params: &[(String, String)]) -> String {
    if params.is_empty() {
        String::new()
    } else {
        format!(
            " {}",
            params
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        )
    }
}

/// Symbolic state tracker for cover trace witness construction.
///
/// Tracks concrete field values for each state field, the lifecycle status,
/// and chosen parameter values at each step. This lets us compute intermediate
/// states and emit `by decide` proofs.
struct WitnessState {
    /// Field values: (name, concrete_value_as_string).
    /// Pubkey fields map to "pk", Nat fields to their numeric value.
    fields: Vec<(String, String)>,
    /// Current lifecycle status (e.g., "Uninitialized", "Active").
    status: Option<String>,
}

impl WitnessState {
    /// Initialize from spec fields and lifecycle.
    fn new(fields: &[(String, String)], lifecycle: &[String]) -> Self {
        let field_vals: Vec<(String, String)> = fields
            .iter()
            .map(|(name, typ)| {
                let val = match map_type(typ) {
                    "Pubkey" => "pk".to_string(),
                    _ => "0".to_string(),
                };
                (name.clone(), val)
            })
            .collect();
        let status = lifecycle.first().cloned();
        WitnessState {
            fields: field_vals,
            status,
        }
    }

    /// Render as a Lean struct literal: `⟨pk, pk, 0, 0, pk, .Uninitialized⟩`
    fn to_lean(&self) -> String {
        let mut parts: Vec<String> = self.fields.iter().map(|(_, v)| v.clone()).collect();
        if let Some(ref s) = self.status {
            parts.push(format!(".{}", s));
        }
        format!("⟨{}⟩", parts.join(", "))
    }

    /// Apply a handler's effects, updating field values.
    /// `param_values` maps parameter names to chosen concrete values.
    fn apply(&mut self, handler: &crate::check::ParsedHandler, param_values: &[(String, String)]) {
        // Apply effects
        for (field, op_kind, value) in &handler.effects {
            let resolved = self.resolve_value(value, param_values);
            match op_kind.as_str() {
                "set" => {
                    if let Some(f) = self.fields.iter_mut().find(|(n, _)| n == field) {
                        f.1 = resolved;
                    }
                }
                "add" => {
                    if let Some(f) = self.fields.iter_mut().find(|(n, _)| n == field) {
                        let cur: u128 = f.1.parse().unwrap_or(0);
                        let add: u128 = resolved.parse().unwrap_or(0);
                        f.1 = (cur + add).to_string();
                    }
                }
                "sub" => {
                    if let Some(f) = self.fields.iter_mut().find(|(n, _)| n == field) {
                        let cur: u128 = f.1.parse().unwrap_or(0);
                        let sub: u128 = resolved.parse().unwrap_or(0);
                        f.1 = cur.saturating_sub(sub).to_string();
                    }
                }
                _ => {}
            }
        }
        // Apply lifecycle transition
        if let Some(ref post) = handler.post_status {
            self.status = Some(post.clone());
        }
    }

    /// Resolve an effect value to a concrete string.
    /// Checks param_values first, then tries parsing as integer.
    /// Falls back to "1" for unknown references.
    fn resolve_value(&self, value: &str, param_values: &[(String, String)]) -> String {
        // Check if it's a parameter
        if let Some((_, v)) = param_values.iter().find(|(n, _)| n == value) {
            return v.clone();
        }
        // Check if it's already a number
        if value.parse::<u128>().is_ok() {
            return value.to_string();
        }
        // Check if it's a state field reference (e.g., "s.field" patterns are unlikely
        // in effect values, but handle self-references)
        if let Some(f) = self.fields.iter().find(|(n, _)| n == value) {
            return f.1.clone();
        }
        // Fallback
        "1".to_string()
    }
}

/// Choose good witness values for handler parameters.
///
/// Heuristics:
/// - Default: choose 1 for numeric params (satisfies common `> 0` and `≤ N` guards)
/// - Parameters appearing only in `param < state.field` patterns (index-like): choose 0
/// - Pubkey params: choose pk
fn choose_param_values(handler: &crate::check::ParsedHandler) -> Vec<(String, String)> {
    // Collect all guard/requires expressions to check for patterns
    let mut all_exprs: Vec<&str> = Vec::new();
    if let Some(ref g) = handler.guard_str {
        all_exprs.push(g);
    }
    for req in &handler.requires {
        all_exprs.push(&req.lean_expr);
    }
    let combined = all_exprs.join(" ");

    handler
        .takes_params
        .iter()
        .map(|(name, typ)| {
            let val = match map_type(typ) {
                "Pubkey" => "pk".to_string(),
                _ => {
                    // Check if this is an index-like param: only appears in `param < state.X`
                    // and never in `> 0` or as a bound
                    let is_index_like = combined.contains(&format!("{} < s.", name))
                        && !combined.contains(&format!("{} > 0", name))
                        && !combined.contains(&format!("{} \u{2265}", name)) // ≥
                        && !combined.contains(&format!("\u{2264} {}", name)); // ≤ param
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

/// Generate the auto-proof for a cover trace theorem.
///
/// Constructs concrete witness states by symbolically executing each handler in
/// the trace, then emits `let` declarations and an `exact ⟨..., by decide, ...⟩`.
///
/// Returns None if the trace can't be auto-proven (e.g., handler not found).
fn cover_trace_proof(
    spec: &ParsedSpec,
    trace: &[String],
    fields: &[(String, String)],
    lifecycle: &[String],
) -> Option<String> {
    if trace.is_empty() {
        return None;
    }

    let mut state = WitnessState::new(fields, lifecycle);
    type CoverStep = (String, Vec<(String, String)>, WitnessState);
    let mut steps: Vec<CoverStep> = Vec::new();

    // Pre-step: for the first handler with a `who` clause, we need signer = s.who_field.
    // Since we init all Pubkeys to pk and signer to pk, this works automatically.

    for op_name in trace {
        let handler = spec.handlers.iter().find(|o| o.name == *op_name)?;
        let param_values = choose_param_values(handler);

        // Save current state before applying effects (we need it for the proof)
        let state_before = WitnessState {
            fields: state.fields.clone(),
            status: state.status.clone(),
        };

        state.apply(handler, &param_values);

        steps.push((op_name.clone(), param_values, state_before));
    }

    // Build the proof
    let mut proof = String::new();
    proof.push_str(" := by\n");

    // Emit pk definition
    proof.push_str("  let pk : Pubkey := ⟨0, 0, 0, 0⟩\n");

    // Emit s0 (initial state — from the first step's state_before)
    if let Some((_, _, ref s0)) = steps.first() {
        proof.push_str(&format!("  let s0 : State := {}\n", s0.to_lean()));
    }

    // Emit intermediate states s1, s2, ... (post-state of each step except last)
    for (i, (_, _, _)) in steps.iter().enumerate() {
        if i < steps.len() - 1 {
            // The post-state of step i becomes s{i+1}
            // We need the state AFTER applying step i
            let mut s = WitnessState::new(fields, lifecycle);
            for step in steps.iter().take(i + 1) {
                let handler = spec.handlers.iter().find(|o| o.name == step.0)?;
                s.apply(handler, &step.1);
            }
            proof.push_str(&format!("  let s{} : State := {}\n", i + 1, s.to_lean()));
        }
    }

    // Build the exact ⟨...⟩ term
    // Structure: ⟨s0, pk, [params...], s1, by decide, [params...], s2, by decide, ..., by decide⟩
    let mut exact_parts: Vec<String> = Vec::new();
    exact_parts.push("s0".to_string());
    exact_parts.push("pk".to_string());

    for (i, (_op_name, param_values, _)) in steps.iter().enumerate() {
        // Add parameter witness values
        for (_, val) in param_values {
            exact_parts.push(val.clone());
        }

        if i < steps.len() - 1 {
            // Intermediate step: add s_{i+1} and `by decide`
            exact_parts.push(format!("s{}", i + 1));
            exact_parts.push("by decide".to_string());
        } else {
            // Last step: just `by decide`
            exact_parts.push("by decide".to_string());
        }
    }

    proof.push_str(&format!("  exact ⟨{}⟩\n", exact_parts.join(", ")));

    Some(proof)
}

/// Render cover properties — existential reachability proofs.
fn render_covers(out: &mut String, spec: &ParsedSpec, state_type: &str) {
    if spec.covers.is_empty() {
        return;
    }

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Cover properties — reachability (existential proofs)\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    // Helper: resolve the state type for a handler
    let resolve_state_type = |op_name: &str| -> String {
        let op = spec.handlers.iter().find(|o| o.name == op_name);
        if let Some(op) = op {
            if let Some(ref acct) = op.on_account {
                // If on_account matches the primary state type name, use it directly
                if acct == state_type {
                    return state_type.to_string();
                }
                return lean_state_name(acct);
            }
        }
        state_type.to_string()
    };

    for cover in &spec.covers {
        for (i, trace) in cover.traces.iter().enumerate() {
            let suffix = if cover.traces.len() > 1 {
                format!("_{}", i)
            } else {
                String::new()
            };

            // For multi-account specs, check if all ops share the same state type
            let trace_state_types: Vec<String> =
                trace.iter().map(|op| resolve_state_type(op)).collect();
            let all_same = trace_state_types.windows(2).all(|w| w[0] == w[1]);
            let effective_type = if all_same && !trace_state_types.is_empty() {
                trace_state_types[0].clone()
            } else {
                // Cross-account trace — skip with a comment
                out.push_str(&format!(
                    "-- cover_{}{}: trace [{}] spans multiple account types, skipped\n\n",
                    cover.name,
                    suffix,
                    trace.join(", ")
                ));
                continue;
            };

            // Generate existential proof: there exists initial state and signer such that
            // the trace sequence produces a valid final state
            out.push_str(&format!(
                "/-- {} — trace [{}] is reachable. -/\n",
                cover.name,
                trace.join(", ")
            ));
            out.push_str(&format!(
                "theorem cover_{}{} : ∃ (s0 : {}) (signer : Pubkey),\n",
                cover.name, suffix, effective_type
            ));
            // Build nested match chain
            let mut indent = "    ".to_string();
            for (j, op_name) in trace.iter().enumerate() {
                let trans = safe_name(&format!("{}Transition", op_name));
                let handler = spec.handlers.iter().find(|o| o.name == *op_name);
                let param_args = handler
                    .map(|o| {
                        o.takes_params
                            .iter()
                            .enumerate()
                            .map(|(k, (_, _))| format!("v{}_{}", j, k))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                let extra_exists = handler
                    .map(|o| {
                        o.takes_params
                            .iter()
                            .enumerate()
                            .map(|(k, (_, t))| format!("(v{}_{} : {})", j, k, map_type(t)))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();

                if !extra_exists.is_empty() {
                    out.push_str(&format!("{}∃ {}, ", indent, extra_exists));
                }

                let s_var = if j == 0 {
                    "s0".to_string()
                } else {
                    format!("s{}", j)
                };
                let s_next = format!("s{}", j + 1);

                if j < trace.len() - 1 {
                    let param_str = if param_args.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", param_args)
                    };
                    out.push_str(&format!(
                        "∃ ({} : {}), {} {} signer{} = some {} ∧\n",
                        s_next, effective_type, trans, s_var, param_str, s_next
                    ));
                    indent.push_str("  ");
                } else {
                    let param_str = if param_args.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", param_args)
                    };
                    // Try to auto-prove with witness construction
                    let proof =
                        cover_trace_proof(spec, trace, &spec.state_fields, &spec.lifecycle_states);
                    if let Some(proof_script) = proof {
                        out.push_str(&format!(
                            "{} {} signer{} ≠ none{}\n",
                            trans, s_var, param_str, proof_script
                        ));
                    } else {
                        out.push_str(&format!(
                            "{} {} signer{} ≠ none := sorry\n\n",
                            trans, s_var, param_str
                        ));
                    }
                }
            }
        }

        for (op_name, when_expr) in &cover.reachable {
            out.push_str(&format!("/-- {} — {} is reachable", cover.name, op_name));
            if let Some(ref expr) = when_expr {
                out.push_str(&format!(" when {}. -/\n", expr));
            } else {
                out.push_str(". -/\n");
            }
            out.push_str(&format!(
                "theorem cover_{}_{} : ∃ (s : {}) (signer : Pubkey),\n",
                cover.name,
                safe_name(op_name),
                state_type
            ));
            if let Some(ref expr) = when_expr {
                out.push_str(&format!("    {} ∧ ", expr));
            } else {
                out.push_str("    ");
            }
            let trans = safe_name(&format!("{}Transition", op_name));
            let handler = spec.handlers.iter().find(|o| o.name == *op_name);
            let param_exists = handler
                .map(|o| {
                    o.takes_params
                        .iter()
                        .map(|(n, t)| format!("({} : {})", n, map_type(t)))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let param_args = handler
                .map(|o| param_args_str(&o.takes_params))
                .unwrap_or_default();
            if !param_exists.is_empty() {
                out.push_str(&format!("∃ {}, ", param_exists));
            }
            out.push_str(&format!(
                "{} s signer{} ≠ none := sorry\n\n",
                trans, param_args
            ));
        }
    }
}

/// Render liveness properties — bounded reachability from one state to another.
fn render_liveness(out: &mut String, spec: &ParsedSpec, state_type: &str) {
    if spec.liveness_props.is_empty() {
        return;
    }

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Liveness properties — bounded reachability (leads-to)\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    // Helper: resolve state type for a liveness block from its via operations
    let resolve_liveness_state = |via_ops: &[String]| -> String {
        if !spec.account_types.is_empty() && !via_ops.is_empty() {
            // Check the first via op's on_account
            if let Some(op) = spec.handlers.iter().find(|o| o.name == via_ops[0]) {
                if let Some(ref acct) = op.on_account {
                    return lean_state_name(acct);
                }
            }
        }
        state_type.to_string()
    };

    // Track which applyOps helpers we've already emitted
    let mut emitted_helpers: Vec<String> = Vec::new();

    for liveness in &spec.liveness_props {
        let effective_type = resolve_liveness_state(&liveness.via_ops);
        let bound = liveness.within_steps.unwrap_or(10);

        // Derive operation type and applyOp dispatcher
        let (op_type, apply_fn, prefix) = if effective_type == "State" {
            (
                "Operation".to_string(),
                "applyOp".to_string(),
                String::new(),
            )
        } else if effective_type.ends_with("State") {
            let p = effective_type[..effective_type.len() - 5].to_string();
            (format!("{}Operation", p), format!("apply{}Op", p), p)
        } else {
            (
                "Operation".to_string(),
                "applyOp".to_string(),
                String::new(),
            )
        };

        let apply_ops_fn = format!("apply{}Ops", prefix);

        // Emit applyOps helper if not already emitted for this type
        if !emitted_helpers.contains(&effective_type) {
            out.push_str(&format!(
                "def {} (s : {}) (signer : Pubkey) : List {} → Option {}\n",
                apply_ops_fn, effective_type, op_type, effective_type
            ));
            out.push_str("  | [] => some s\n");
            out.push_str(&format!(
                "  | op :: ops => match {} s signer op with\n",
                apply_fn
            ));
            out.push_str(&format!(
                "    | some s' => {} s' signer ops\n",
                apply_ops_fn
            ));
            out.push_str("    | none => none\n\n");
            emitted_helpers.push(effective_type.clone());
        }

        out.push_str(&format!(
            "/-- {} — from {} leads to {} within {} steps via [{}]. -/\n",
            liveness.name,
            liveness.from_state,
            liveness.leads_to_state,
            bound,
            liveness.via_ops.join(", ")
        ));
        out.push_str(&format!(
            "theorem liveness_{} (s : {}) (signer : Pubkey)\n",
            liveness.name, effective_type
        ));
        out.push_str(&format!(
            "    (h : s.status = .{}) :\n",
            liveness.from_state
        ));

        // Find a path through the lifecycle graph using via ops
        let path = find_liveness_path(
            &liveness.from_state,
            &liveness.leads_to_state,
            &liveness.via_ops,
            &spec.handlers,
        );

        if let Some(ref ops_path) = path {
            let proof = liveness_proof_script(ops_path, &apply_ops_fn, &apply_fn, &spec.handlers);
            out.push_str(&format!(
                "    \u{2203} ops, ops.length \u{2264} {} \u{2227} \u{2200} s', {} s signer ops = some s' \u{2192} s'.status = .{}{}\n",
                bound, apply_ops_fn, liveness.leads_to_state, proof
            ));
        } else {
            // Fallback: can't find path, emit sorry
            out.push_str(&format!(
                "    \u{2203} ops, ops.length \u{2264} {} \u{2227} \u{2200} s', {} s signer ops = some s' \u{2192} s'.status = .{} := sorry\n\n",
                bound, apply_ops_fn, liveness.leads_to_state
            ));
        }
    }
}

/// Find a sequence of via ops that transitions from `from` to `to` through the lifecycle.
fn find_liveness_path(
    from_state: &str,
    to_state: &str,
    via_ops: &[String],
    handlers: &[crate::check::ParsedHandler],
) -> Option<Vec<String>> {
    // Single step: find a via op that goes directly from → to
    for op_name in via_ops {
        if let Some(handler) = handlers.iter().find(|h| h.name == *op_name) {
            let pre = handler.pre_status.as_deref().unwrap_or("");
            let post = handler.post_status.as_deref().unwrap_or("");
            if pre == from_state && post == to_state {
                return Some(vec![op_name.clone()]);
            }
        }
    }

    // Multi-step: BFS through lifecycle states using via ops (max depth = via_ops.len())
    let mut queue: Vec<(String, Vec<String>)> = vec![(from_state.to_string(), Vec::new())];
    let max_depth = via_ops.len();

    while let Some((current, path)) = queue.first().cloned() {
        queue.remove(0);
        if path.len() >= max_depth {
            continue;
        }
        for op_name in via_ops {
            if let Some(handler) = handlers.iter().find(|h| h.name == *op_name) {
                let pre = handler.pre_status.as_deref().unwrap_or("");
                let post = handler.post_status.as_deref().unwrap_or("");
                if pre == current && !post.is_empty() {
                    let mut new_path = path.clone();
                    new_path.push(op_name.clone());
                    if post == to_state {
                        return Some(new_path);
                    }
                    queue.push((post.to_string(), new_path));
                }
            }
        }
    }
    None
}

/// Generate a liveness proof script for a given ops path.
///
/// For each step in the path, unfolds the transition and uses `split at h_apply`
/// to handle the `if` guard. The true branch proceeds to the next step; the false
/// branch is closed by `simp at h_apply` (vacuously true: `none ≠ some`).
fn liveness_proof_script(
    ops_path: &[String],
    apply_ops_fn: &str,
    apply_fn: &str,
    handlers: &[crate::check::ParsedHandler],
) -> String {
    let n = ops_path.len();

    // Build the ops list literal: [.op1, .op2, ...]
    let ops_list: Vec<String> = ops_path
        .iter()
        .map(|name| format!(".{}", safe_name(name)))
        .collect();
    let ops_literal = format!("[{}]", ops_list.join(", "));

    let mut proof = String::new();
    proof.push_str(" := by\n");
    proof.push_str(&format!(
        "  refine \u{27E8}{}, by decide, fun s' h_apply => ?\u{5F}\u{27E9}\n",
        ops_literal
    ));

    // Check if any op in the path has a `who` guard or other non-trivially-reducible condition
    let needs_split: Vec<bool> = ops_path
        .iter()
        .map(|name| {
            handlers
                .iter()
                .find(|h| h.name == *name)
                .map(|h| h.who.is_some() || h.guard_str.is_some() || !h.requires.is_empty())
                .unwrap_or(false)
        })
        .collect();

    // Collect transition names for the simp set
    let trans_names: Vec<String> = ops_path
        .iter()
        .map(|name| safe_name(&format!("{}Transition", name)))
        .collect();

    if n == 1 {
        // Single-step liveness
        let trans = &trans_names[0];
        if needs_split[0] {
            // Has who/guard — need double split:
            // First split on the match in applyOps (some vs none), then split on
            // the if inside the transition to extract the concrete post-state.
            proof.push_str(&format!(
                "  simp only [{}, {}, {}] at h_apply\n",
                apply_ops_fn, apply_fn, trans
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
            // No who — simp with h fully reduces the if
            proof.push_str(&format!(
                "  simp only [{}, {}, {}, h, \u{2193}reduceIte] at h_apply\n",
                apply_ops_fn, apply_fn, trans
            ));
            proof.push_str("  cases h_apply; rfl\n");
        }
    } else {
        // Multi-step: unfold applyOps step by step.
        //
        // For each step, we split the outer match in applyOps, then if the transition
        // has a guard (who/requires), we do a double split to resolve the if condition
        // and substitute the concrete post-state before proceeding to the next step.
        proof.push_str(&format!(
            "  simp only [{}, {}] at h_apply\n",
            apply_ops_fn, apply_fn,
        ));

        liveness_multi_step_proof(
            &mut proof,
            &trans_names,
            &needs_split,
            0,
            "  ",
            apply_ops_fn,
            apply_fn,
        );
    }

    proof
}

/// Recursively generate the nested split proof for multi-step liveness.
#[allow(clippy::only_used_in_recursion)]
fn liveness_multi_step_proof(
    proof: &mut String,
    trans_names: &[String],
    needs_split: &[bool],
    step: usize,
    indent: &str,
    apply_ops_fn: &str,
    apply_fn: &str,
) {
    if step >= trans_names.len() {
        return;
    }

    let trans = &trans_names[step];
    let is_last = step == trans_names.len() - 1;

    proof.push_str(&format!("{}simp only [{}] at h_apply\n", indent, trans));
    proof.push_str(&format!("{}split at h_apply\n", indent));

    if is_last {
        // Last step: the true branch must prove the target status.
        if needs_split[step] {
            // Double split: resolve the if, then subst, then rfl
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
    } else {
        // Non-last step: resolve this step's transition, then recurse.
        // NOTE: The initial `simp only [applyOps, applyOp]` at the top level
        // already unfolded the entire applyOps chain. After resolving each step
        // via subst/cases, the remaining chain is in unfolded form — only the
        // next transition name needs to be simp'd.
        if needs_split[step] {
            // Guard present: double split to resolve the if and get concrete state
            proof.push_str(&format!("{}\u{B7} next heq =>\n", indent));
            let inner = format!("{}  ", indent);
            proof.push_str(&format!("{}split at heq\n", inner));
            proof.push_str(&format!("{}\u{B7} next hg =>\n", inner));
            let inner2 = format!("{}  ", inner);
            proof.push_str(&format!("{}simp at heq\n", inner2));
            proof.push_str(&format!("{}subst heq\n", inner2));
            // Recurse: only simp the next transition, not applyOps/applyOp
            liveness_multi_step_proof(
                proof,
                trans_names,
                needs_split,
                step + 1,
                &inner2,
                apply_ops_fn,
                apply_fn,
            );
            proof.push_str(&format!("{}\u{B7} simp at heq\n", inner));
        } else {
            // No guard: simple split and recurse
            proof.push_str(&format!("{}\u{B7}\n", indent));
            let next_indent = format!("{}  ", indent);
            liveness_multi_step_proof(
                proof,
                trans_names,
                needs_split,
                step + 1,
                &next_indent,
                apply_ops_fn,
                apply_fn,
            );
        }
    }

    // False branch: none = some s' is absurd
    proof.push_str(&format!("{}\u{B7} simp at h_apply\n", indent));
}

/// Render environment block theorems — properties hold under external state changes.
fn render_environments(out: &mut String, spec: &ParsedSpec, state_type: &str) {
    if spec.environments.is_empty() {
        return;
    }

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Environment — properties hold under external state changes\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    for env in &spec.environments {
        // For each property, generate a theorem showing it holds after env mutation
        for prop in &spec.properties {
            if prop.expression.is_none() {
                continue;
            }

            // Build parameter signature for mutated fields
            let param_sig: String = env
                .mutates
                .iter()
                .map(|(name, typ)| format!(" (new_{} : {})", name, map_type(typ)))
                .collect();

            // Build constraint hypotheses
            let constraint_hyps: String = env
                .constraints
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    // Replace field refs with new_ prefixed versions
                    let mut expr = c.clone();
                    for (field, _) in &env.mutates {
                        expr = expr
                            .replace(&format!("s.{}", field), &format!("new_{}", field))
                            .replace(&format!("state.{}", field), &format!("new_{}", field));
                        // Bare field name in constraint
                        if expr.trim() == *field || expr.contains(field) {
                            expr = expr.replace(field, &format!("new_{}", field));
                        }
                    }
                    format!("\n    (h_c{} : {})", i, expr)
                })
                .collect();

            // Build with-update
            let with_parts: String = env
                .mutates
                .iter()
                .map(|(name, _)| format!("{} := new_{}", safe_name(name), name))
                .collect::<Vec<_>>()
                .join(", ");

            out.push_str(&format!(
                "theorem {}_under_{} (s : {}){}{}\n",
                prop.name, env.name, state_type, param_sig, constraint_hyps
            ));
            out.push_str(&format!("    (h_inv : {} s) :\n", prop.name));

            // Auto-prove: if mutated fields don't appear in the property expression,
            // the property is trivially preserved (struct update doesn't touch relevant fields).
            let prop_expr = prop.expression.as_deref().unwrap_or("");
            let mutated_fields_overlap = env.mutates.iter().any(|(field, _)| {
                // Check if the field name appears in the property expression
                // (as s.field or bare field reference)
                prop_expr.contains(&format!("s.{}", safe_name(field)))
                    || prop_expr.contains(&format!("state.{}", field))
            });

            if !mutated_fields_overlap {
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

/// Render aborts_if theorems — prove that operations reject under specified conditions.
/// Also generates abort theorems from `requires ... else Error` clauses (negated form).
fn render_aborts_if(
    out: &mut String,
    ops: &[&crate::check::ParsedHandler],
    fields: &[(String, String)],
    fallback_fields: &[(String, String)],
    state_type: &str,
) {
    let has_aborts = ops
        .iter()
        .any(|op| !op.aborts_if.is_empty() || op.requires.iter().any(|r| r.error_name.is_some()));
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

    for op in ops {
        let trans_name = safe_name(&format!("{}Transition", op.name));
        let param_sig = param_sig_str(&op.takes_params);
        let param_args = param_args_str(&op.takes_params);

        // Build guard condition parts (same structure as render_transitions)
        let cond_parts = build_guard_cond_parts(op, fields, fallback_fields);

        // Collect all abort conditions (negated form)
        let mut all_abort_conditions: Vec<String> = Vec::new();

        // Traditional aborts_if clauses — the expression IS the abort condition
        for abort in &op.aborts_if {
            all_abort_conditions.push(abort.lean_expr.clone());
        }

        // Requires clauses with else Error — negated positive condition
        for req in &op.requires {
            if req.error_name.is_some() {
                all_abort_conditions.push(format!("\u{00AC}({})", req.lean_expr));
                // ¬(...)
            }
        }

        if op.aborts_total && !all_abort_conditions.is_empty() {
            // Aborts total: single IFF theorem with disjunction of all conditions
            let theorem_name = safe_name(&format!("{}_aborts_iff", op.name));
            out.push_str(&format!(
                "theorem {} (s : {}) (signer : Pubkey){} :\n",
                theorem_name, state_type, param_sig
            ));
            out.push_str(&format!(
                "    {} s signer{} = none \u{2194}\n",
                trans_name, param_args
            ));
            let disjunction = all_abort_conditions.join(" \u{2228} "); // ∨
            out.push_str(&format!("    ({}) := sorry\n\n", disjunction));
        } else {
            // Count per-error occurrences across both aborts_if and
            // requires-with-else so duplicates (issue #8 finding #3)
            // can be disambiguated. When the same error name appears
            // multiple times across a single handler — common in
            // real Anchor programs where one error code covers several
            // preconditions — bare `{op}_aborts_if_{error}` collides
            // and Lake reports "already been declared". Suffix each
            // occurrence with its positional index (_0, _1, …) when
            // count > 1; keep the unsuffixed form for unique cases so
            // bundled examples don't churn.
            let mut error_total: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for abort in &op.aborts_if {
                *error_total.entry(abort.error_name.clone()).or_insert(0) += 1;
            }
            for req in &op.requires {
                if let Some(ref e) = req.error_name {
                    *error_total.entry(e.clone()).or_insert(0) += 1;
                }
            }
            let mut error_seen: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            let theorem_name_for = |error_name: &str,
                                    seen: &mut std::collections::HashMap<String, usize>|
             -> String {
                let total = error_total.get(error_name).copied().unwrap_or(0);
                let idx = {
                    let entry = seen.entry(error_name.to_string()).or_insert(0);
                    let cur = *entry;
                    *entry += 1;
                    cur
                };
                if total > 1 {
                    safe_name(&format!("{}_aborts_if_{}_{}", op.name, error_name, idx))
                } else {
                    safe_name(&format!("{}_aborts_if_{}", op.name, error_name))
                }
            };

            // Per-condition abort theorems
            for abort in &op.aborts_if {
                let theorem_name = theorem_name_for(&abort.error_name, &mut error_seen);
                out.push_str(&format!(
                    "theorem {} (s : {}) (signer : Pubkey){}\n",
                    theorem_name, state_type, param_sig
                ));
                out.push_str(&format!(
                    "    (h : {}) : {} s signer{} = none := sorry\n\n",
                    abort.lean_expr, trans_name, param_args
                ));
            }

            // Requires-based abort theorems — auto-proven via if_neg projection
            for req in &op.requires {
                if let Some(ref error_name) = req.error_name {
                    let theorem_name = theorem_name_for(error_name, &mut error_seen);
                    out.push_str(&format!(
                        "theorem {} (s : {}) (signer : Pubkey){}\n",
                        theorem_name, state_type, param_sig
                    ));

                    // Find the position of this requires expression in cond_parts
                    let req_pos = cond_parts.iter().position(|c| c == &req.lean_expr);

                    if let Some(pos) = req_pos {
                        let proof = abort_requires_proof(&trans_name, &cond_parts, pos);
                        out.push_str(&format!(
                            "    (h : \u{00AC}({})) : {} s signer{} = none{}\n",
                            req.lean_expr, trans_name, param_args, proof
                        ));
                    } else {
                        // Fallback: can't locate in guard, emit sorry
                        out.push_str(&format!(
                            "    (h : \u{00AC}({})) : {} s signer{} = none := sorry\n\n",
                            req.lean_expr, trans_name, param_args
                        ));
                    }
                }
            }
        }
    }
}

/// Render post-condition theorems from `ensures` clauses.
///
/// Each ensures clause generates a theorem of the form:
/// ```lean
/// theorem handler_ensures_N (s s' : State) (signer : Pubkey) ...
///     (h : handlerTransition s signer ... = some s') :
///     <ensures_expr> := sorry
/// ```
/// In the ensures expression, `state.field` is rendered as `s'.field` (post-state)
/// and `old(state.field)` as `s.field` (pre-state).
fn render_ensures(out: &mut String, ops: &[&crate::check::ParsedHandler], state_type: &str) {
    let has_ensures = ops.iter().any(|op| !op.ensures.is_empty());
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

    for op in ops {
        for (i, ens) in op.ensures.iter().enumerate() {
            let trans_name = safe_name(&format!("{}Transition", op.name));
            let param_sig = param_sig_str(&op.takes_params);

            let theorem_name = safe_name(&format!("{}_ensures_{}", op.name, i));
            out.push_str(&format!(
                "theorem {} (s s' : {}) (signer : Pubkey){}\n",
                theorem_name, state_type, param_sig
            ));
            out.push_str(&format!(
                "    (h : {} s signer{} = some s') :\n",
                trans_name,
                param_args_str(&op.takes_params)
            ));
            out.push_str(&format!("    {} := sorry\n\n", ens.lean_expr));
        }
    }
}

/// Render frame condition theorems from `modifies` clauses.
///
/// For each handler with a `modifies` clause, generates a theorem proving that
/// all fields NOT in the modifies list remain unchanged after the transition.
/// If the handler also transitions lifecycle (pre/post status), `status` is
/// implicitly considered modified.
fn render_frame_conditions(
    out: &mut String,
    ops: &[&crate::check::ParsedHandler],
    fields: &[(String, String)],
    state_type: &str,
) {
    let has_modifies = ops.iter().any(|op| op.modifies.is_some());
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

    for op in ops {
        if let Some(ref modified_fields) = op.modifies {
            let trans_name = safe_name(&format!("{}Transition", op.name));
            let param_sig = param_sig_str(&op.takes_params);

            // Compute unchanged fields: all fields minus modified ones.
            // If handler transitions lifecycle, status is implicitly modified.
            let status_is_modified = op.pre_status.is_some() && op.post_status.is_some();
            let unchanged: Vec<&str> = fields
                .iter()
                .filter(|(name, _)| {
                    !(modified_fields.contains(name) || name == "status" && status_is_modified)
                })
                .map(|(name, _)| name.as_str())
                .collect();

            if unchanged.is_empty() {
                continue;
            }

            let theorem_name = safe_name(&format!("{}_frame", op.name));
            out.push_str(&format!(
                "theorem {} (s s' : {}) (signer : Pubkey){}\n",
                theorem_name, state_type, param_sig
            ));
            out.push_str(&format!(
                "    (h : {} s signer{} = some s') :\n",
                trans_name,
                param_args_str(&op.takes_params)
            ));

            let frame_conjuncts: Vec<String> = unchanged
                .iter()
                .map(|f| format!("s'.{} = s.{}", safe_name(f), safe_name(f)))
                .collect();
            out.push_str(&format!(
                "    {} := sorry\n\n",
                frame_conjuncts.join(" \u{2227} ") // ∧
            ));
        }
    }
}

/// Render overflow safety obligations for operations with add effects.
///
/// For each operation that has "add" effects on numeric fields, generates a
/// theorem requiring that all numeric fields in the post-state remain valid
/// (within their declared type's bounds).
fn render_overflow_obligations(
    out: &mut String,
    spec: &ParsedSpec,
    ops: &[&crate::check::ParsedHandler],
    fields: &[(String, String)],
    state_type: &str,
) {
    // Collect handlers that have add effects
    let add_ops: Vec<&&crate::check::ParsedHandler> = ops
        .iter()
        .filter(|op| op.effects.iter().any(|(_, kind, _)| kind == "add"))
        .collect();

    if add_ops.is_empty() {
        return;
    }

    // Collect numeric field names for the validity predicate
    let numeric_fields: Vec<&str> = fields
        .iter()
        .filter(|(_, t)| {
            matches!(
                t.as_str(),
                "U8" | "U16" | "U32" | "U64" | "U128" | "I64" | "I128"
            )
        })
        .map(|(n, _)| n.as_str())
        .collect();

    if numeric_fields.is_empty() {
        return;
    }

    // Determine the appropriate bounds predicate based on field types
    // Use the widest type present to determine the bound
    let valid_fn = |ftype: &str| -> &str {
        match ftype {
            "U8" => "valid_u8",
            "U16" => "valid_u16",
            "U32" => "valid_u32",
            "U64" => "valid_u64",
            "U128" => "valid_u128",
            "I64" => "valid_i64",
            "I128" => "valid_i128",
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

    for op in &add_ops {
        let trans_name = safe_name(&format!("{}Transition", op.name));
        let param_sig = param_sig_str(&op.takes_params);

        // Build pre-condition: all numeric fields are valid
        let pre_parts: Vec<String> = fields
            .iter()
            .filter(|(_, t)| {
                matches!(
                    t.as_str(),
                    "U8" | "U16" | "U32" | "U64" | "U128" | "I64" | "I128"
                )
            })
            .map(|(n, t)| format!("{} s.{}", valid_fn(t), safe_name(n)))
            .collect();

        // Build post-condition: all numeric fields remain valid
        let post_parts: Vec<String> = fields
            .iter()
            .filter(|(_, t)| {
                matches!(
                    t.as_str(),
                    "U8" | "U16" | "U32" | "U64" | "U128" | "I64" | "I128"
                )
            })
            .map(|(n, t)| format!("{} s'.{}", valid_fn(t), safe_name(n)))
            .collect();

        // Collect invariant hypotheses: all properties that cover this operation
        let inv_hyps: Vec<String> = spec
            .properties
            .iter()
            .filter(|p| p.preserved_by.contains(&op.name) && p.expression.is_some())
            .map(|p| p.name.clone())
            .collect();

        out.push_str(&format!(
            "theorem {}_overflow_safe (s s' : {}) (signer : Pubkey){}\n",
            safe_name(&op.name),
            state_type,
            param_sig
        ));
        let pre_joined = pre_parts
            .iter()
            .map(|p| paren_if_low_prec(p))
            .collect::<Vec<_>>()
            .join(" ∧ ");
        out.push_str(&format!("    (h_valid : {})\n", pre_joined));
        for inv in &inv_hyps {
            out.push_str(&format!("    (h_inv_{} : {} s)\n", safe_name(inv), inv));
        }
        out.push_str(&format!(
            "    (h : {} s signer{} = some s') :\n",
            trans_name,
            param_args_str(&op.takes_params)
        ));
        // Generate proof script
        let has_cond = handler_has_condition(op, fields);
        let proof = overflow_proof_script(op, fields, has_cond);
        let post_joined = post_parts
            .iter()
            .map(|p| paren_if_low_prec(p))
            .collect::<Vec<_>>()
            .join(" ∧ ");
        out.push_str(&format!("    {}{}\n", post_joined, proof));
    }
}

/// Generate a mechanical proof script for an overflow safety theorem.
///
/// For each numeric field in the post-state:
/// - Unchanged fields: project from `h_valid` hypothesis
/// - Add-modified fields: unfold the `valid_T` predicate and use `omega`
///   (the guard provides the overflow bound)
fn overflow_proof_script(
    op: &crate::check::ParsedHandler,
    fields: &[(String, String)],
    has_cond: bool,
) -> String {
    let trans_name = safe_name(&format!("{}Transition", op.name));

    // Collect numeric fields with their types (in order matching h_valid)
    let numeric_fields: Vec<(&str, &str)> = fields
        .iter()
        .filter(|(_, t)| {
            matches!(
                t.as_str(),
                "U8" | "U16" | "U32" | "U64" | "U128" | "I64" | "I128"
            )
        })
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();

    let n = numeric_fields.len();
    if n == 0 {
        return " := sorry\n".to_string();
    }

    // Build refine tuple: h_valid projections for unchanged fields, ?_ for changed
    let mut refine_parts: Vec<String> = Vec::new();
    let mut changed_types: Vec<&str> = Vec::new();

    for (i, (name, ftype)) in numeric_fields.iter().enumerate() {
        let is_add = op.effects.iter().any(|(f, k, _)| f == name && k == "add");
        if is_add {
            refine_parts.push("?_".to_string());
            changed_types.push(ftype);
        } else {
            // h_valid projection (right-associative ∧ chain)
            let proj = h_valid_projection(i, n);
            refine_parts.push(proj);
        }
    }

    // Build simp lemmas for each changed field
    let simp_goals: Vec<String> = changed_types
        .iter()
        .map(|ftype| {
            let vfn = valid_fn_name(ftype);
            let vmod = valid_module_name(ftype);
            let vmax = valid_max_name(ftype);
            format!("    simp only [{}, {}, {}]; omega", vfn, vmod, vmax)
        })
        .collect();

    let refine_str = format!("\u{27E8}{}\u{27E9}", refine_parts.join(", "));

    if has_cond {
        let mut proof = format!(" := by\n  unfold {} at h; split at h\n", trans_name);
        proof.push_str("  · next hg =>\n    cases h\n");
        proof.push_str(&format!("    refine {}\n", refine_str));
        for goal in &simp_goals {
            proof.push_str(&format!("{}\n", goal));
        }
        proof.push_str("  · contradiction\n");
        proof
    } else {
        let mut proof = format!(" := by\n  unfold {} at h; cases h\n", trans_name);
        proof.push_str(&format!("  refine {}\n", refine_str));
        for goal in &simp_goals {
            proof.push_str(&format!("{}\n", goal));
        }
        proof
    }
}

/// Generate h_valid projection path for position `i` in `n` numeric fields.
fn h_valid_projection(i: usize, n: usize) -> String {
    let mut path = "h_valid".to_string();
    for _ in 0..i {
        path.push_str(".2");
    }
    if i < n - 1 {
        path.push_str(".1");
    }
    path
}

/// Return the Lean `valid_*` function name for a DSL type.
fn valid_fn_name(ftype: &str) -> &str {
    match ftype {
        "U8" => "valid_u8",
        "U16" => "valid_u16",
        "U32" => "valid_u32",
        "U64" => "valid_u64",
        "U128" => "valid_u128",
        _ => "valid_u64",
    }
}

/// Return the fully-qualified `Valid.valid_*` name for simp unfolding.
fn valid_module_name(ftype: &str) -> &str {
    match ftype {
        "U8" => "Valid.valid_u8",
        "U16" => "Valid.valid_u16",
        "U32" => "Valid.valid_u32",
        "U64" => "Valid.valid_u64",
        "U128" => "Valid.valid_u128",
        _ => "Valid.valid_u64",
    }
}

/// Return the `Valid.*_MAX` constant name for simp unfolding.
fn valid_max_name(ftype: &str) -> &str {
    match ftype {
        "U8" => "Valid.U8_MAX",
        "U16" => "Valid.U16_MAX",
        "U32" => "Valid.U32_MAX",
        "U64" => "Valid.U64_MAX",
        "U128" => "Valid.U128_MAX",
        _ => "Valid.U64_MAX",
    }
}

// ============================================================================
// sBPF rendering — generates qedguards-compatible Lean from sBPF .qedspec
// ============================================================================

/// Render an sBPF spec into Lean 4 source.
///
/// Produces: namespace, error constants, offset constants, ea_* lemmas,
/// guard theorem stubs (with hypotheses derived from checks + layout),
/// and a Spec completeness structure.
fn render_sbpf(spec: &ParsedSpec) -> String {
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

    out.push_str("open QEDGen.Solana.SBPF\n");
    out.push_str("open QEDGen.Solana.SBPF.Memory\n\n");

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
        let init_expr = if has_insn_reg {
            format!("initState2 inputAddr insnAddr mem {}", entry)
        } else {
            "initState inputAddr mem".to_string()
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
                    out.push_str("    (inputAddr insnAddr : Nat) (mem : Mem)\n");
                } else {
                    out.push_str("    (inputAddr : Nat) (mem : Mem)\n");
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
                    binders.push("(mem : Mem)".to_string());
                } else {
                    binders.push("(inputAddr : Nat)".to_string());
                    binders.push("(mem : Mem)".to_string());
                }
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

/// Derive guard hypotheses from checks expression + input/insn layout.
fn derive_guard_hypotheses(
    guard: &crate::check::ParsedGuard,
    all_offsets: &[(&str, &str, i64, bool)],
    _instr: &crate::check::ParsedInstruction,
    _spec: &ParsedSpec,
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
                    ];
                    bindings.extend(rhs_bindings.clone());
                    bindings.push(format!(
                        "(h_{}_ne : {} \u{2260} {})",
                        var_name, var_name, rhs_var
                    ));
                    let after = Some(vec![format!(
                        "(h_{} : {} mem {} = {})",
                        var_name, read_fn, addr_expr, rhs_var
                    )]);
                    DerivedHypotheses { bindings, after }
                }
                ">=" => {
                    let mut bindings = vec![
                        format!("({} : Nat)", var_name),
                        format!(
                            "(h_{}_val : {} mem {} = {})",
                            var_name, read_fn, addr_expr, var_name
                        ),
                    ];
                    bindings.extend(rhs_bindings.clone());
                    bindings.push(format!("(h_{}_lt : {} < {})", var_name, var_name, rhs_var));
                    let mut after_binds = vec![
                        format!("({} : Nat)", var_name),
                        format!(
                            "(h_{}_val : {} mem {} = {})",
                            var_name, read_fn, addr_expr, var_name
                        ),
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

/// Map DSL types to Lean types.
///
/// Keep in sync with the Rust-side `codegen::primitive_map`. Any DSL
/// primitive with a Rust mapping must have a Lean mapping here too, or
/// it leaks through as its DSL name (`U16 → "U16"`) and Lake fails
/// with "Constructor field `U16` contains universe level metavariables".
/// Parity regression tracked as issue #8 finding #1.
fn map_type(t: &str) -> &str {
    match t {
        "U8" | "U16" | "U32" | "U64" | "U128" => "Nat",
        "I8" | "I16" | "I32" | "I64" | "I128" => "Int",
        _ => t,
    }
}

/// Return the Lean numeric literal for the maximum value of a DSL type.
/// Returns None for non-numeric types (Pubkey, Bool, etc.)
fn type_max_const(t: &str) -> Option<&str> {
    match t {
        "U8" => Some("255"),
        "U16" => Some("65535"),
        "U32" => Some("4294967295"),
        "U64" => Some("18446744073709551615"),
        "U128" => Some("340282366920938463463374607431768211455"),
        _ => None,
    }
}

/// Quote Lean keywords as «name».
/// Extract field names referenced in a Lean property expression.
///
/// Looks for patterns like `s.field_name` and returns the field names.
fn fields_referenced_in_expr(expr: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    for (i, _) in expr.match_indices("s.") {
        let rest = &expr[i + 2..];
        let end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        if end > 0 {
            let field = &rest[..end];
            if !fields.contains(&field) {
                fields.push(field);
            }
        }
    }
    fields
}

fn safe_name(name: &str) -> String {
    let keywords = [
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
    if keywords.contains(&name) {
        format!("\u{00AB}{}\u{00BB}", name)
    } else {
        name.to_string()
    }
}

/// Build parameter signature string for transition functions.
fn param_sig_str(params: &[(String, String)]) -> String {
    if params.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> = params
            .iter()
            .map(|(n, t)| format!(" ({} : {})", n, map_type(t)))
            .collect();
        parts.join("")
    }
}

// ============================================================================
// New-DSL renderer: record types + Map[N] T + sum/forall properties
// ============================================================================

/// Rewrite subscript syntax in Lean expressions: `A[i]` → `(A i)`.
/// Applies to each maximal preceding `A = [A-Za-z_][A-Za-z0-9_.]*`.
/// E.g. `s.accounts[i].capital` → `(s.accounts i).capital`.
fn rewrite_subscripts_lean(s: &str) -> String {
    // Uses char_indices so multi-byte UTF-8 (∧ ≤ ≥ ∀ ∃ ∑ etc.) is preserved.
    let mut out = String::with_capacity(s.len() + 8);
    let mut it = s.char_indices().peekable();
    while let Some((i, ch)) = it.next() {
        if ch != '[' {
            out.push(ch);
            continue;
        }
        // We just saw `[`. Walk back through `out` over the preceding
        // ASCII path characters to find the root.
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
        // Scan forward for `]` — subscript index is simple (ASCII ident only),
        // so byte-level find is safe here.
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
        // Advance the iterator past the consumed `[idx]`.
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

/// Return the const name that `AccountIdx` is bounded by.
/// Priority order:
///   1. An explicit `type AccountIdx = Fin[N]` alias, if declared.
///   2. Heuristic: first `MAX_*` const (excluding TVL-like caps) or first `MAX*`.
///   3. Literal `1024` fallback.
fn pick_account_idx_bound(spec: &ParsedSpec) -> String {
    // (1) Declared alias: find `AccountIdx` in type_aliases, parse `Fin[N]`.
    for (name, target) in &spec.type_aliases {
        if name == "AccountIdx" {
            if let Some(rest) = target.trim().strip_prefix("Fin") {
                let rest = rest.trim_start();
                if let Some(rest) = rest.strip_prefix('[') {
                    if let Some(close) = rest.find(']') {
                        return rest[..close].trim().to_string();
                    }
                }
            }
        }
    }
    // (2) Heuristic fallback — kept for specs that don't declare the alias.
    for (n, _) in &spec.constants {
        if n.starts_with("MAX_") && !n.contains("TVL") {
            return n.clone();
        }
    }
    for (n, _) in &spec.constants {
        if n.starts_with("MAX") {
            return n.clone();
        }
    }
    "1024".to_string()
}

/// Collect all Map-typed field names from account types, keyed by field name.
/// Returns (field_name → (bound_const, inner_record_name)).
fn collect_map_fields(spec: &ParsedSpec) -> std::collections::BTreeMap<String, (String, String)> {
    use std::collections::BTreeMap;
    let mut out = BTreeMap::new();
    for acct in &spec.account_types {
        for (fname, ftype) in &acct.fields {
            let trimmed = ftype.trim_start();
            if let Some(rest) = trimmed.strip_prefix("Map") {
                let rest = rest.trim_start();
                if let Some(rest) = rest.strip_prefix('[') {
                    if let Some(close) = rest.find(']') {
                        let bound = rest[..close].trim().to_string();
                        let inner = rest[close + 1..].trim().to_string();
                        out.insert(fname.clone(), (bound, inner));
                    }
                }
            }
        }
    }
    out
}

/// Map a DSL scalar type to Lean, falling back to record names as-is.
fn map_scalar_type(t: &str) -> String {
    match t.trim() {
        "U8" | "U16" | "U32" | "U64" | "U128" => "Nat".to_string(),
        "I8" | "I16" | "I32" | "I64" | "I128" => "Int".to_string(),
        "Bool" => "Bool".to_string(),
        "Pubkey" => "Pubkey".to_string(),
        other => other.to_string(),
    }
}

/// Default value for initializing a record field in a Map (for empty-slot defaults).
fn default_value_for(t: &str) -> &'static str {
    match t.trim() {
        "U8" | "U16" | "U32" | "U64" | "U128" => "0",
        "I8" | "I16" | "I32" | "I64" | "I128" => "0",
        "Bool" => "false",
        _ => "default",
    }
}

/// Rewrite a parsed effect value string so it refers to pre-state `s.` and
/// subscripts are in Lean form.
///   - integer literals → leave alone (strip underscores)
///   - handler params (in `params`) → pass through as-is
///   - anything else → prepend `s.` and rewrite subscripts
fn effect_value_to_lean(value: &str, params: &[(String, String)]) -> String {
    let trimmed = value.trim();
    // Integer literal
    if !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| c.is_ascii_digit() || c == '_' || c == '-')
    {
        return trimmed.replace('_', "");
    }
    // Handler-param reference — bare ident matching a declared param.
    let is_bare_ident = trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_');
    if is_bare_ident && params.iter().any(|(n, _)| n == trimmed) {
        return trimmed.to_string();
    }
    // Already pre-rendered in Lean form? Signals:
    //   - starts with `s.` (pre-state prefix added by adapter's expr_to_lean)
    //   - starts with `(` (parenthesized compound expression)
    //   - contains `match ` or `=>` or `.{Ident}` (constructor, record ops)
    // For these, do NOT re-prefix — just pass through subscript rewriting.
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
    // Bare field name: add pre-state prefix.
    let first = trimmed.chars().next().unwrap_or('_');
    let prefixed = if first.is_ascii_alphabetic() || first == '_' {
        format!("s.{}", trimmed)
    } else {
        trimmed.to_string()
    };
    rewrite_subscripts_lean(&prefixed)
}

/// One subscripted effect: `(inner_field, op_kind, value)` — parts of an
/// `accounts[i].inner_field (op) value` assignment.
type IndexedEffect = (String, String, String);

/// Per-`(root_field, idx)` group of subscripted effects, used to collapse
/// multiple `Function.update` calls targeting the same indexed path into one.
type IndexedEffectsByRoot = std::collections::BTreeMap<(String, String), Vec<IndexedEffect>>;

/// Split an indexed-path LHS `name[idx].field` into its parts.
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

/// Render a full Spec.lean for an indexed-state spec.
fn render_indexed_state(spec: &ParsedSpec) -> String {
    let mut out = String::new();

    // -- Imports --
    // `QEDGenMathlib.IndexedState` lives in the sibling lean_solana_mathlib
    // package (Mathlib-dependent slice). Its internal namespace is still
    // `QEDGen.Solana.IndexedState` so `open` statements and fully-qualified
    // references are unchanged from before the split.
    out.push_str("import Mathlib.Algebra.BigOperators.Fin\n");
    out.push_str("import QEDGen.Solana.Account\n");
    out.push_str("import QEDGenMathlib.IndexedState\n\n");

    out.push_str(&format!("namespace {}\n\n", spec.program_name));
    out.push_str("open QEDGen.Solana\n");
    out.push_str("open QEDGen.Solana.IndexedState\n\n");

    // -- Constants --
    for (name, val) in &spec.constants {
        out.push_str(&format!("abbrev {} : Nat := {}\n", safe_name(name), val));
    }
    if !spec.constants.is_empty() {
        out.push('\n');
    }

    // -- AccountIdx alias --
    let idx_bound = pick_account_idx_bound(spec);
    out.push_str(&format!(
        "abbrev AccountIdx : Type := Fin {}\n\n",
        idx_bound
    ));

    // -- Record structures (e.g. Account) --
    for rec in &spec.records {
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

    // -- Sum types (emitted as `inductive` with a `structure` per payload variant) --
    // For each variant that carries fields, emit `structure <Type><Variant>Data`
    // and reference it as the constructor's payload. No-payload variants become
    // bare constructors. A default Inhabited instance picks the first variant.
    for st in &spec.sum_types {
        // Emit payload structures.
        for v in &st.variants {
            if v.fields.is_empty() {
                continue;
            }
            let payload_name = format!("{}{}Data", st.name, v.name);
            out.push_str(&format!("structure {} where\n", payload_name));
            for (fname, ftype) in &v.fields {
                out.push_str(&format!(
                    "  {} : {}\n",
                    safe_name(fname),
                    map_scalar_type(ftype)
                ));
            }
            out.push_str("  deriving Repr, DecidableEq, BEq\n\n");

            out.push_str(&format!(
                "instance : Inhabited {} := \u{27E8}{{\n",
                payload_name
            ));
            for (fname, ftype) in &v.fields {
                out.push_str(&format!(
                    "  {} := {},\n",
                    safe_name(fname),
                    default_value_for(ftype)
                ));
            }
            out.push_str("}\u{27E9}\n\n");
        }

        // Emit the inductive itself.
        out.push_str(&format!("inductive {} where\n", st.name));
        for v in &st.variants {
            if v.fields.is_empty() {
                out.push_str(&format!("  | {}\n", v.name));
            } else {
                out.push_str(&format!("  | {} (d : {}{}Data)\n", v.name, st.name, v.name));
            }
        }
        out.push_str("  deriving Repr, DecidableEq, BEq\n\n");

        // Inhabited: pick the first no-payload variant, else the first variant
        // with its payload's default.
        let first_no_payload = st.variants.iter().find(|v| v.fields.is_empty());
        if let Some(v) = first_no_payload {
            out.push_str(&format!(
                "instance : Inhabited {} := \u{27E8}.{}\u{27E9}\n\n",
                st.name, v.name
            ));
        } else if let Some(v) = st.variants.first() {
            out.push_str(&format!(
                "instance : Inhabited {} := \u{27E8}.{} default\u{27E9}\n\n",
                st.name, v.name
            ));
        }

        // Per-variant Bool discriminator helpers: `T.isVariant : T → Bool`.
        // These make `x is .Variant` → `T.isVariant x = true` which Lean can
        // decide automatically (Bool equality is Decidable). Marked @[simp]
        // so proofs about them reduce automatically when the variant is
        // syntactically evident.
        for v in &st.variants {
            let pat = if v.fields.is_empty() {
                format!(".{}", v.name)
            } else {
                format!(".{} _", v.name)
            };
            out.push_str(&format!(
                "@[simp] def {ty}.is{vn} : {ty} \u{2192} Bool\n",
                ty = st.name,
                vn = v.name
            ));
            out.push_str(&format!("  | {} => true\n", pat));
            out.push_str("  | _ => false\n\n");
        }
    }

    // -- Status inductive (lifecycle) --
    let lifecycle = &spec.lifecycle_states;
    if !lifecycle.is_empty() {
        out.push_str("inductive Status where\n");
        for s in lifecycle {
            out.push_str(&format!("  | {}\n", s));
        }
        out.push_str("  deriving Repr, DecidableEq, BEq\n\n");
    }

    // -- State structure --
    // Fields are Active's payload; Status discriminates the variant.
    let map_fields = collect_map_fields(spec);
    let active_acct = spec.account_types.iter().find(|a| !a.fields.is_empty());
    out.push_str("structure State where\n");
    if let Some(acct) = active_acct {
        for (fname, ftype) in &acct.fields {
            let trimmed = ftype.trim();
            let lean_ty = if let Some(rest) = trimmed.strip_prefix("Map") {
                let rest = rest.trim_start();
                if let Some(rest) = rest.strip_prefix('[') {
                    if let Some(close) = rest.find(']') {
                        let bound = rest[..close].trim();
                        let inner = rest[close + 1..].trim();
                        format!("Map {} {}", bound, inner)
                    } else {
                        trimmed.to_string()
                    }
                } else {
                    trimmed.to_string()
                }
            } else {
                map_scalar_type(trimmed)
            };
            out.push_str(&format!("  {} : {}\n", safe_name(fname), lean_ty));
        }
    }
    if !lifecycle.is_empty() {
        out.push_str("  status : Status\n");
    }
    out.push('\n');

    // -- Transitions --
    for op in &spec.handlers {
        let trans_name = safe_name(&format!("{}Transition", op.name));
        let param_sig = param_sig_str(&op.takes_params);

        // Guard conjuncts
        let mut conds: Vec<String> = Vec::new();
        if let Some(ref who) = op.who {
            // Heuristic: who refers to a state field (e.g. `authority`).
            conds.push(format!("signer = s.{}", safe_name(who)));
        }
        if let Some(ref pre) = op.pre_status {
            conds.push(format!("s.status = .{}", pre));
        }
        for req in &op.requires {
            let rewritten = rewrite_subscripts_lean(&req.lean_expr);
            conds.push(format!("({})", rewritten));
        }

        // Effect updates. Scalar effects (on non-Map fields) are emitted as
        // normal record-update entries. Subscripted effects (`accounts[i].x`)
        // all sharing the same root and index are collapsed into a single
        // `Function.update` with an anonymous-record update that sets every
        // touched inner field.
        let mut scalar_parts: Vec<String> = Vec::new();
        // (root_field, idx) → Vec<(inner_field, op_kind, value)>
        let mut indexed_by_root: IndexedEffectsByRoot = std::collections::BTreeMap::new();
        for (field, op_kind, value) in &op.effects {
            if let Some((root, idx, inner_field)) = parse_indexed_lhs(field) {
                if map_fields.contains_key(root) {
                    indexed_by_root
                        .entry((root.to_string(), idx.to_string()))
                        .or_default()
                        .push((inner_field.to_string(), op_kind.clone(), value.clone()));
                    continue;
                }
            }
            // Plain scalar effect
            let sf = safe_name(field);
            let val_lean = effect_value_to_lean(value, &op.takes_params);
            match op_kind.as_str() {
                "add" => scalar_parts.push(format!("{} := s.{} + {}", sf, sf, val_lean)),
                "sub" => scalar_parts.push(format!("{} := s.{} - {}", sf, sf, val_lean)),
                "set" => scalar_parts.push(format!("{} := {}", sf, val_lean)),
                _ => {}
            }
        }

        let mut with_parts: Vec<String> = scalar_parts;
        for ((root, idx), ops) in &indexed_by_root {
            // Whole-map-entry update: LHS is `accounts[i] := <value>` with no
            // inner field. Emit `Function.update s.accounts i <value>`.
            // Detected by having exactly one op whose inner_field is empty.
            let whole_entry = ops.len() == 1 && ops[0].0.is_empty();
            let update = if whole_entry {
                let (_, _, value) = &ops[0];
                // Value is pre-rendered Lean from render_effect's complex-expr
                // path. Apply subscript rewriting so any `x[i]` inside a
                // match scrutinee or constructor payload becomes `(x i)`.
                let val_lean = rewrite_subscripts_lean(value);
                format!(
                    "Function.update s.{root} {idx} ({val})",
                    root = root,
                    idx = idx,
                    val = val_lean
                )
            } else {
                let mut inner_updates: Vec<String> = Vec::new();
                for (fname, op_kind, value) in ops {
                    let val_lean = effect_value_to_lean(value, &op.takes_params);
                    let rhs = match op_kind.as_str() {
                        "add" => format!(
                            "(s.{root} {idx}).{fname} + {val}",
                            root = root,
                            idx = idx,
                            fname = fname,
                            val = val_lean
                        ),
                        "sub" => format!(
                            "(s.{root} {idx}).{fname} - {val}",
                            root = root,
                            idx = idx,
                            fname = fname,
                            val = val_lean
                        ),
                        _ => val_lean,
                    };
                    inner_updates.push(format!("{} := {}", fname, rhs));
                }
                format!(
                    "Function.update s.{root} {idx} {{ (s.{root} {idx}) with {inners} }}",
                    root = root,
                    idx = idx,
                    inners = inner_updates.join(", ")
                )
            };
            with_parts.push(format!("{} := {}", safe_name(root), update));
        }
        if let Some(ref post) = op.post_status {
            with_parts.push(format!("status := .{}", post));
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

        if conds.is_empty() {
            out.push_str(&format!("  {}\n\n", then_body));
        } else {
            out.push_str(&format!("  if {} then\n", conds.join(" \u{2227} ")));
            out.push_str(&format!("    {}\n", then_body));
            out.push_str("  else none\n\n");
        }
    }

    // -- Operation inductive + applyOp --
    if !spec.handlers.is_empty() {
        out.push_str("inductive Operation where\n");
        for op in &spec.handlers {
            let args: String = op
                .takes_params
                .iter()
                .map(|(n, t)| format!(" ({} : {})", n, map_scalar_type(t)))
                .collect();
            out.push_str(&format!("  | {}{}\n", safe_name(&op.name), args));
        }
        out.push('\n');

        out.push_str("def applyOp (s : State) (signer : Pubkey) : Operation → Option State\n");
        for op in &spec.handlers {
            let binders: Vec<String> = op.takes_params.iter().map(|(n, _)| n.clone()).collect();
            let call_args = if binders.is_empty() {
                String::new()
            } else {
                format!(" {}", binders.join(" "))
            };
            let lhs_bind = if binders.is_empty() {
                String::new()
            } else {
                format!(" {}", binders.join(" "))
            };
            out.push_str(&format!(
                "  | .{name}{bind} => {name}Transition s signer{call}\n",
                name = safe_name(&op.name),
                bind = lhs_bind,
                call = call_args
            ));
        }
        out.push('\n');
    }

    // -- Property predicates --
    for prop in &spec.properties {
        if let Some(ref expr_lean) = prop.expression {
            let rewritten = rewrite_subscripts_lean(expr_lean);
            out.push_str(&format!(
                "/-- Property: {}. -/\ndef {} (s : State) : Prop :=\n  {}\n\n",
                prop.name,
                safe_name(&prop.name),
                rewritten
            ));
        }
    }

    // -- Preservation + liveness theorems are NOT emitted here.
    //
    // Durable user-owned proofs live in a sibling `Proofs.lean`. Codegen
    // never writes theorem bodies so regeneration can't clobber proof work.
    // `qedgen check` diffs the spec's preservation obligations against the
    // theorems declared in Proofs.lean and flags orphans/missing stubs.
    //
    // Users/agents write proofs in `Proofs.lean` with the shape:
    //   `theorem <prop>_preserved_by_<handler> (s s' : State) ... : ... := by ...`
    // `qedgen init` seeds a `Proofs.lean` scaffold on first run; subsequent
    // `qedgen codegen` calls leave it alone.

    out.push_str(&format!("end {}\n", spec.program_name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chumsky_adapter;

    const MULTISIG_SPEC: &str = include_str!("../../../examples/rust/multisig/multisig.qedspec");

    // Issue #8 fixture bundle (contributed by @lmvdz, gist at
    // https://gist.github.com/lmvdz/639804a0585317cb56cb14d2620e0ade).
    // Each `ISSUE_8_FIXTURES` entry is a `(name, source)` pair so a
    // failing iteration can report which fixture tripped.
    const ISSUE_8_FIXTURES: &[(&str, &str)] = &[
        (
            "pool",
            include_str!("../../../examples/regressions/issue-8/pool.qedspec"),
        ),
        (
            "repro-01-u16-type",
            include_str!("../../../examples/regressions/issue-8/repro-01-u16-type.qedspec"),
        ),
        (
            "repro-02-composite-or-parens",
            include_str!(
                "../../../examples/regressions/issue-8/repro-02-composite-or-parens.qedspec"
            ),
        ),
        (
            "repro-03-duplicate-theorem",
            include_str!(
                "../../../examples/regressions/issue-8/repro-03-duplicate-theorem.qedspec"
            ),
        ),
        (
            "repro-04-liveness-params",
            include_str!("../../../examples/regressions/issue-8/repro-04-liveness-params.qedspec"),
        ),
        (
            "repro-05-uninterpreted-helper",
            include_str!(
                "../../../examples/regressions/issue-8/repro-05-uninterpreted-helper.qedspec"
            ),
        ),
        (
            "repro-06-cover-witness-bool",
            include_str!(
                "../../../examples/regressions/issue-8/repro-06-cover-witness-bool.qedspec"
            ),
        ),
        (
            "repro-07-pubkey-literal-assign",
            include_str!(
                "../../../examples/regressions/issue-8/repro-07-pubkey-literal-assign.qedspec"
            ),
        ),
        (
            "repro-08-pubkey-literal-compare",
            include_str!(
                "../../../examples/regressions/issue-8/repro-08-pubkey-literal-compare.qedspec"
            ),
        ),
    ];

    #[test]
    fn lean_gen_has_namespace() {
        let spec = chumsky_adapter::parse_str(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("namespace Multisig"));
        assert!(lean.contains("end Multisig"));
    }

    #[test]
    fn lean_gen_has_status_inductive() {
        let spec = chumsky_adapter::parse_str(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("inductive Status where"));
        assert!(lean.contains("| Uninitialized"));
        assert!(lean.contains("| Active"));
        assert!(lean.contains("| HasProposal"));
    }

    #[test]
    fn lean_gen_has_state_structure() {
        let spec = chumsky_adapter::parse_str(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("structure State where"));
        assert!(lean.contains("creator : Pubkey"));
        assert!(lean.contains("threshold : Nat"));
        assert!(lean.contains("status : Status"));
    }

    #[test]
    fn lean_gen_has_transitions() {
        let spec = chumsky_adapter::parse_str(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("def create_vaultTransition"));
        assert!(lean.contains("signer = s.creator"));
        assert!(lean.contains("s.status = .Uninitialized"));
        assert!(lean.contains("status := .Active"));
    }

    #[test]
    fn lean_gen_has_operation_inductive() {
        let spec = chumsky_adapter::parse_str(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("inductive Operation where"));
        assert!(lean.contains("| create_vault (threshold : Nat) (member_count : Nat)"));
        assert!(lean.contains("| propose"));
        assert!(lean.contains("| approve (member_index : Nat)"));
    }

    #[test]
    fn lean_gen_has_apply_op() {
        let spec = chumsky_adapter::parse_str(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("def applyOp (s : State) (signer : Pubkey)"));
        assert!(lean.contains("| .create_vault threshold member_count => create_vaultTransition s signer threshold member_count"));
        assert!(lean.contains("| .propose => proposeTransition s signer"));
    }

    #[test]
    fn lean_gen_has_properties() {
        let spec = chumsky_adapter::parse_str(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("def threshold_bounded (s : State) : Prop :="));
        assert!(lean.contains("theorem threshold_bounded_inductive"));
        assert!(lean.contains("theorem votes_bounded_inductive"));
        // Multisig is fully auto-proven: all preservation, abort, overflow, cover,
        // and liveness theorems have mechanical proofs — no sorry markers remain.
        assert!(
            !lean.contains(":= sorry"),
            "multisig should be fully auto-proven"
        );
    }

    #[test]
    fn lean_gen_sub_auto_guard() {
        let spec = chumsky_adapter::parse_str(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        // remove_member has effect: member_count -= 1
        // Should auto-generate underflow guard: 1 ≤ s.member_count
        assert!(lean.contains("1 \u{2264} s.member_count"));
    }

    // ========================================================================
    // Multi-account (Lending) tests
    // ========================================================================

    const LENDING_SPEC: &str = include_str!("../../../examples/rust/lending/lending.qedspec");

    #[test]
    fn lean_gen_multi_per_account_status() {
        let spec = chumsky_adapter::parse_str(LENDING_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("inductive PoolStatus where"));
        assert!(lean.contains("| Uninitialized"));
        assert!(lean.contains("| Paused"));
        assert!(lean.contains("inductive LoanStatus where"));
        assert!(lean.contains("| Empty"));
        assert!(lean.contains("| Liquidated"));
    }

    #[test]
    fn lean_gen_multi_per_account_state() {
        let spec = chumsky_adapter::parse_str(LENDING_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("structure PoolState where"));
        assert!(lean.contains("  authority : Pubkey"));
        assert!(lean.contains("  total_deposits : Nat"));
        assert!(lean.contains("  status : PoolStatus"));
        assert!(lean.contains("structure LoanState where"));
        assert!(lean.contains("  borrower : Pubkey"));
        assert!(lean.contains("  status : LoanStatus"));
    }

    #[test]
    fn lean_gen_multi_transitions_use_correct_state() {
        let spec = chumsky_adapter::parse_str(LENDING_SPEC).unwrap();
        let lean = render(&spec);
        // Pool operations use PoolState
        assert!(lean.contains("def init_poolTransition (s : PoolState)"));
        assert!(lean.contains("def depositTransition (s : PoolState)"));
        // Loan operations use LoanState
        assert!(lean.contains("def borrowTransition (s : LoanState)"));
        assert!(lean.contains("def repayTransition (s : LoanState)"));
        assert!(lean.contains("def liquidateTransition (s : LoanState)"));
    }

    #[test]
    fn lean_gen_multi_per_account_operation_inductive() {
        let spec = chumsky_adapter::parse_str(LENDING_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("inductive PoolOperation where"));
        assert!(lean.contains("inductive LoanOperation where"));
        assert!(lean.contains("def applyPoolOp (s : PoolState)"));
        assert!(lean.contains("def applyLoanOp (s : LoanState)"));
    }

    #[test]
    fn lean_gen_multi_property_binds_to_correct_account() {
        let spec = chumsky_adapter::parse_str(LENDING_SPEC).unwrap();
        let lean = render(&spec);
        // pool_solvency references total_deposits/total_borrows -> binds to PoolState
        assert!(lean.contains("def pool_solvency (s : PoolState)"));
        assert!(lean.contains("theorem pool_solvency_inductive (s s' : PoolState)"));
    }

    // ========================================================================
    // sBPF (Dropset) tests — inline old-syntax spec for backward compat
    // ========================================================================

    const DROPSET_SPEC: &str = r#"
spec Dropset

pragma sbpf {
  const DISC_REGISTER_MARKET     = 0
  const ACCT_NON_DUP_MARKER      = 255
  const DATA_LEN_ZERO             = 0
  const SIZE_OF_EMPTY_ACCOUNT     = 10_336
  const SIZE_OF_MARKET_HEADER     = 40
  const SIZE_OF_ADDRESS           = 32
  const SIZE_OF_CREATE_ACCOUNT    = 56

  pubkey RENT [
    5_862_609_301_215_225_606,
    9_219_231_539_345_853_473,
    4_971_307_250_928_769_624,
    2_329_533_411
  ]

  errors [
    InvalidDiscriminant         = 1   "Discriminant is not REGISTER_MARKET",
    InvalidInstructionLength    = 2   "Instruction data is not 1 byte",
    InvalidNumberOfAccounts     = 3   "Fewer than 10 accounts provided",
    UserHasData                 = 4   "User account already has data",
    MarketAccountIsDuplicate    = 5   "Market account is a duplicate",
    MarketHasData               = 6   "Market account already has data",
    BaseMintIsDuplicate         = 7   "Base mint account is a duplicate",
    QuoteMintIsDuplicate        = 8   "Quote mint account is a duplicate",
    InvalidMarketPubkey         = 9   "Market pubkey does not match derived PDA",
    SystemProgramIsDuplicate    = 10  "System Program account is a duplicate",
    InvalidSystemProgramPubkey  = 11  "System Program pubkey is wrong",
    RentSysvarIsDuplicate       = 12  "Rent sysvar account is a duplicate",
    InvalidRentSysvarPubkey     = 13  "Rent sysvar pubkey is wrong"
  ]

  /// Validates accounts, derives market PDA, creates market account via CPI
  instruction RegisterMarket {
  discriminant DISC_REGISTER_MARKET
  entry 24

  const ACCOUNTS_REQUIRED    = 10
  const INSTRUCTION_DATA_LEN = 1

  input_layout {
    n_accounts       : U64    @ 0       "Number of accounts in input buffer"
    user_data_len    : U64    @ 88      "Data length of user account"
    market_dup       : U8     @ 10344   "Market account duplicate flag"
    market_data_len  : U64    @ 10424   "Market account data length"
    market_pubkey    : Pubkey @ 10352   "Market account address (4 chunks)"
    base_mint_dup    : U8     @ 20680   "Base mint duplicate flag"
    base_data_len    : U64    @ 20760   "Base mint data length"
  }

  insn_layout {
    insn_len         : U64    @ -8      "Instruction data length"
    discriminant     : U8     @ 0       "Instruction discriminant byte"
  }

  /// Instruction byte must be REGISTER_MARKET
  guard rejects_invalid_discriminant {
    checks discriminant == DISC_REGISTER_MARKET
    error InvalidDiscriminant
    fuel 8
  }
  guard rejects_invalid_account_count {
    checks n_accounts >= ACCOUNTS_REQUIRED
    error InvalidNumberOfAccounts
    fuel 10
  }
  guard rejects_invalid_instruction_length {
    checks insn_len == INSTRUCTION_DATA_LEN
    error InvalidInstructionLength
    fuel 12
  }
  guard rejects_user_has_data {
    checks user_data_len == DATA_LEN_ZERO
    error UserHasData
    fuel 14
  }
  guard rejects_market_duplicate {
    checks market_dup == ACCT_NON_DUP_MARKER
    error MarketAccountIsDuplicate
    fuel 16
  }
  guard rejects_market_has_data {
    checks market_data_len == DATA_LEN_ZERO
    error MarketHasData
    fuel 18
  }
  guard rejects_base_mint_duplicate {
    checks base_mint_dup == ACCT_NON_DUP_MARKER
    error BaseMintIsDuplicate
    fuel 20
  }
  guard rejects_quote_mint_duplicate {
    error QuoteMintIsDuplicate
    fuel 30
  }
  guard rejects_invalid_market_pubkey {
    checks market_pubkey == derived_pda
    error InvalidMarketPubkey
    fuel 61
  }
  guard rejects_system_program_duplicate {
    error SystemProgramIsDuplicate
    fuel 74
  }
  guard rejects_invalid_system_program_pubkey {
    error InvalidSystemProgramPubkey
    fuel 86
  }
  guard rejects_rent_sysvar_duplicate {
    error RentSysvarIsDuplicate
    fuel 96
  }
  guard rejects_invalid_rent_sysvar_pubkey {
    checks rent_pubkey == RENT
    error InvalidRentSysvarPubkey
    fuel 108
  }

  property memory_safety {
    scope guards
  }
  property pda_derivation {
    flow market_pda from seeds [base_mint_addr, quote_mint_addr]
  }
  property account_pointer_flow {
    flow r9 through [market, system_program, rent_sysvar]
  }
  property cpi_create_account {
    cpi system_program CreateAccount {
      payer        user
      target       market_pda
      space        SIZE_OF_MARKET_HEADER
      signer_seeds [base_mint_addr, quote_mint_addr, bump]
    }
  }
  property accepts_valid_input {
    after all guards
    exit 0
  }
}
}
"#;

    #[test]
    fn lean_gen_sbpf_routes_to_sbpf_renderer() {
        let spec = chumsky_adapter::parse_str(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Should use sBPF imports, not state-machine imports
        assert!(lean.contains("open QEDGen.Solana.SBPF"));
        assert!(lean.contains("import QEDGen"));
        assert!(!lean.contains("structure State where"));
    }

    #[test]
    fn lean_gen_sbpf_namespace() {
        let spec = chumsky_adapter::parse_str(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("namespace RegisterMarket"));
        assert!(lean.contains("end RegisterMarket"));
    }

    #[test]
    fn lean_gen_sbpf_constants() {
        let spec = chumsky_adapter::parse_str(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Global constants are emitted as comments (avoid conflict with prog module)
        assert!(lean.contains("--   DISC_REGISTER_MARKET = 0"));
        assert!(lean.contains("--   ACCT_NON_DUP_MARKER = 255"));
        assert!(lean.contains("--   DATA_LEN_ZERO = 0"));
        // Instruction-level constants ARE emitted as abbrevs
        assert!(lean.contains("abbrev ACCOUNTS_REQUIRED : Nat := 10"));
        assert!(lean.contains("abbrev INSTRUCTION_DATA_LEN : Nat := 1"));
    }

    #[test]
    fn lean_gen_sbpf_pubkey_chunks() {
        let spec = chumsky_adapter::parse_str(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Pubkey chunks are emitted as comments (avoid conflict with prog module)
        assert!(lean.contains("--   PUBKEY_RENT_CHUNK_0 = 5862609301215225606"));
        assert!(lean.contains("--   PUBKEY_RENT_CHUNK_1 = 9219231539345853473"));
        assert!(lean.contains("--   PUBKEY_RENT_CHUNK_2 = 4971307250928769624"));
        assert!(lean.contains("--   PUBKEY_RENT_CHUNK_3 = 2329533411"));
    }

    #[test]
    fn lean_gen_sbpf_error_constants() {
        let spec = chumsky_adapter::parse_str(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Error constants emitted as abbrevs in instruction namespace
        assert!(lean.contains("abbrev E_INVALID_DISCRIMINANT : Nat := 1"));
        assert!(lean.contains("abbrev E_INVALID_NUMBER_OF_ACCOUNTS : Nat := 3"));
        assert!(lean.contains("abbrev E_MARKET_ACCOUNT_IS_DUPLICATE : Nat := 5"));
        assert!(lean.contains("abbrev E_INVALID_RENT_SYSVAR_PUBKEY : Nat := 13"));
    }

    #[test]
    fn lean_gen_sbpf_offset_constants_and_ea_lemmas() {
        let spec = chumsky_adapter::parse_str(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Offset constants
        assert!(lean.contains("abbrev N_ACCOUNTS : Int := 0"));
        assert!(lean.contains("abbrev USER_DATA_LEN : Int := 88"));
        assert!(lean.contains("abbrev MARKET_DUP : Int := 10344"));
        assert!(lean.contains("abbrev MARKET_PUBKEY : Int := 10352"));
        // ea_* lemmas
        assert!(lean
            .contains("@[simp] theorem ea_N_ACCOUNTS (b : Nat) : effectiveAddr b N_ACCOUNTS = b"));
        assert!(lean.contains(
            "@[simp] theorem ea_USER_DATA_LEN (b : Nat) : effectiveAddr b USER_DATA_LEN = b + 88"
        ));
        // Negative offset for insn_layout
        assert!(lean.contains("abbrev INSN_LEN : Int := -8"));
        assert!(lean
            .contains("@[simp] theorem ea_INSN_LEN (b : Nat) : effectiveAddr b INSN_LEN = b - 8"));
    }

    #[test]
    fn lean_gen_sbpf_guard_theorems() {
        let spec = chumsky_adapter::parse_str(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // P1: discriminant check — field "discriminant" → var "discriminant"
        assert!(lean.contains("theorem rejects_invalid_discriminant"));
        assert!(lean.contains("h_discriminant_ne : discriminant ≠ DISC_REGISTER_MARKET"));
        assert!(lean.contains("= some E_INVALID_DISCRIMINANT"));
        // P2: account count check — field "n_accounts" → var "nAccounts"
        assert!(lean.contains("theorem rejects_invalid_account_count"));
        assert!(lean.contains("h_nAccounts_lt : nAccounts < ACCOUNTS_REQUIRED"));
        // P5: market duplicate check (should have accumulated hypotheses from P1-P4)
        assert!(lean.contains("theorem rejects_market_duplicate"));
        assert!(lean.contains("= some E_MARKET_ACCOUNT_IS_DUPLICATE"));
    }

    #[test]
    fn lean_gen_sbpf_hypothesis_accumulation() {
        let spec = chumsky_adapter::parse_str(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // P2 (rejects_invalid_account_count) should have after-hypothesis from P1
        // The after-hyp from P1 is: readU8 at insn addr = DISC_REGISTER_MARKET
        let p2_section = lean
            .split("theorem rejects_invalid_account_count")
            .nth(1)
            .unwrap()
            .split("theorem ")
            .next()
            .unwrap();
        assert!(p2_section.contains("h_disc"));
    }

    #[test]
    fn lean_gen_sbpf_spec_structure() {
        let spec = chumsky_adapter::parse_str(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("structure Spec (progAt : Nat → Option Insn) where"));
        // Should have a field for each guard
        assert!(lean.contains("  rejects_invalid_discriminant :"));
        assert!(lean.contains("  rejects_market_duplicate :"));
        assert!(lean.contains("  rejects_invalid_rent_sysvar_pubkey :"));
    }

    #[test]
    fn lean_gen_sbpf_property_stubs() {
        let spec = chumsky_adapter::parse_str(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("theorem memory_safety : True := trivial"));
        assert!(lean.contains("theorem pda_derivation : True := trivial"));
        assert!(lean.contains("theorem account_pointer_flow : True := trivial"));
        assert!(lean.contains("theorem cpi_create_account : True := trivial"));
        assert!(lean.contains("theorem accepts_valid_input : True := trivial"));
    }

    #[test]
    fn lean_gen_sbpf_initstate2_for_two_pointer() {
        let spec = chumsky_adapter::parse_str(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Dropset has insn_layout, so should use initState2
        assert!(lean.contains("initState2 inputAddr insnAddr mem"));
    }

    #[test]
    fn lean_gen_sbpf_entry_point() {
        let spec = chumsky_adapter::parse_str(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Dropset entry is 24
        assert!(lean.contains("initState2 inputAddr insnAddr mem 24"));
    }

    // ========================================================================
    // v2.0 feature tests
    // ========================================================================

    const PERCOLATOR_SPEC: &str =
        include_str!("../../../examples/rust/percolator/percolator.qedspec");

    #[test]
    fn lean_gen_proof_decomposition_sub_lemmas() {
        let spec = chumsky_adapter::parse_str(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        // Per-operation sub-lemmas for threshold_bounded
        assert!(lean.contains("theorem threshold_bounded_preserved_by_create_vault"));
        assert!(lean.contains("theorem threshold_bounded_preserved_by_propose"));
        assert!(lean.contains("theorem threshold_bounded_preserved_by_approve"));
        // Sub-lemmas have sorry
        assert!(lean.contains("threshold_bounded_preserved_by_create_vault"));
        // Master theorem uses exact
        assert!(lean.contains("exact threshold_bounded_preserved_by_create_vault"));
    }

    #[test]
    fn lean_gen_aborts_if_theorems() {
        let spec = chumsky_adapter::parse_str(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("theorem create_vault_aborts_if_InvalidThreshold"));
        assert!(lean.contains("theorem create_vault_aborts_if_TooManyMembers"));
        assert!(lean.contains("theorem approve_aborts_if_NotAMember"));
        assert!(lean.contains("theorem execute_aborts_if_ThresholdNotMet"));
        // Requires-based aborts are auto-proven via if_neg projection
        assert!(lean.contains("rw [if_neg"));
    }

    #[test]
    fn lean_gen_cover_theorems() {
        let spec = chumsky_adapter::parse_str(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("theorem cover_proposal_lifecycle"));
        assert!(lean.contains("theorem cover_rejection_flow"));
        // Should be existential proofs with auto-generated witnesses
        assert!(lean.contains("∃ (s0 : State) (signer : Pubkey)"));
        // Covers are auto-proven with concrete witnesses via `by decide`
        assert!(lean.contains("by decide"));
        assert!(lean.contains("let pk : Pubkey"));
    }

    #[test]
    fn lean_gen_does_not_emit_liveness_in_spec() {
        // Liveness obligations are user-owned in Proofs.lean — durability
        // comes from scaffold-once codegen + compile-time spec-hash drift
        // detection via the `#[qed(verified, spec = ...)]` macro. Spec.lean
        // must stay codegen-owned.
        let spec = chumsky_adapter::parse_str(PERCOLATOR_SPEC).unwrap();
        let lean = render(&spec);
        assert!(!lean.contains("theorem liveness_drain_completes"));
    }

    #[test]
    fn lean_gen_overflow_obligations() {
        let spec = chumsky_adapter::parse_str(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        // approve has an add effect (approval_count += 1)
        assert!(lean.contains("theorem approve_overflow_safe"));
        assert!(lean.contains("valid_u"));
    }

    #[test]
    fn lean_gen_multi_aborts_if() {
        let spec = chumsky_adapter::parse_str(LENDING_SPEC).unwrap();
        let lean = render(&spec);
        // Pool ops: init_pool and deposit have aborts_if
        assert!(lean.contains("theorem init_pool_aborts_if_InvalidAmount"));
        assert!(lean.contains("theorem deposit_aborts_if_InvalidAmount"));
        // Loan ops: borrow has aborts_if
        assert!(lean.contains("theorem borrow_aborts_if_InvalidAmount"));
    }

    #[test]
    fn lean_gen_multi_environment() {
        let spec = chumsky_adapter::parse_str(LENDING_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("theorem pool_solvency_under_interest_rate_change"));
        assert!(lean.contains("new_interest_rate"));
        assert!(lean.contains("{ s with interest_rate := new_interest_rate }"));
    }

    #[test]
    fn lean_gen_sum_type_inductive() {
        // A sum type used as a Map value should render as a proper Lean
        // `inductive` with a separate `structure` per payload-carrying variant,
        // rather than the flattened-with-status treatment used for State.
        let src = r#"
spec SumDemo

const MAX_SLOTS = 8

type AccountIdx = Fin[MAX_SLOTS]

type Slot
  | Empty
  | Filled of {
      count : U64,
    }

type State
  | Active of {
      authority : Pubkey,
      slots     : Map[MAX_SLOTS] Slot,
    }
"#;
        let spec = chumsky_adapter::parse_str(src).unwrap();
        let lean = render(&spec);
        // Payload structure
        assert!(
            lean.contains("structure SlotFilledData where"),
            "missing SlotFilledData; got:\n{}",
            &lean[..lean.len().min(2000)]
        );
        // Inductive
        assert!(
            lean.contains("inductive Slot where"),
            "missing Slot inductive"
        );
        assert!(
            lean.contains("| Empty") && lean.contains("| Filled (d : SlotFilledData)"),
            "missing Slot variants"
        );
        // Inhabited
        assert!(
            lean.contains("instance : Inhabited Slot := \u{27E8}.Empty\u{27E9}"),
            "missing Inhabited Slot"
        );
    }

    // Regression: issue #8 finding #2 — a cond_part containing a top-level
    // `∨` / `→` / `↔` must be parenthesized before being `∧`-joined, else
    // Lean parses `A ∧ B ∨ C` as `(A ∧ B) ∨ C` and the generated theorem
    // projections (`hg.2.1` etc.) don't typecheck.
    #[test]
    fn paren_if_low_prec_wraps_top_level_or() {
        assert_eq!(
            paren_if_low_prec("side = 0 \u{2228} side = 1"),
            "(side = 0 \u{2228} side = 1)"
        );
    }

    #[test]
    fn paren_if_low_prec_wraps_top_level_implies() {
        assert_eq!(
            paren_if_low_prec("a = 1 \u{2192} b = 2"),
            "(a = 1 \u{2192} b = 2)"
        );
    }

    #[test]
    fn paren_if_low_prec_wraps_top_level_iff() {
        assert_eq!(
            paren_if_low_prec("a = 1 \u{2194} b = 2"),
            "(a = 1 \u{2194} b = 2)"
        );
    }

    #[test]
    fn paren_if_low_prec_leaves_pure_conjunction_alone() {
        // ∧ binds tighter than the ∧-join, no wrap needed.
        assert_eq!(
            paren_if_low_prec("a = 1 \u{2227} b = 2"),
            "a = 1 \u{2227} b = 2"
        );
    }

    #[test]
    fn paren_if_low_prec_leaves_simple_equality_alone() {
        assert_eq!(paren_if_low_prec("s.a = 0"), "s.a = 0");
    }

    #[test]
    fn paren_if_low_prec_respects_paren_nesting() {
        // ∨ is already inside parens → no double-wrap.
        assert_eq!(
            paren_if_low_prec("(a = 0 \u{2228} a = 1) \u{2227} b = 2"),
            "(a = 0 \u{2228} a = 1) \u{2227} b = 2"
        );
    }

    // Issue #8 finding #1 regression. Before the fix, `U16` leaked
    // through as the DSL type name, producing Lake's
    // "universe level metavariables" error. Now map_type covers every
    // primitive the Rust side does.
    #[test]
    fn finding_1_u16_lowers_to_nat() {
        let spec_src =
            include_str!("../../../examples/regressions/issue-8/repro-01-u16-type.qedspec");
        let spec = chumsky_adapter::parse_str(spec_src).unwrap();
        let lean = render(&spec);
        assert!(
            lean.contains("mm_count : Nat"),
            "expected U16 param to lower to Nat, got:\n{}",
            lean
        );
        assert!(
            !lean.contains("mm_count : U16"),
            "U16 leaked through — fix regressed:\n{}",
            lean
        );
    }

    // Map parity: every primitive the Rust side maps must have a Lean
    // mapping too. The string-level check here catches the class of
    // drift (finding #1) without running through full render.
    #[test]
    fn map_type_covers_all_signed_and_unsigned_primitives() {
        for unsigned in ["U8", "U16", "U32", "U64", "U128"] {
            assert_eq!(
                super::map_type(unsigned),
                "Nat",
                "unsigned {unsigned} should map to Nat"
            );
        }
        for signed in ["I8", "I16", "I32", "I64", "I128"] {
            assert_eq!(
                super::map_type(signed),
                "Int",
                "signed {signed} should map to Int"
            );
        }
    }

    // Issue #8 finding #3 regression. Two `requires X else SameErr`
    // previously collided at `h_aborts_if_SameErr`; now they get
    // positional suffixes `_0` / `_1`.
    #[test]
    fn finding_3_duplicate_error_theorems_uniquify() {
        let spec_src = include_str!(
            "../../../examples/regressions/issue-8/repro-03-duplicate-theorem.qedspec"
        );
        let spec = chumsky_adapter::parse_str(spec_src).unwrap();
        let lean = render(&spec);
        assert!(
            lean.contains("theorem h_aborts_if_SameErr_0"),
            "expected _0 suffix, got:\n{}",
            lean
        );
        assert!(
            lean.contains("theorem h_aborts_if_SameErr_1"),
            "expected _1 suffix, got:\n{}",
            lean
        );
        // Count plain (no-suffix) occurrences — should be zero.
        let plain_count = lean
            .matches("theorem h_aborts_if_SameErr (")
            .count();
        assert_eq!(
            plain_count, 0,
            "unsuffixed theorem name leaked through:\n{}",
            lean
        );
    }

    // Parity: when an error appears only once, no suffix should
    // be added (avoids churning every existing example).
    #[test]
    fn finding_3_unique_error_keeps_bare_name() {
        // Uses the repro-02 fixture: two requires, DIFFERENT errors.
        let spec_src = include_str!(
            "../../../examples/regressions/issue-8/repro-02-composite-or-parens.qedspec"
        );
        let spec = chumsky_adapter::parse_str(spec_src).unwrap();
        let lean = render(&spec);
        assert!(
            lean.contains("theorem h_aborts_if_E1 "),
            "expected bare E1, got:\n{}",
            lean
        );
        assert!(
            lean.contains("theorem h_aborts_if_E2 "),
            "expected bare E2, got:\n{}",
            lean
        );
    }

    // Issue #8 finding #2 regression. Runs against the exact fixture
    // shipped in the gist, so fix drift would surface as test failure.
    #[test]
    fn finding_2_requires_with_or_is_parenthesized() {
        let spec_src = include_str!(
            "../../../examples/regressions/issue-8/repro-02-composite-or-parens.qedspec"
        );
        let spec = chumsky_adapter::parse_str(spec_src).unwrap();
        let lean = render(&spec);
        assert!(
            lean.contains("(side = 0 \u{2228} side = 1)"),
            "expected paren-wrapped disjunction, got:\n{}",
            lean
        );
        assert!(
            !lean.contains("\u{2227} side = 0 \u{2228} side = 1"),
            "raw ∧ adjacent to unwrapped ∨ — fix regressed:\n{}",
            lean
        );
    }

    // Smoke test: every issue-8 fixture must parse cleanly. This is a
    // floor, not a ceiling — individual-finding regression tests add
    // stronger assertions. But a parse break on any of these files
    // (e.g. someone rejects a DSL shape real specs use) surfaces loudly
    // here without needing a per-fixture test.
    #[test]
    fn issue_8_fixtures_all_parse() {
        for (name, src) in ISSUE_8_FIXTURES {
            let parsed = chumsky_adapter::parse_str(src);
            assert!(
                parsed.is_ok(),
                "fixture {} failed to parse: {:?}",
                name,
                parsed.err()
            );
        }
    }

    // Render-smoke: every fixture must also make it through `render`
    // without panic. Guarantees codegen changes don't silently regress
    // a fixture from "produces wrong Lean" to "produces no Lean at
    // all" — a subtler failure mode that per-finding tests wouldn't
    // catch if they only inspect the output string for a known pattern.
    #[test]
    fn issue_8_fixtures_all_render() {
        for (name, src) in ISSUE_8_FIXTURES {
            let spec = chumsky_adapter::parse_str(src)
                .unwrap_or_else(|e| panic!("fixture {} failed to parse: {:?}", name, e));
            let _ = render(&spec);
        }
    }
}
