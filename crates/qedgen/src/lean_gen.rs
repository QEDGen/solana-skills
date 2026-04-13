//! Generate Lean 4 source from a `ParsedSpec`.
//!
//! Replaces the Lean elaborator as the source of truth when using `.qedspec` files.
//! Produces the same structures: State, Status, transitions, Operation inductive,
//! applyOp, CPI theorems, property predicates, and inductive preservation theorems.

use anyhow::Result;
use std::path::Path;

use crate::check::ParsedSpec;

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
    // sBPF mode: target is "assembly", or assembly_path is set with instructions
    if spec.target.as_deref() == Some("assembly")
        || (spec.assembly_path.is_some() && !spec.instructions.is_empty())
    {
        return render_sbpf(spec);
    }

    let is_multi_account = spec.account_types.len() > 1;

    if is_multi_account {
        render_multi_account(spec)
    } else {
        render_single_account(spec)
    }
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
        out.push_str("inductive Status where\n");
        for s in &spec.lifecycle_states {
            out.push_str(&format!("  | {}\n", s));
        }
        out.push_str("  deriving Repr, DecidableEq, BEq\n\n");
    }

    // State structure
    out.push_str("structure State where\n");
    for (fname, ftype) in &spec.state_fields {
        out.push_str(&format!("  {} : {}\n", safe_name(fname), map_type(ftype)));
    }
    if has_lifecycle {
        out.push_str("  status : Status\n");
    }
    out.push_str("  deriving Repr, DecidableEq, BEq\n\n");

    // Transition functions
    let ops_refs: Vec<&crate::check::ParsedOperation> = spec.operations.iter().collect();
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
            "/-- Invariant: {}. -/\ntheorem {} : True := sorry\n\n",
            inv_name, inv_name
        ));
    }

    // Operation inductive + applyOp
    render_operation_inductive(&mut out, &ops_refs, "State");

    // Property predicates and inductive theorems
    render_properties(&mut out, &spec.properties, &ops_refs, "State");

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
        let status_name = format!("{}Status", acct_name);
        let state_name = format!("{}State", acct_name);

        // Status inductive
        if !acct.lifecycle.is_empty() {
            out.push_str(&format!("inductive {} where\n", status_name));
            for s in &acct.lifecycle {
                out.push_str(&format!("  | {}\n", s));
            }
            out.push_str("  deriving Repr, DecidableEq, BEq\n\n");
        }

        // State structure
        out.push_str(&format!("structure {} where\n", state_name));
        for (fname, ftype) in &acct.fields {
            out.push_str(&format!("  {} : {}\n", safe_name(fname), map_type(ftype)));
        }
        if !acct.lifecycle.is_empty() {
            out.push_str(&format!("  status : {}\n", status_name));
        }
        out.push_str("  deriving Repr, DecidableEq, BEq\n\n");

        // Operations targeting this account
        let ops: Vec<&crate::check::ParsedOperation> = spec
            .operations
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
            &ops_to_owned(&ops),
            &acct.fields,
            &state_name,
            &status_name,
        );

        // CPI theorems
        render_cpi_theorems(&mut out, &ops_to_owned(&ops));

        // Operation inductive + applyOp per account
        render_operation_inductive(&mut out, &ops_to_owned(&ops), &state_name);
    }

    // Invariants
    for (inv_name, _desc) in &spec.invariants {
        out.push_str(&format!(
            "/-- Invariant: {}. -/\ntheorem {} : True := sorry\n\n",
            inv_name, inv_name
        ));
    }

    // Properties — for multi-account, reference the state type from the first account
    // that has matching fields. Properties using `state.X` bind to the account whose
    // fields contain X.
    render_properties_multi(&mut out, spec);

    out.push_str(&format!("end {}\n", name));
    out
}

/// Helper to convert &[&ParsedOperation] to Vec<ParsedOperation> references.
/// We can't easily change the signatures, so this just helps with the borrow.
fn ops_to_owned<'a>(
    ops: &[&'a crate::check::ParsedOperation],
) -> Vec<&'a crate::check::ParsedOperation> {
    ops.to_vec()
}

/// Render transition functions for a set of operations.
fn render_transitions(
    out: &mut String,
    spec: &ParsedSpec,
    ops: &[&crate::check::ParsedOperation],
    fields: &[(String, String)],
    state_type: &str,
    _status_type: &str,
) {
    for op in ops {
        let trans_name = safe_name(&format!("{}Transition", op.name));
        let param_sig = param_sig_str(&op.takes_params);

        // Build condition parts
        let mut cond_parts: Vec<String> = Vec::new();
        if let Some(ref who) = op.who {
            cond_parts.push(format!("signer = s.{}", safe_name(who)));
        }
        if let Some(ref pre) = op.pre_status {
            cond_parts.push(format!("s.status = .{}", pre));
        }
        // Auto-guards for sub effects
        for (field, op_kind, _value) in &op.effects {
            if op_kind == "sub" {
                let ftype = fields
                    .iter()
                    .find(|(n, _)| n == field)
                    .or_else(|| spec.state_fields.iter().find(|(n, _)| n == field))
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

        let has_cond = !cond_parts.is_empty();
        let if_cond = cond_parts.join(" \u{2227} "); // ∧

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

        if has_cond {
            out.push_str(&format!("  if {} then\n", if_cond));
            out.push_str(&format!("    {}\n", then_body));
            out.push_str("  else none\n\n");
        } else {
            out.push_str(&format!("  {}\n\n", then_body));
        }
    }
}

/// Render CPI correctness theorems.
fn render_cpi_theorems(out: &mut String, ops: &[&crate::check::ParsedOperation]) {
    for op in ops {
        if !op.has_calls {
            continue;
        }
        let cpi_program = op.program_id.as_deref().unwrap_or("UNKNOWN_PROGRAM");
        let disc = op.calls_discriminator.as_deref().unwrap_or("DISC_UNKNOWN");
        let ctx_name = safe_name(&format!("{}CpiContext", op.name));

        // CPI context structure
        out.push_str(&format!("structure {} where\n", ctx_name));
        for (acct, _) in &op.calls_accounts {
            out.push_str(&format!("  {} : Pubkey\n", acct));
        }
        out.push_str("  deriving Repr, DecidableEq, BEq\n\n");

        // build_cpi function
        let build_name = safe_name(&format!("{}_build_cpi", op.name));
        out.push_str(&format!(
            "def {} (ctx : {}) : CpiInstruction :=\n",
            build_name, ctx_name
        ));
        out.push_str(&format!("  {{ programId := {}\n", cpi_program));
        out.push_str("  , accounts := [");
        for (i, (acct, flag)) in op.calls_accounts.iter().enumerate() {
            let (is_signer, is_writable) = parse_flag(flag);
            if i > 0 {
                out.push_str(",\n      ");
            }
            out.push_str(&format!(
                "\u{27E8}ctx.{}, {}, {}\u{27E9}",
                acct, is_signer, is_writable
            ));
        }
        out.push_str("]\n");
        out.push_str(&format!("  , data := {} }}\n\n", disc));

        // cpi_correct theorem
        let op_name = safe_name(&op.name);
        out.push_str(&format!(
            "/-- {} CPI targets {} with correct accounts and discriminator. -/\n",
            op.name, cpi_program
        ));
        out.push_str(&format!(
            "theorem {}.cpi_correct (ctx : {}) :\n    let cpi := {} ctx\n    ",
            op_name, ctx_name, build_name
        ));
        let mut parts: Vec<String> = vec![format!("targetsProgram cpi {}", cpi_program)];
        for (i, (acct, flag)) in op.calls_accounts.iter().enumerate() {
            let (is_signer, is_writable) = parse_flag(flag);
            parts.push(format!(
                "accountAt cpi {} ctx.{} {} {}",
                i, acct, is_signer, is_writable
            ));
        }
        parts.push(format!("hasDiscriminator cpi {}", disc));
        out.push_str(&parts.join(" \u{2227}\n    "));
        out.push_str(" := by\n");
        out.push_str(&format!(
            "  unfold {} targetsProgram accountAt hasDiscriminator\n",
            build_name
        ));
        out.push_str("  exact \u{27E8}rfl, ");
        let rfls: Vec<&str> = (0..parts.len() - 1).map(|_| "rfl").collect();
        out.push_str(&rfls.join(", "));
        out.push_str("\u{27E9}\n\n");
    }
}

/// Render Operation inductive and applyOp dispatcher.
fn render_operation_inductive(
    out: &mut String,
    ops: &[&crate::check::ParsedOperation],
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
    ops: &[&crate::check::ParsedOperation],
    state_type: &str,
) {
    render_properties_inner(out, properties, ops, state_type, "Operation", "applyOp");
}

/// Render properties for multi-account specs.
fn render_properties_multi(out: &mut String, spec: &ParsedSpec) {
    // For each property, determine which account's state type to use.
    // Heuristic: look at the expression's `s.field` references and match against account fields.
    for prop in &spec.properties {
        // Find which account type this property's fields belong to
        let target_acct = if let Some(ref expr) = prop.expression {
            spec.account_types.iter().find(|a| {
                a.fields
                    .iter()
                    .any(|(f, _)| expr.contains(&format!("s.{}", f)))
            })
        } else {
            None
        };

        let (state_type, op_type, apply_name) = if let Some(acct) = target_acct {
            (
                format!("{}State", acct.name),
                format!("{}Operation", acct.name),
                format!("apply{}Op", acct.name),
            )
        } else {
            // Default to first account
            let acct = &spec.account_types[0];
            (
                format!("{}State", acct.name),
                format!("{}Operation", acct.name),
                format!("apply{}Op", acct.name),
            )
        };

        if let Some(ref expr) = prop.expression {
            out.push_str(&format!(
                "def {} (s : {}) : Prop := {}\n\n",
                prop.name, state_type, expr
            ));
        }

        // Get operations for this account type
        let target_name = target_acct
            .map(|a| a.name.as_str())
            .unwrap_or(&spec.account_types[0].name);
        let acct_ops: Vec<&crate::check::ParsedOperation> = spec
            .operations
            .iter()
            .filter(|op| {
                op.on_account.as_deref() == Some(target_name)
                    || (op.on_account.is_none() && target_name == spec.account_types[0].name)
            })
            .collect();

        if !acct_ops.is_empty() {
            out.push_str(&format!(
                "/-- {} is preserved by every operation. Prove by `cases op` with unfold/omega per case. -/\n",
                prop.name
            ));
            out.push_str(&format!(
                "theorem {}_inductive (s s' : {}) (signer : Pubkey) (op : {})\n    (h_inv : {} s) (h : {} s signer op = some s') : {} s' := sorry\n\n",
                prop.name, state_type, op_type, prop.name, apply_name, prop.name
            ));
        }
    }
}

/// Inner helper for property rendering.
fn render_properties_inner(
    out: &mut String,
    properties: &[crate::check::ParsedProperty],
    _ops: &[&crate::check::ParsedOperation],
    state_type: &str,
    op_type: &str,
    apply_name: &str,
) {
    for prop in properties {
        if let Some(ref expr) = prop.expression {
            out.push_str(&format!(
                "def {} (s : {}) : Prop := {}\n\n",
                prop.name, state_type, expr
            ));
        }

        out.push_str(&format!(
            "/-- {} is preserved by every operation. Prove by `cases op` with unfold/omega per case. -/\n",
            prop.name
        ));
        out.push_str(&format!(
            "theorem {}_inductive (s s' : {}) (signer : Pubkey) (op : {})\n    (h_inv : {} s) (h : {} s signer op = some s') : {} s' := sorry\n\n",
            prop.name, state_type, op_type, prop.name, apply_name, prop.name
        ));
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

    let instr = &spec.instructions[0]; // Currently single-instruction
    let ns = &instr.name;

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

    out.push_str(&format!("namespace {}\n\n", ns));

    // ── Global constants ─────────────────────────────────────────────────
    // For sBPF specs with assembly_path, global constants (e.g., DISC_REGISTER_MARKET)
    // are already defined in the prog module. We emit them as comments only.
    // Instruction-level constants (e.g., ACCOUNTS_REQUIRED) are spec-specific
    // and must be declared here.
    if !spec.constants.is_empty() {
        out.push_str("-- Global constants (from prog module, not re-declared):\n");
        for (name, val) in &spec.constants {
            let clean_val = val.replace('_', "");
            out.push_str(&format!("--   {} = {}\n", name, clean_val));
        }
        out.push('\n');
    }
    if !instr.constants.is_empty() {
        out.push_str("-- Instruction-level constants\n");
        for (name, val) in &instr.constants {
            let clean_val = val.replace('_', "");
            out.push_str(&format!("abbrev {} : Nat := {}\n", name, clean_val));
        }
        out.push('\n');
    }

    // ── Pubkey constants ───────────────────────────────────────────────────
    // For sBPF, pubkey chunk constants are also in the prog module.
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

    // ── Error constants ──────────────────────────────────────────────────
    // Emitted as abbrevs in the instruction namespace. For sBPF specs, these
    // may shadow same-named constants from the prog module — proofs that open
    // both namespaces should use `hiding` on one to avoid ambiguity.
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

    // ── Offset constants (from input_layout + insn_layout) ───────────────
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

    // ── Entry point ──────────────────────────────────────────────────────
    let entry = instr.entry.unwrap_or(0);
    let has_insn_reg = instr.insn_layout.iter().any(|_| true);
    let init_expr = if has_insn_reg {
        format!("initState2 inputAddr insnAddr mem {}", entry)
    } else {
        "initState inputAddr mem".to_string()
    };

    // ── Guard theorem stubs ──────────────────────────────────────────────
    out.push_str("-- Guard theorem stubs\n");
    out.push_str("-- Hypotheses derived from checks + layout. Fill proofs with wp_exec.\n\n");

    // Build accumulated after-hypotheses for the Spec structure
    let mut accumulated_after: Vec<(String, String)> = Vec::new(); // (var_name, type_expr)
                                                                   // Track cumulative fuel for each guard
    let mut _cumulative_fuel: u64 = 0;

    for (i, guard) in instr.guards.iter().enumerate() {
        let error_lean = error_to_lean_name(&guard.error);

        // Derive hypotheses from checks expression + layout
        let hyps = derive_guard_hypotheses(guard, &all_offsets, instr, spec);

        // Doc comment
        if let Some(ref doc) = guard.doc {
            out.push_str(&format!("/-- {} -/\n", doc.trim()));
        }

        // Theorem signature
        out.push_str(&format!("theorem {}\n", guard.name));

        // Base parameters
        if has_insn_reg {
            out.push_str("    (inputAddr insnAddr : Nat) (mem : Mem)\n");
        } else {
            out.push_str("    (inputAddr : Nat) (mem : Mem)\n");
        }

        // Accumulated after-hypotheses from prior guards
        for (var_decl, _) in &accumulated_after {
            out.push_str(&format!("    {}\n", var_decl));
        }

        // This guard's hypotheses
        for hyp in &hyps.bindings {
            out.push_str(&format!("    {}\n", hyp));
        }

        // Conclusion — use per-guard fuel if specified, else FUEL placeholder
        let fuel_str = match guard.fuel {
            Some(f) => f.to_string(),
            None => "FUEL".to_string(),
        };
        out.push_str(&format!(
            "    :\n    (executeFn {}.progAt ({}) {}).exitCode\n      \
             = some {} := sorry\n\n",
            prog_module, init_expr, fuel_str, error_lean
        ));

        // Accumulate after-hypotheses for next guards
        if let Some(ref after_hyps) = hyps.after {
            for ah in after_hyps {
                accumulated_after.push((ah.clone(), String::new()));
            }
        }

        let _ = i; // suppress unused
    }

    // ── Spec completeness structure ──────────────────────────────────────
    out.push_str("-- Completeness structure: fill all fields to prove every guard is covered\n");
    out.push_str("structure Spec (progAt : Nat \u{2192} Option Insn) where\n");

    // Reset accumulation for structure fields
    let mut acc_after_for_spec: Vec<String> = Vec::new();
    for guard in &instr.guards {
        let error_lean = error_to_lean_name(&guard.error);
        let hyps = derive_guard_hypotheses(guard, &all_offsets, instr, spec);

        // Build the universal quantifier binders
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
            // Skip TODO comments — they break ∀ binder syntax
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

    // ── Property theorem stubs ───────────────────────────────────────────
    if !instr.properties.is_empty() {
        out.push_str("-- Property theorem stubs\n\n");
        for prop in &instr.properties {
            if let Some(ref doc) = prop.doc {
                out.push_str(&format!("/-- {} -/\n", doc.trim()));
            }
            out.push_str(&format!("theorem {} : True := trivial\n\n", prop.name));
        }
    }

    out.push_str(&format!("end {}\n", ns));
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
fn map_type(t: &str) -> &str {
    match t {
        "U64" | "U128" | "U8" => "Nat",
        "I128" => "Int",
        _ => t,
    }
}

/// Quote Lean keywords as «name».
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

/// Parse account flag to (isSigner, isWritable).
fn parse_flag(flag: &str) -> (&str, &str) {
    match flag {
        "readonly" => ("false", "false"),
        "writable" => ("false", "true"),
        "signer" => ("true", "false"),
        "signer_writable" => ("true", "true"),
        _ => ("false", "false"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;

    const MULTISIG_SPEC: &str =
        include_str!("../../../examples/rust/multisig/formal_verification/multisig.qedspec");

    #[test]
    fn lean_gen_has_namespace() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("namespace Multisig"));
        assert!(lean.contains("end Multisig"));
    }

    #[test]
    fn lean_gen_has_status_inductive() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("inductive Status where"));
        assert!(lean.contains("| Uninitialized"));
        assert!(lean.contains("| Active"));
        assert!(lean.contains("| HasProposal"));
    }

    #[test]
    fn lean_gen_has_state_structure() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("structure State where"));
        assert!(lean.contains("creator : Pubkey"));
        assert!(lean.contains("threshold : Nat"));
        assert!(lean.contains("status : Status"));
    }

    #[test]
    fn lean_gen_has_transitions() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("def create_vaultTransition"));
        assert!(lean.contains("signer = s.creator"));
        assert!(lean.contains("s.status = .Uninitialized"));
        assert!(lean.contains("status := .Active"));
    }

    #[test]
    fn lean_gen_has_operation_inductive() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("inductive Operation where"));
        assert!(lean.contains("| create_vault (threshold : Nat) (member_count : Nat)"));
        assert!(lean.contains("| propose"));
        assert!(lean.contains("| approve (member_index : Nat)"));
    }

    #[test]
    fn lean_gen_has_apply_op() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("def applyOp (s : State) (signer : Pubkey)"));
        assert!(lean.contains("| .create_vault threshold member_count => create_vaultTransition s signer threshold member_count"));
        assert!(lean.contains("| .propose => proposeTransition s signer"));
    }

    #[test]
    fn lean_gen_has_properties() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("def threshold_bounded (s : State) : Prop :="));
        assert!(lean.contains("theorem threshold_bounded_inductive"));
        assert!(lean.contains("theorem approvals_bounded_inductive"));
        assert!(lean.contains(":= sorry"));
    }

    #[test]
    fn lean_gen_sub_auto_guard() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        // remove_member has effect: member_count -= 1
        // Should auto-generate underflow guard: 1 ≤ s.member_count
        assert!(lean.contains("1 \u{2264} s.member_count"));
    }

    // ========================================================================
    // Multi-account (Lending) tests
    // ========================================================================

    const LENDING_SPEC: &str =
        include_str!("../../../examples/rust/lending/formal_verification/lending.qedspec");

    #[test]
    fn lean_gen_multi_per_account_status() {
        let spec = parser::parse(LENDING_SPEC).unwrap();
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
        let spec = parser::parse(LENDING_SPEC).unwrap();
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
        let spec = parser::parse(LENDING_SPEC).unwrap();
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
        let spec = parser::parse(LENDING_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("inductive PoolOperation where"));
        assert!(lean.contains("inductive LoanOperation where"));
        assert!(lean.contains("def applyPoolOp (s : PoolState)"));
        assert!(lean.contains("def applyLoanOp (s : LoanState)"));
    }

    #[test]
    fn lean_gen_multi_property_binds_to_correct_account() {
        let spec = parser::parse(LENDING_SPEC).unwrap();
        let lean = render(&spec);
        // pool_solvency references total_deposits/total_borrows -> binds to PoolState
        assert!(lean.contains("def pool_solvency (s : PoolState)"));
        assert!(lean.contains("theorem pool_solvency_inductive (s s' : PoolState)"));
    }

    // ========================================================================
    // sBPF (Dropset) tests
    // ========================================================================

    const DROPSET_SPEC: &str =
        include_str!("../../../examples/sbpf/dropset/formal_verification/dropset.qedspec");

    #[test]
    fn lean_gen_sbpf_routes_to_sbpf_renderer() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Should use sBPF imports, not state-machine imports
        assert!(lean.contains("open QEDGen.Solana.SBPF"));
        assert!(lean.contains("import QEDGen"));
        assert!(!lean.contains("structure State where"));
    }

    #[test]
    fn lean_gen_sbpf_namespace() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("namespace RegisterMarket"));
        assert!(lean.contains("end RegisterMarket"));
    }

    #[test]
    fn lean_gen_sbpf_constants() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
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
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Pubkey chunks are emitted as comments (avoid conflict with prog module)
        assert!(lean.contains("--   PUBKEY_RENT_CHUNK_0 = 5862609301215225606"));
        assert!(lean.contains("--   PUBKEY_RENT_CHUNK_1 = 9219231539345853473"));
        assert!(lean.contains("--   PUBKEY_RENT_CHUNK_2 = 4971307250928769624"));
        assert!(lean.contains("--   PUBKEY_RENT_CHUNK_3 = 2329533411"));
    }

    #[test]
    fn lean_gen_sbpf_error_constants() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Error constants emitted as abbrevs in instruction namespace
        assert!(lean.contains("abbrev E_INVALID_DISCRIMINANT : Nat := 1"));
        assert!(lean.contains("abbrev E_INVALID_NUMBER_OF_ACCOUNTS : Nat := 3"));
        assert!(lean.contains("abbrev E_MARKET_ACCOUNT_IS_DUPLICATE : Nat := 5"));
        assert!(lean.contains("abbrev E_INVALID_RENT_SYSVAR_PUBKEY : Nat := 13"));
    }

    #[test]
    fn lean_gen_sbpf_offset_constants_and_ea_lemmas() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
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
        let spec = parser::parse(DROPSET_SPEC).unwrap();
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
        let spec = parser::parse(DROPSET_SPEC).unwrap();
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
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("structure Spec (progAt : Nat → Option Insn) where"));
        // Should have a field for each guard
        assert!(lean.contains("  rejects_invalid_discriminant :"));
        assert!(lean.contains("  rejects_market_duplicate :"));
        assert!(lean.contains("  rejects_invalid_rent_sysvar_pubkey :"));
    }

    #[test]
    fn lean_gen_sbpf_property_stubs() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("theorem memory_safety : True := trivial"));
        assert!(lean.contains("theorem pda_derivation : True := trivial"));
        assert!(lean.contains("theorem account_pointer_flow : True := trivial"));
        assert!(lean.contains("theorem cpi_create_account : True := trivial"));
        assert!(lean.contains("theorem accepts_valid_input : True := trivial"));
    }

    #[test]
    fn lean_gen_sbpf_initstate2_for_two_pointer() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Dropset has insn_layout, so should use initState2
        assert!(lean.contains("initState2 inputAddr insnAddr mem"));
    }

    #[test]
    fn lean_gen_sbpf_entry_point() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Dropset entry is 24
        assert!(lean.contains("initState2 inputAddr insnAddr mem 24"));
    }
}
