use super::*;

// ============================================================================
// File generators
// ============================================================================

/// Pinocchio `src/lib.rs` emitter: no_std crate root + module decls +
/// `declare_id!` + `entrypoint!` + the byte-dispatch `process_instruction`.
/// Idempotent: a pre-existing `src/lib.rs` is left untouched (user-owned)
/// unless `force` regenerates it (#288 — recoverability is asserted by
/// the caller before any artifact is written).
pub(crate) fn emit_pinocchio_program_lib(
    spec: &ParsedSpec,
    fp: &SpecFingerprint,
    output_dir: &Path,
    force: bool,
) -> Result<()> {
    let surface = FrameworkSurface::for_target(Target::Pinocchio);
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;
    let lib_path = src_dir.join("lib.rs");
    if lib_path.exists() && force {
        eprintln!(
            "regenerating user-owned {} (--force) — previous version is in git history.",
            lib_path.display()
        );
    } else if lib_path.exists() {
        eprintln!(
            "programs/{}/src/lib.rs already exists — skipping (user-owned). guards.rs regenerated.",
            output_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("<program>")
        );
        return Ok(());
    }
    let program_id = spec
        .program_id
        .as_deref()
        .unwrap_or("11111111111111111111111111111111");

    let mut out = String::new();
    out.push_str(&marker("DO NOT EDIT", fp, "src/lib.rs"));
    out.push_str(surface.crate_attrs);
    out.push_str(surface.prelude_import);
    out.push('\n');
    out.push_str("mod instructions;\n");
    if !spec.events.is_empty() {
        out.push_str("pub mod events;\n");
    }
    if !spec.error_codes.is_empty() {
        out.push_str("pub mod errors;\n");
    }
    out.push_str("pub mod state;\n");
    out.push_str("pub mod guards;\n");
    out.push_str("#[cfg(kani)]\n");
    out.push_str("extern crate kani;\n");
    out.push_str("#[cfg(kani)]\n");
    out.push_str("mod kani_impl;\n");
    if guards_use_math_helpers(spec) {
        out.push_str("pub mod math;\n");
    }
    if !spec.ref_impls.is_empty() {
        out.push_str("pub mod ref_impls;\n");
    }
    if spec
        .imported_namespaces
        .values()
        .any(|ns| !ns.account_types.is_empty())
    {
        out.push_str("pub mod imported;\n");
    }
    out.push('\n');
    emit_pinocchio_lib_tail(&mut out, spec, program_id);
    out.push_str("// ---- END GENERATED ----\n");
    write_generated_file(&src_dir.join("lib.rs"), &out)?;
    Ok(())
}

/// Emit the Pinocchio `lib.rs` tail: program ID, `entrypoint!`, and the
/// `process_instruction` dispatcher (leading discriminant byte → each
/// handler's `process_<name>` wrapper). `entrypoint!` expands to
/// allocator/panic-handler macros that are internally `target_os =
/// "solana"`-gated, so the invocation is emitted unconditionally.
pub(crate) fn emit_pinocchio_lib_tail(out: &mut String, spec: &ParsedSpec, program_id: &str) {
    out.push_str(&format!(
        "pinocchio_pubkey::declare_id!(\"{}\");\n\n",
        program_id
    ));
    // `entrypoint!`'s single-arg arm recursively calls `entrypoint!`
    // *unqualified*, so it must be imported (a `pinocchio::entrypoint!`
    // path call fails to resolve the inner recursion).
    out.push_str("use pinocchio::entrypoint;\n");
    out.push_str("entrypoint!(process_instruction);\n\n");
    out.push_str("/// Instruction dispatch — the leading byte of `instruction_data`\n");
    out.push_str("/// selects the handler (discriminant = declaration order).\n");
    out.push_str("pub fn process_instruction(\n");
    out.push_str("    _program_id: &pinocchio::pubkey::Pubkey,\n");
    out.push_str("    accounts: &[AccountInfo],\n");
    out.push_str("    instruction_data: &[u8],\n");
    out.push_str(") -> ProgramResult {\n");
    out.push_str("    let (discriminant, data) = instruction_data\n");
    out.push_str("        .split_first()\n");
    out.push_str("        .ok_or(ProgramError::InvalidInstructionData)?;\n");
    out.push_str("    match *discriminant {\n");
    for (i, handler) in spec.handlers.iter().enumerate() {
        out.push_str(&format!(
            "        {} => instructions::{}::process_{}(accounts, data),\n",
            i, handler.name, handler.name
        ));
    }
    out.push_str("        _ => Err(ProgramError::InvalidInstructionData),\n");
    out.push_str("    }\n");
    out.push_str("}\n");
}

/// Emit `src/state.rs` for the Pinocchio target. zeropod zero-copy: each
/// persisted struct is the *schema* (plain Rust field types) and
/// `#[derive(ZeroPod)]` generates the alignment-1 `<Struct>Zc` companion
/// mutated in place via `from_bytes_mut`. Lifecycle / sum-type State
/// lowers to a `u8` discriminant + `#[repr(u8)]` enum (same shape as
/// Anchor/Quasar, keeping the delegated guard codegen consistent);
/// variant payloads flatten into one superset struct, tag byte selecting
/// the live variant.
pub(crate) fn emit_pinocchio_state(
    spec: &ParsedSpec,
    fp: &SpecFingerprint,
    out: &mut String,
) -> Result<()> {
    out.push_str(&marker("DO NOT EDIT", fp, "src/state.rs"));
    out.push_str("use zeropod::ZeroPod;\n\n");

    // Record types referenced by state fields → ZeroPod structs (emitted
    // ahead of the account structs that nest them).
    for record in &spec.records {
        out.push_str("#[derive(ZeroPod)]\n");
        out.push_str(&format!("pub struct {} {{\n", record.name));
        for (fname, ftype) in &record.fields {
            out.push_str(&format!(
                "    pub {}: {},\n",
                fname,
                map_type_standalone(ftype, spec)?
            ));
        }
        out.push_str("}\n\n");
    }

    if spec.account_types.len() > 1 {
        // Multi-account: one ZeroPod struct per account type.
        for acct in &spec.account_types {
            let struct_name = format!("{}Account", acct.name);
            let enum_name = format!("{}Status", acct.name);
            emit_pinocchio_state_struct(
                out,
                &struct_name,
                &acct.fields,
                acct.pda_ref.is_some(),
                &acct.lifecycle,
                &enum_name,
                spec,
            )?;
        }
    } else if spec
        .account_types
        .first()
        .map(|a| a.variants.len() > 1)
        .unwrap_or(false)
    {
        // Sum-type State → discriminant tag byte + flat superset struct.
        let acct = &spec.account_types[0];
        let state_name = format!("{}Account", to_pascal_case(&spec.program_name));
        let tag_name = format!("{}Tag", state_name);
        out.push_str(&format!(
            "/// Discriminant tag for `{}`. Variant payloads are stored\n\
             /// flattened in the struct below; the `tag` byte selects the\n\
             /// live variant.\n",
            state_name
        ));
        out.push_str("#[derive(Clone, Copy, PartialEq, Eq)]\n#[repr(u8)]\n");
        out.push_str(&format!("pub enum {} {{\n", tag_name));
        for (i, v) in acct.variants.iter().enumerate() {
            out.push_str(&format!("    {} = {},\n", v.name, i));
        }
        out.push_str("}\n\n");

        out.push_str("#[derive(ZeroPod)]\n");
        out.push_str(&format!("pub struct {} {{\n", state_name));
        out.push_str("    pub tag: u8,\n");
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for v in &acct.variants {
            for (fname, ftype) in &v.fields {
                if seen.insert(fname.clone()) {
                    out.push_str(&format!(
                        "    pub {}: {},\n",
                        fname,
                        map_type_standalone(ftype, spec)?
                    ));
                }
            }
        }
        if !spec.pdas.is_empty() && !seen.contains("bump") {
            out.push_str("    pub bump: u8,\n");
        }
        out.push_str("}\n\n");
    } else {
        // Single flat record state.
        let state_name = format!("{}Account", to_pascal_case(&spec.program_name));
        emit_pinocchio_state_struct(
            out,
            &state_name,
            &spec.state_fields,
            !spec.pdas.is_empty(),
            &spec.lifecycle_states,
            "Status",
            spec,
        )?;
    }

    out.push_str("// ---- END GENERATED ----\n");
    Ok(())
}

/// Emit one Pinocchio `#[derive(ZeroPod)]` state struct (+ its optional
/// `#[repr(u8)]` lifecycle enum). `has_pda` appends a `bump: u8`;
/// `lifecycle` (when non-empty) emits the named-constant enum + a
/// `status: u8` field. Shared by the single-account + multi-account
/// branches of `emit_pinocchio_state`.
pub(crate) fn emit_pinocchio_state_struct(
    out: &mut String,
    struct_name: &str,
    fields: &[(String, String)],
    has_pda: bool,
    lifecycle: &[String],
    enum_name: &str,
    spec: &ParsedSpec,
) -> Result<()> {
    if !lifecycle.is_empty() {
        out.push_str(&format!("/// {} lifecycle states.\n", enum_name));
        out.push_str("#[derive(Clone, Copy, PartialEq, Eq)]\n#[repr(u8)]\n");
        out.push_str(&format!("pub enum {} {{\n", enum_name));
        for (i, s) in lifecycle.iter().enumerate() {
            out.push_str(&format!("    {} = {},\n", s, i));
        }
        out.push_str("}\n\n");
    }
    out.push_str("#[derive(ZeroPod)]\n");
    out.push_str(&format!("pub struct {} {{\n", struct_name));
    for (fname, ftype) in fields {
        out.push_str(&format!(
            "    pub {}: {},\n",
            fname,
            map_type_standalone(ftype, spec)?
        ));
    }
    if has_pda && !fields.iter().any(|(n, _)| n == "bump") {
        out.push_str("    pub bump: u8,\n");
    }
    if !lifecycle.is_empty() && !fields.iter().any(|(n, _)| n == "status") {
        out.push_str("    pub status: u8,\n");
    }
    out.push_str("}\n\n");
    Ok(())
}

/// DSL integer type → (Rust primitive, byte width) for little-endian
/// (de)serialization of instruction-data params. None for non-integer
/// types (the Pinocchio wrapper emits a `todo!()` for those).
pub(crate) fn numeric_param_width(dsl_type: &str) -> Option<(&'static str, usize)> {
    match dsl_type.trim() {
        "U8" => Some(("u8", 1)),
        "I8" => Some(("i8", 1)),
        "U16" => Some(("u16", 2)),
        "I16" => Some(("i16", 2)),
        "U32" => Some(("u32", 4)),
        "I32" => Some(("i32", 4)),
        "U64" => Some(("u64", 8)),
        "I64" => Some(("i64", 8)),
        "U128" => Some(("u128", 16)),
        "I128" => Some(("i128", 16)),
        _ => None,
    }
}

/// Emit one Pinocchio `instructions/<name>.rs` scaffold. USER-OWNED
/// (emitted only when the file is missing). Shape: a `struct <Pascal><'a>`
/// of `&AccountInfo` fields + `fn handler` (calls `crate::guards::<name>`,
/// then applies effects) + a free `process_<name>(accounts, data)` wrapper
/// that binds the account slice positionally, parses params from
/// `instruction_data` (LE, offset-tracked), and calls `.handler()`.
pub(crate) fn render_pinocchio_handler_scaffold(
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Result<String> {
    let pascal = to_pascal_case(&handler.name);
    let mut out = String::new();

    out.push_str("// User-owned. Regenerating the spec does NOT overwrite this file.\n");
    out.push_str("// Guard checks live in the sibling `crate::guards` module and ARE\n");
    out.push_str("// regenerated on every `qedgen codegen`.\n\n");
    out.push_str(
        "use pinocchio::{account_info::AccountInfo, program_error::ProgramError, ProgramResult};\n",
    );
    out.push_str("use zeropod::ZeroPodFixed;\n");
    out.push_str("use crate::state::*;\n");
    if !spec.ref_impls.is_empty() {
        out.push_str("use crate::ref_impls::*;\n");
    }
    out.push_str("use crate::guards;\n");
    if !spec.error_codes.is_empty() {
        out.push_str("use crate::errors::*;\n");
    }
    out.push('\n');

    // Every field is a raw &AccountInfo (zeropod decode in .handler()).
    out.push_str(&format!("pub struct {}<'a> {{\n", pascal));
    for acct in &handler.accounts {
        out.push_str(&format!("    pub {}: &'a AccountInfo,\n", acct.name));
    }
    out.push_str("}\n\n");

    let params_sig: String = handler
        .takes_params
        .iter()
        .map(|(n, t)| map_type_standalone(t, spec).map(|ty| format!(", {}: {}", n, ty)))
        .collect::<Result<Vec<_>>>()?
        .join("");
    let param_names: Vec<&str> = handler
        .takes_params
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();

    out.push_str(&format!("impl {}<'_> {{\n", pascal));
    out.push_str(&format!(
        "    pub fn handler(&mut self{}) -> ProgramResult {{\n",
        params_sig
    ));
    if param_names.is_empty() {
        out.push_str(&format!("        guards::{}(self)?;\n", handler.name));
    } else {
        out.push_str(&format!(
            "        guards::{}(self, {})?;\n",
            handler.name,
            param_names.join(", ")
        ));
    }
    let needs_fill = emit_pinocchio_effect_body(&mut out, handler, spec);
    if needs_fill {
        out.push_str("        todo!(\"fill non-mechanical effects, events, transfers, calls\")\n");
    } else {
        out.push_str("        Ok(())\n");
    }
    out.push_str("    }\n");
    out.push_str("}\n\n");

    out.push_str(&format!(
        "/// Entrypoint wrapper — binds the account slice + parses params, then\n\
         /// calls `{}::handler`. Invoked by `process_instruction` in lib.rs.\n",
        pascal
    ));
    out.push_str(&format!(
        "pub fn process_{}(accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {{\n",
        handler.name
    ));
    if handler.accounts.is_empty() {
        out.push_str("    let _ = accounts;\n");
    } else {
        let names: Vec<&str> = handler.accounts.iter().map(|a| a.name.as_str()).collect();
        out.push_str(&format!(
            "    let [{}, ..] = accounts else {{\n",
            names.join(", ")
        ));
        out.push_str("        return Err(ProgramError::NotEnoughAccountKeys);\n");
        out.push_str("    };\n");
    }
    if handler.takes_params.is_empty() {
        out.push_str("    let _ = instruction_data;\n");
    } else {
        let mut offset = 0usize;
        for (pname, ptype) in &handler.takes_params {
            match numeric_param_width(ptype) {
                Some((rust_ty, width)) => {
                    out.push_str(&format!(
                        "    let {} = {}::from_le_bytes(\n        instruction_data\n            .get({}..{})\n            .ok_or(ProgramError::InvalidInstructionData)?\n            .try_into()\n            .map_err(|_| ProgramError::InvalidInstructionData)?,\n    );\n",
                        pname,
                        rust_ty,
                        offset,
                        offset + width
                    ));
                    offset += width;
                }
                None => {
                    out.push_str(&format!(
                        "    // TODO: parse non-numeric param `{}` (spec type {}) from instruction_data\n",
                        pname, ptype
                    ));
                    out.push_str(&format!(
                        "    let {}: {} = todo!(\"parse {} from instruction_data\");\n",
                        pname,
                        map_type_standalone(ptype, spec)?,
                        pname
                    ));
                }
            }
        }
    }
    let field_init: String = handler
        .accounts
        .iter()
        .map(|a| a.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!(
        "    let mut ctx = {} {{ {} }};\n",
        pascal, field_init
    ));
    out.push_str(&format!("    ctx.handler({})\n", param_names.join(", ")));
    out.push_str("}\n");

    Ok(out)
}

/// Emit the `.handler()` effect body for a Pinocchio handler. Mechanical
/// SCALAR state effects lower to a one-time mutable zeropod decode plus
/// per-field `.get()` arithmetic (native int op, then `.into()` back to
/// the Pod field); RHS expressions render tree-native (#223) under the
/// zeropod Pod style with `self`-prefixed account key loads.
/// SPL Token CPIs lower via `try_emit_cpi` — the handler struct's
/// `&'a AccountInfo` fields match `pinocchio_token`'s CPI struct fields
/// directly. Deferred surfaces (non-scalar effects, events, `transfers`
/// sugar, generic non-SPL CPI) emit documented breadcrumbs.
pub(crate) fn emit_pinocchio_effect_body(
    out: &mut String,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> bool {
    let prog = to_pascal_case(&spec.program_name);
    let err = format!("{}Error", prog);
    let has = |n: &str| spec.error_codes.iter().any(|c| c == n);
    let overflow = if has("MathOverflow") {
        format!("ProgramError::from({}::MathOverflow)", err)
    } else {
        "ProgramError::ArithmeticOverflow".to_string()
    };
    let underflow = if has("MathUnderflow") {
        format!("ProgramError::from({}::MathUnderflow)", err)
    } else if has("MathOverflow") {
        format!("ProgramError::from({}::MathOverflow)", err)
    } else {
        "ProgramError::ArithmeticOverflow".to_string()
    };

    // Classify scalar state effects (lhs is a simple field after stripping
    // any `Variant.` prefix — no `.` / `[` remaining).
    let scalar: Vec<(String, &crate::check::ParsedEffect)> = handler
        .effects
        .iter()
        .filter_map(|e| {
            let field = strip_variant_prefix(&e.field, spec);
            if field.contains('.') || field.contains('[') {
                None
            } else {
                Some((field, e))
            }
        })
        .collect();

    // Single-account decode only (multi-account state typing is deferred).
    let single_state = spec.account_types.len() <= 1;
    if !scalar.is_empty() && single_state {
        if let Some(acct) = resolve_handler_state_account(handler, spec) {
            out.push_str(&format!(
                "        let __state = {}Account::from_bytes_mut(unsafe {{ self.{}.borrow_mut_data_unchecked() }})\n            .map_err(|_| ProgramError::InvalidAccountData)?;\n",
                prog, acct.name
            ));
            for (field, eff) in &scalar {
                // Effect body lives in the handler method, so account refs
                // bind through `self` (not the guard fn's `ctx`).
                let r = render_pinocchio_expr(
                    crate::codegen_shared::effect_tree(eff),
                    crate::rust_codegen_util::tree_render::AcctKeyStyle::PinocchioSelf,
                );
                let line = match eff.op.as_str() {
                    // Pubkey fields are raw `[u8; 32]` (no Pod wrapper), so
                    // assign the deref'd value directly — `.into()` is for
                    // the native-int → Pod-scalar conversion only.
                    "set" if state_field_is_pubkey(spec, field) => {
                        format!("        __state.{field} = {r};\n")
                    }
                    "set" => format!("        __state.{field} = ({r}).into();\n"),
                    "add" => format!(
                        "        __state.{field} = __state.{field}.get().checked_add({r}).ok_or({overflow})?.into();\n"
                    ),
                    "sub" => format!(
                        "        __state.{field} = __state.{field}.get().checked_sub({r}).ok_or({underflow})?.into();\n"
                    ),
                    "add_sat" => format!(
                        "        __state.{field} = __state.{field}.get().saturating_add({r}).into();\n"
                    ),
                    "sub_sat" => format!(
                        "        __state.{field} = __state.{field}.get().saturating_sub({r}).into();\n"
                    ),
                    "add_wrap" => format!(
                        "        __state.{field} = __state.{field}.get().wrapping_add({r}).into();\n"
                    ),
                    "sub_wrap" => format!(
                        "        __state.{field} = __state.{field}.get().wrapping_sub({r}).into();\n"
                    ),
                    other => format!("        // TODO: effect op `{other}` on `{field}` not mechanized\n"),
                };
                out.push_str(&line);
            }
        } else {
            out.push_str(
                "        // TODO(slice 6 4b): could not resolve the state account for effects\n",
            );
        }
    }

    // Deferred surfaces — documented breadcrumbs (not silently dropped).
    let complex_effects =
        handler.effects.len() > scalar.len() || (!scalar.is_empty() && !single_state);
    if complex_effects {
        out.push_str("        // TODO(slice 6 4b-cont): non-scalar effects (array / nested /\n        // variant-payload writes) + multi-account state.\n");
    }

    // Anchor and Quasar lifecycle init is owned by their account macros.
    // Pinocchio has no equivalent implicit creation step: mutating the zero-copy
    // view without first allocating/assigning the PDA would be a plausible but
    // incomplete operation, so make the ownership boundary executable.
    let needs_pda_creation = matches!(
        handler.pre_status.as_deref(),
        Some("Uninitialized" | "Empty")
    ) && handler
        .accounts
        .iter()
        .any(|a| !a.is_signer && a.pda_seeds.is_some());
    if needs_pda_creation {
        out.push_str(
            "        // Lifecycle init requires PDA allocation/assignment — agent fill: create the complete signed System CPI\n",
        );
    }
    // Explicit `call Interface.handler(...)` sites. Non-SPL (generic
    // invoke) call sites return `None` and fall through to a breadcrumb.
    let mut any_unmechanized_call = false;
    for c in &handler.calls {
        match plan_cpi(c, handler, spec, Target::Pinocchio) {
            CpiPlan::Complete(rendered) => {
                out.push_str(&format!(
                    "        // Spec call: {}.{}\n",
                    c.target_interface, c.target_handler
                ));
                out.push_str(&rendered);
            }
            CpiPlan::AgentFill(reason) => {
                out.push_str(&format!(
                    "        // Spec call: {}.{} — agent fill: {}\n",
                    c.target_interface,
                    c.target_handler,
                    reason.render(),
                ));
                any_unmechanized_call = true;
            }
        }
    }

    // `transfers { … }` stays agent-fill on every target (CPI/authority
    // business logic); events carry no payload binding in the spec.
    for emit in &handler.emits {
        out.push_str(&format!(
            "        // Spec event: emit {} — agent fill\n",
            emit
        ));
    }
    for transfer in &handler.transfers {
        out.push_str(&format!(
            "        // Spec transfer: {} -> {} amount={} — agent fill: assemble the complete CPI and authority\n",
            transfer.from,
            transfer.to,
            transfer.amount.as_deref().unwrap_or("?"),
        ));
    }

    complex_effects
        || needs_pda_creation
        || any_unmechanized_call
        || !handler.emits.is_empty()
        || !handler.transfers.is_empty()
}

/// `true` when the spec's state is a multi-variant ADT (≥2 variants in a
/// single account type) opted into the wrapper-struct + inner-enum
/// emission (`pragma state_repr = adt` via `state_repr_is_adt`).
/// Single-record account types, single-variant ADTs, multi-account specs,
/// and non-opted specs all stay on the flat-fields + `Status`-enum path.
pub fn is_multi_variant_adt_state(spec: &ParsedSpec) -> bool {
    spec.state_repr_is_adt()
        && spec.account_types.len() == 1
        && spec
            .account_types
            .first()
            .map(|a| a.variants.len() > 1)
            .unwrap_or(false)
}

/// The generated account-wrapper struct name for the state a handler
/// drives. Single source for the two sites that must agree: the struct
/// `generate_state` emits, and the `space = 8 + <T>::INIT_SPACE`
/// reference `account_attr` renders on an `init` account. When they
/// disagree the generated scaffold does not compile.
///
/// Only a MULTI-account spec names the wrapper after the ADT — there is
/// one struct per account type. A single-account spec (flat state or
/// multi-variant ADT alike) names it after the program, so keying on
/// `on_account` alone is wrong: a lifecycle-typed handler
/// (`: Vault.Uninitialized -> Vault.Active`) sets `on_account` even
/// when the spec has exactly one account type.
pub fn state_struct_name(spec: &ParsedSpec, on_account: Option<&str>) -> String {
    match on_account {
        Some(adt) if spec.account_types.len() > 1 => format!("{adt}Account"),
        _ => format!(
            "{}Account",
            crate::codegen_shared::to_pascal_case(&spec.program_name)
        ),
    }
}

/// Index an ADT account's variant fields: field name → every
/// `(variant_name, declared_type)` occurrence, in name-sorted (BTreeMap)
/// order. Raw view — includes fields whose type differs across variants;
/// see [`consistent_variant_fields`] for the accessor-eligible subset.
pub(crate) fn variant_field_index(
    acct: &crate::check::ParsedAccountType,
) -> std::collections::BTreeMap<String, Vec<(String, String)>> {
    let mut field_index: std::collections::BTreeMap<String, Vec<(String, String)>> =
        std::collections::BTreeMap::new();
    for variant in &acct.variants {
        for (fname, ftype) in &variant.fields {
            field_index
                .entry(fname.clone())
                .or_default()
                .push((variant.name.clone(), ftype.clone()));
        }
    }
    field_index
}

/// Fields shared across an ADT account's variants with a *consistent*
/// declared type: field name → (type, carrying variants), name-sorted.
/// This is the single source of truth for which fields get a wrapper
/// accessor — emission (`render_adt_inner_enum`) and consumption
/// (`generate_guards` / `render_let_binding_rust`) must agree, or a
/// guard would call an accessor that was never emitted.
pub(crate) fn consistent_variant_fields(
    acct: &crate::check::ParsedAccountType,
) -> std::collections::BTreeMap<String, (String, Vec<String>)> {
    variant_field_index(acct)
        .into_iter()
        .filter_map(|(fname, occurrences)| {
            let first_ty = occurrences[0].1.clone();
            if occurrences.iter().any(|(_, t)| t != &first_ty) {
                return None;
            }
            let variants = occurrences.into_iter().map(|(v, _)| v).collect();
            Some((fname, (first_ty, variants)))
        })
        .collect()
}

/// Accessor-eligible field names for the spec's (single) multi-variant
/// ADT account; empty for flat-state specs.
pub(crate) fn adt_accessor_field_names(spec: &ParsedSpec) -> std::collections::HashSet<String> {
    if !is_multi_variant_adt_state(spec) {
        return std::collections::HashSet::new();
    }
    let Some(acct) = spec.account_types.first() else {
        return std::collections::HashSet::new();
    };
    consistent_variant_fields(acct).into_keys().collect()
}

/// Render the multi-variant ADT inner enum + its consistent-field
/// accessors (the shared tail of the wrapper-struct emission — the
/// wrapper struct itself differs per site and stays with the caller).
/// `inner_doc` is the full doc-comment block above the enum;
/// `accessor_doc` renders the per-field doc block; `blank_after_impl`
/// matches each site's historical trailing-newline shape.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_adt_inner_enum(
    out: &mut String,
    acct: &crate::check::ParsedAccountType,
    inner_name: &str,
    inner_doc: &str,
    accessor_doc: &dyn Fn(&str) -> String,
    parsed: &ParsedSpec,
    target: Target,
    blank_after_impl: bool,
) -> Result<()> {
    out.push_str(inner_doc);
    out.push_str(
        "#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Debug, PartialEq)]\n",
    );
    out.push_str(&format!("pub enum {} {{\n", inner_name));
    for variant in &acct.variants {
        if variant.fields.is_empty() {
            out.push_str(&format!("    {},\n", variant.name));
        } else {
            out.push_str(&format!("    {} {{\n", variant.name));
            for (fname, ftype) in &variant.fields {
                out.push_str(&format!(
                    "        {}: {},\n",
                    fname,
                    map_type_for_target(ftype, parsed, target)?
                ));
            }
            out.push_str("    },\n");
        }
    }
    out.push_str("}\n\n");

    // Accessors for fields shared (with consistent type) across variants.
    if !variant_field_index(acct).is_empty() {
        out.push_str(&format!("impl {} {{\n", inner_name));
        for (fname, (first_ty, variants)) in &consistent_variant_fields(acct) {
            let rust_ty = map_type_for_target(first_ty, parsed, target)?;
            out.push_str(&accessor_doc(fname));
            out.push_str(&format!(
                "    pub fn {}(&self) -> &{} {{\n        match self {{\n",
                fname, rust_ty
            ));
            for variant_name in variants {
                out.push_str(&format!(
                    "            Self::{} {{ {}, .. }} => {},\n",
                    variant_name, fname, fname
                ));
            }
            if variants.len() < acct.variants.len() {
                out.push_str(&format!(
                    "            _ => panic!(\"{}::{}() called on a variant without `{}`\"),\n",
                    inner_name, fname, fname
                ));
            }
            out.push_str("        }\n    }\n");
        }
        out.push_str(if blank_after_impl { "}\n\n" } else { "}\n" });
    }
    Ok(())
}

/// Identifier-character predicate for the word-bounded rewrites: ASCII
/// alphanumerics plus underscore.
pub(crate) fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Word-boundary substring search: `needle` appears in `haystack` as a
/// complete identifier. Detects whether a `requires` expression
/// references an `abstract` binder without false-matching `<binder>_x` /
/// `prefix<binder>` substrings.
pub(crate) fn contains_word_boundary(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let hb = haystack.as_bytes();
    let nb = needle.as_bytes();
    if nb.len() > hb.len() {
        return false;
    }
    let mut i = 0;
    while i + nb.len() <= hb.len() {
        if &hb[i..i + nb.len()] == nb {
            let prev_ok = i == 0 || !is_ident_char(hb[i - 1]);
            let next_idx = i + nb.len();
            let next_ok = next_idx >= hb.len() || !is_ident_char(hb[next_idx]);
            if prev_ok && next_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Rewrite each `[<idx>]` substring to `[(<idx>) as usize]`. Used by
/// `mechanize_effect` (Rust output) to keep the field-string Lean-clean
/// while still satisfying Rust's `usize`-only array indexing. Same
/// transform as `path_to_rust`'s Index emission, applied at codegen
/// time instead of at expr-render time so both Lean and Rust read the
/// same `(field, op_kind, value)` tuple.
pub(crate) fn rewrite_index_to_usize(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = String::with_capacity(field.len() + 16);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            // Find matching `]`.
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b']' {
                end += 1;
            }
            if end >= bytes.len() {
                // Unbalanced — give up and emit verbatim.
                out.push_str(&field[i..]);
                break;
            }
            let idx_expr = &field[start..end];
            // Don't double-wrap if already cast.
            if idx_expr.contains("as usize") {
                out.push_str(&field[i..=end]);
            } else {
                out.push_str(&format!("[({}) as usize]", idx_expr));
            }
            i = end + 1;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Render the pre-status check (when `write` is false) or the post-status
/// write (when `write` is true) for R26 lifecycle enforcement. Returns an
/// empty string when the lifecycle clause doesn't require a runtime
/// emission (init handlers skip the pre-check; pre==post handlers skip
/// the post-write; specs without lifecycle declarations skip everything).
pub(crate) fn lifecycle_check_line(
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    write: bool,
    surface: &FrameworkSurface,
) -> String {
    // Find the state-bearing account name and its `<ADT>Status` enum.
    let state_acct = find_state_account(handler);
    let Some(sa) = state_acct else {
        return String::new();
    };

    // Multi-variant ADT state has no `status: u8` byte; the variant IS
    // the discriminator. Pre-check rewrites to `matches!(inner,
    // Inner::<pre> { .. })`; post-write is a no-op (the effect lowering
    // moves the variant). Anchor-only: Quasar's zero-copy `#[account]`
    // can't carry enum payloads, so Quasar stays on the flat shape and
    // the legacy `status` byte check below (its wrapper has no `inner`
    // field).
    if is_multi_variant_adt_state(spec) && matches!(surface.target, Target::Anchor) {
        if write {
            return String::new();
        }
        let pre = handler.pre_status.as_deref().unwrap_or("");
        if pre.is_empty() || matches!(pre, "Uninitialized" | "Empty") {
            return String::new();
        }
        let Some(acct) = spec.account_types.first() else {
            return String::new();
        };
        let Some(variant) = acct.variants.iter().find(|v| v.name == pre) else {
            return String::new();
        };
        let inner_name = format!("{}AccountInner", to_pascal_case(&spec.program_name));
        let err_enum = format!("crate::errors::{}Error", to_pascal_case(&spec.program_name));
        let err_ctor = surface.error_expr(&err_enum, "InvalidLifecycle");
        // Payload variants need `{ .. }`; unit variants don't.
        let pattern = if variant.fields.is_empty() {
            format!("{}::{}", inner_name, pre)
        } else {
            format!("{}::{} {{ .. }}", inner_name, pre)
        };
        return format!(
            "    // lifecycle: require inner == {pre}\n    if !matches!(ctx.{acct}.inner, {pattern}) {{ return Err({err_ctor}); }}\n",
            pre = pre,
            acct = sa.name,
            pattern = pattern,
            err_ctor = err_ctor,
        );
    }

    // Status enum naming mirrors state emission: multi-account →
    // `<ADT>Status` per lifecycle; otherwise a single `Status`. Note: one
    // `account_types` entry (`type State | …`) is still "single-state".
    let is_multi = spec.account_types.len() > 1;
    let (enum_name, lifecycle): (String, &Vec<String>) = if is_multi {
        let Some(adt) = handler.on_account.as_deref() else {
            return String::new();
        };
        let Some(at) = spec.account_types.iter().find(|a| a.name == adt) else {
            return String::new();
        };
        if at.lifecycle.is_empty() {
            return String::new();
        }
        (format!("{}Status", at.name), &at.lifecycle)
    } else {
        // Lifecycle lives on `account_types[0].lifecycle` (ADT form) or
        // `spec.lifecycle_states` (flat `state {}` form); prefer the ADT.
        let lifecycle: &Vec<String> = spec
            .account_types
            .first()
            .map(|at| &at.lifecycle)
            .filter(|v| !v.is_empty())
            .unwrap_or(&spec.lifecycle_states);
        if lifecycle.is_empty() {
            return String::new();
        }
        ("Status".to_string(), lifecycle)
    };

    let pre = handler.pre_status.as_deref().unwrap_or("");
    let post = handler.post_status.as_deref().unwrap_or("");
    if pre.is_empty() && post.is_empty() {
        return String::new();
    }

    let is_init_pre = matches!(pre, "Uninitialized" | "Empty");

    let err_enum = format!("crate::errors::{}Error", to_pascal_case(&spec.program_name));

    if write {
        // Post-status write: only when post is set and differs from pre.
        if post.is_empty() || pre == post {
            return String::new();
        }
        if !lifecycle.iter().any(|s| s == post) {
            return String::new();
        }
        format!(
            "    // lifecycle: status := {post}\n    ctx.{acct}.status = {enum_name}::{post} as u8;\n",
            post = post,
            acct = sa.name,
            enum_name = enum_name,
        )
    } else {
        // Pre-status check: skip on init transitions (init zeros the
        // account) and when there's no pre to check.
        if is_init_pre || pre.is_empty() {
            return String::new();
        }
        if !lifecycle.iter().any(|s| s == pre) {
            return String::new();
        }
        let err_ctor = surface.error_expr(&err_enum, "InvalidLifecycle");
        format!(
            "    // lifecycle: require status == {pre}\n    if ctx.{acct}.status != {enum_name}::{pre} as u8 {{ return Err({err_ctor}); }}\n",
            pre = pre,
            acct = sa.name,
            enum_name = enum_name,
            err_ctor = err_ctor,
        )
    }
}

pub(crate) fn find_state_account(
    handler: &ParsedHandler,
) -> Option<&crate::check::ParsedHandlerAccount> {
    // Writable-only first; fall back to all non-signer/non-program/
    // non-token candidates so read-only handlers still get `s.field`
    // rewritten to `ctx.<acct>.field` in guards.rs (bare `s.field`
    // wouldn't compile).
    if let Some(found) = find_state_account_filtered(handler, true) {
        return Some(found);
    }
    find_state_account_filtered(handler, false)
}

pub(crate) fn find_state_account_filtered(
    handler: &ParsedHandler,
    require_writable: bool,
) -> Option<&crate::check::ParsedHandlerAccount> {
    let mut candidates: Vec<&crate::check::ParsedHandlerAccount> = handler
        .accounts
        .iter()
        .filter(|a| (!require_writable || a.is_writable) && !a.is_signer && !a.is_program)
        .filter(|a| {
            // Drop token/mint accounts — they hold balances, not program state.
            !matches!(a.account_type.as_deref(), Some("token") | Some("mint"))
        })
        .collect();

    // Prefer PDA-derived candidates when available.
    let pda_candidates: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|a| a.pda_seeds.is_some())
        .collect();
    if !pda_candidates.is_empty() {
        candidates = pda_candidates;
    }

    if candidates.len() == 1 {
        return Some(candidates[0]);
    }
    // Multi-state disambiguator: when the handler declares `on_account`
    // (`: Loan.Pre -> Loan.Post`), pick the handler-account whose name
    // matches the ADT (lowercase) — otherwise two writable PDA candidates
    // would return None and leave guard refs un-rewritten.
    if let Some(adt) = handler.on_account.as_deref() {
        if let Some(matched) = candidates
            .iter()
            .copied()
            .find(|a| account_name_matches_adt(adt, &a.name))
        {
            return Some(matched);
        }
    }
    None
}

/// Canonical SPL Token program ID. Calls into an interface whose
/// `program_id "..."` matches this constant get the `anchor_spl::token::*`
/// CPI shape; other program IDs route through the generic
/// `solana_program::program::invoke` builder.
pub(crate) const SPL_TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// Canonical System Program ID (all-ones base58 → the all-zero pubkey).
/// Calls into the bundled `System` interface (`import System from "system"`)
/// match this constant and get the `pinocchio_system::instructions::*` CPI
/// shape on the Pinocchio target. The System Program is a native built-in,
/// so unlike SPL Token there is no deployed binary to hash — the upstream
/// pin's `binary_hash` stays all-zero (see `data/interfaces/system.qedspec`).
pub(crate) const SYSTEM_PROGRAM_ID: &str = "11111111111111111111111111111111";
