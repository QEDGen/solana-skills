use super::*;

/// The only two legal outcomes for a spec-level CPI call.
///
/// A target either emits the complete invocation, including every signer the
/// interface requires, or it emits an explicit agent-fill site. In particular,
/// a PDA signer may never fall through to an unsigned `invoke()` / `.invoke()`
/// that looks complete but can only fail at runtime.
pub(crate) enum CpiPlan {
    Complete(String),
    AgentFill(CpiFillReason),
}

pub(crate) enum CpiFillReason {
    /// One or more interface signer slots bind to caller-owned PDAs and the
    /// target's emitter did not assemble signer seeds for them (Anchor builder
    /// shapes do — `CpiContext::new_with_signer`; every other path emits
    /// unsigned invocations).
    PdaSigner { accounts: Vec<String> },
    /// The target has no complete lowering for this interface/handler shape.
    Unsupported { target: Target },
}

impl CpiFillReason {
    pub(crate) fn render(&self) -> String {
        match self {
            Self::PdaSigner { accounts } => format!(
                "PDA signer authority ({}) requires signer-seed assembly; fill the complete signed CPI",
                accounts.join(", ")
            ),
            Self::Unsupported { target } => format!(
                "{} does not fully mechanize this CPI shape; fill the complete invocation",
                target_name(*target)
            ),
        }
    }
}

fn target_name(target: Target) -> &'static str {
    match target {
        Target::Anchor => "Anchor",
        Target::Quasar => "Quasar",
        Target::Pinocchio => "Pinocchio",
    }
}

/// Build the per-call mechanization plan.
///
/// Target dispatch runs first; the PDA-signer check is a post-condition on
/// the emitted artifact. A call whose signer slots bind caller PDAs is
/// `Complete` only if the emitted code actually signs for them
/// (`new_with_signer` / `invoke_signed`) — the Anchor builder shapes do, and
/// the runtime journey (#308/#310) proves that path on-chain. Any emitter
/// that would produce a plausible but unsigned invocation for a program
/// authority (Anchor generic `invoke`, the builder's plain-`new` fallback
/// when seeds aren't assemblable, Quasar, Pinocchio) resolves to an explicit
/// agent-fill site instead.
pub(crate) fn plan_cpi(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    target: Target,
) -> CpiPlan {
    let Some(iface) = spec
        .interfaces
        .iter()
        .find(|i| i.name == call.target_interface)
    else {
        return CpiPlan::AgentFill(CpiFillReason::Unsupported { target });
    };

    let pda_signers = iface
        .handlers
        .iter()
        .find(|h| h.name == call.target_handler)
        .into_iter()
        .flat_map(|h| h.accounts.iter().filter(|a| a.is_signer))
        .filter_map(|signer_slot| {
            let arg = call.args.iter().find(|a| a.name == signer_slot.name)?;
            handler
                .accounts
                .iter()
                .find(|a| a.name == arg.rust_expr && a.pda_seeds.is_some())
                .map(|a| a.name.clone())
        })
        .collect::<Vec<_>>();

    let is_spl_token = iface.program_id.as_deref() == Some(SPL_TOKEN_PROGRAM_ID);
    let is_system = iface.program_id.as_deref() == Some(SYSTEM_PROGRAM_ID);

    let emitted = match (target, is_spl_token, is_system) {
        (Target::Anchor, true, _) => emit_spl_token_cpi_anchor(call, handler, spec),
        (Target::Anchor, false, true) => emit_system_cpi_anchor(call, handler, iface, spec),
        (Target::Anchor, false, false) => emit_generic_cpi_anchor(call, handler, iface, spec),
        (Target::Quasar, true, _) => emit_spl_token_cpi_quasar(call, handler, spec),
        (Target::Quasar, false, true) => emit_system_cpi_quasar(call, handler, spec),
        // Quasar generic CPI not implemented (would use `BufCpiCall`).
        (Target::Quasar, false, false) => None,
        (Target::Pinocchio, true, _) => emit_spl_token_cpi_pinocchio(call, handler, spec),
        (Target::Pinocchio, false, true) => emit_system_cpi_pinocchio(call, handler, spec),
        // Pinocchio generic CPI not implemented (raw invoke_signed).
        (Target::Pinocchio, false, false) => None,
    };

    match emitted {
        Some(code) if pda_signers.is_empty() || code_signs_for_pda(&code) => {
            CpiPlan::Complete(code)
        }
        _ if !pda_signers.is_empty() => CpiPlan::AgentFill(CpiFillReason::PdaSigner {
            accounts: pda_signers,
        }),
        _ => CpiPlan::AgentFill(CpiFillReason::Unsupported { target }),
    }
}

/// Whether emitted CPI code signs for a program-owned PDA. Checked on the
/// artifact itself (not re-derived from dispatch) so no emitter — present or
/// future — can ship an unsigned invocation for a PDA signer.
fn code_signs_for_pda(code: &str) -> bool {
    code.contains("new_with_signer") || code.contains("invoke_signed")
}

/// Try to emit a real CPI invocation for one `call Interface.handler(...)`
/// site. Dispatch on `(target, is_spl_token, is_system)`:
///
/// - Anchor    + SPL Token → `anchor_spl::token::*` builder shape
/// - Anchor    + System    → `anchor_lang::system_program::*` builder shape
/// - Anchor    + generic   → `solana_program::program::invoke` shape
/// - Quasar    + SPL Token → `quasar_spl::TokenCpi` method chain
/// - Quasar    + System    → `Program<SystemProgram>` method chain
/// - Pinocchio + SPL Token → `pinocchio_token::instructions::*` struct + invoke
/// - Pinocchio + System    → `pinocchio_system::instructions::*` struct + invoke
///
/// Returns `None` for unrecognized interfaces/handlers and unimplemented
/// branches (Quasar/Pinocchio generic) — the caller falls back to a
/// structured comment + `todo!()` for the agent to fill.
#[cfg(test)]
pub(crate) fn try_emit_cpi(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    target: Target,
) -> Option<String> {
    match plan_cpi(call, handler, spec, target) {
        CpiPlan::Complete(code) => Some(code),
        CpiPlan::AgentFill(_) => None,
    }
}

/// SPL Token dispatcher. Routes to the right `anchor_spl::token` helper
/// per the called handler's name. Returns None on unrecognized handlers
/// (the caller falls back to comment + `todo!()`).
pub(crate) fn emit_spl_token_cpi_anchor(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Option<String> {
    let token_program_acct = find_token_program_account(handler)?;
    let prog_name = &token_program_acct.name;

    match call.target_handler.as_str() {
        "transfer" => emit_anchor_builder_cpi(
            call,
            handler,
            spec,
            "anchor_spl::token",
            prog_name,
            "Transfer",
            &[("from", "from"), ("to", "to"), ("authority", "authority")],
            Some("amount"),
            "transfer",
        ),
        "mint_to" => emit_anchor_builder_cpi(
            call,
            handler,
            spec,
            "anchor_spl::token",
            prog_name,
            "MintTo",
            &[
                ("mint", "mint"),
                ("to", "to"),
                // anchor_spl's MintTo says `authority`; the qedspec
                // interface says `mint_authority` (SPL docs). Map here.
                ("authority", "mint_authority"),
            ],
            Some("amount"),
            "mint_to",
        ),
        "burn" => emit_anchor_builder_cpi(
            call,
            handler,
            spec,
            "anchor_spl::token",
            prog_name,
            "Burn",
            &[
                ("mint", "mint"),
                ("from", "from"),
                ("authority", "authority"),
            ],
            Some("amount"),
            "burn",
        ),
        "initialize_account" => emit_anchor_builder_cpi(
            call,
            handler,
            spec,
            "anchor_spl::token",
            prog_name,
            "InitializeAccount",
            &[
                ("account", "account"),
                ("mint", "mint"),
                // anchor_spl's InitializeAccount says `authority`; the
                // qedspec interface says `owner` (SPL docs). Map here.
                ("authority", "owner"),
                ("rent", "rent"),
            ],
            None,
            "initialize_account",
        ),
        "close_account" => emit_anchor_builder_cpi(
            call,
            handler,
            spec,
            "anchor_spl::token",
            prog_name,
            "CloseAccount",
            &[
                ("account", "account"),
                ("destination", "destination"),
                ("authority", "authority"),
            ],
            None,
            "close_account",
        ),
        _ => None,
    }
}

/// Find the handler-side `<name> : program` account that points at the
/// token program. Convention: any `is_program` account named
/// `token_program`, or the unique `is_program` account otherwise.
pub(crate) fn find_token_program_account(
    handler: &ParsedHandler,
) -> Option<&crate::check::ParsedHandlerAccount> {
    handler
        .accounts
        .iter()
        .find(|a| a.is_program && a.name == "token_program")
        .or_else(|| {
            let programs: Vec<_> = handler.accounts.iter().filter(|a| a.is_program).collect();
            // .then(...) is lazy; .then_some(programs[0]) would evaluate
            // the index even when len is 0 and panic.
            (programs.len() == 1).then(|| programs[0])
        })
}

// ----------------------------------------------------------------------------
// v2.9 G3 — generic Anchor CPI codegen
// ----------------------------------------------------------------------------

/// Compute Anchor's instruction discriminator for a handler:
/// `Sha256("global:<handler_name>")[..8]`. This is the on-the-wire byte
/// prefix every Anchor instruction starts with — matches `anchor-lang`'s
/// `Discriminator` derive macro.
pub(crate) fn anchor_sighash(handler_name: &str) -> [u8; 8] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(format!("global:{}", handler_name).as_bytes());
    let result = hasher.finalize();
    let mut sighash = [0u8; 8];
    sighash.copy_from_slice(&result[..8]);
    sighash
}

/// Find the handler-side `<name> : program` account that points at a
/// non-SPL-Token target. Convention (mirrors `find_token_program_account`):
///   1. Prefer an account named `<interface_name_snake>_program`
///      (e.g. interface `MyAmm` → handler account `my_amm_program`).
///   2. Fall back to the unique `is_program` account if exactly one
///      exists (excluding any account named `token_program`, which is
///      reserved for SPL Token interactions and would only confuse a
///      generic-CPI dispatch).
///   3. Otherwise None — caller emits comment + `todo!()`.
pub(crate) fn find_program_account_for_interface<'a>(
    handler: &'a ParsedHandler,
    iface_name: &str,
) -> Option<&'a crate::check::ParsedHandlerAccount> {
    let expected_name = format!("{}_program", to_snake_case(iface_name));
    handler
        .accounts
        .iter()
        .find(|a| a.is_program && a.name == expected_name)
        .or_else(|| {
            let programs: Vec<_> = handler
                .accounts
                .iter()
                .filter(|a| a.is_program && a.name != "token_program")
                .collect();
            // .then(...) is lazy; .then_some(programs[0]) would evaluate
            // the index even when len is 0 and panic.
            (programs.len() == 1).then(|| programs[0])
        })
}

/// Convert PascalCase to snake_case — maps an interface name (`MyAmm`) to
/// its conventional program account name (`my_amm_program`).
pub(crate) fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && c.is_ascii_uppercase() {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// Emit a generic `solana_program::program::invoke` CPI shape (sighash +
/// Borsh-serialized args + AccountMeta vec in interface-declared order)
/// for any Anchor program that isn't SPL Token. Returns None when the
/// called handler isn't declared in the interface or no program account
/// is reachable in the calling handler (caller falls back to comment +
/// `todo!()`).
pub(crate) fn emit_generic_cpi_anchor(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    iface: &crate::check::ParsedInterface,
    spec: &ParsedSpec,
) -> Option<String> {
    let program_id = iface.program_id.as_deref()?;
    let iface_handler = iface
        .handlers
        .iter()
        .find(|h| h.name == call.target_handler)?;
    let program_acct = find_program_account_for_interface(handler, &iface.name)?;

    let sighash = anchor_sighash(&call.target_handler);
    let sighash_literal = sighash
        .iter()
        .map(|b| format!("0x{:02x}", b))
        .collect::<Vec<_>>()
        .join(", ");

    // Collect (interface account name → caller's rust_expr at the call
    // site) so each AccountMeta and AccountInfo entry can address the
    // caller-side handler account.
    let arg_account_lookup: std::collections::HashMap<&str, &str> = call
        .args
        .iter()
        .filter(|a| iface_handler.accounts.iter().any(|ia| ia.name == a.name))
        .map(|a| (a.name.as_str(), a.rust_expr.as_str()))
        .collect();

    let mut out = String::new();
    out.push_str("        {\n");
    out.push_str(&format!(
        "            // Generic Anchor CPI to {}.{} (v2.9 G3).\n",
        iface.name, call.target_handler,
    ));
    out.push_str("            use anchor_lang::prelude::*;\n");
    out.push_str("            use anchor_lang::solana_program::program::invoke;\n");
    out.push_str(
        "            use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};\n",
    );
    out.push_str("            use anchor_lang::AnchorSerialize;\n\n");

    // Discriminator + Borsh-serialized handler params.
    out.push_str(&format!(
        "            let mut ix_data: Vec<u8> = vec![{}];\n",
        sighash_literal,
    ));
    for (param_name, _) in &iface_handler.params {
        let arg = call.args.iter().find(|a| &a.name == param_name)?;
        let resolved = resolve_call_arg_for_amount(&arg.rust_expr, handler, spec);
        out.push_str(&format!(
            "            AnchorSerialize::serialize(&{}, &mut ix_data).map_err(|_| ProgramError::Custom(0))?;\n",
            resolved,
        ));
    }
    out.push('\n');

    // AccountMeta vec, in interface-declared order. Match writable / signer
    // role flags from the interface declaration.
    out.push_str("            let accounts = vec![\n");
    for ia in &iface_handler.accounts {
        let caller_acct = arg_account_lookup.get(ia.name.as_str())?;
        let constructor = if ia.is_writable {
            "AccountMeta::new"
        } else {
            "AccountMeta::new_readonly"
        };
        out.push_str(&format!(
            "                {}(self.{}.key(), {}),\n",
            constructor, caller_acct, ia.is_signer,
        ));
    }
    out.push_str("            ];\n\n");

    out.push_str("            let ix = Instruction {\n");
    // `Pubkey::from_str`, not the `pubkey!` macro — the macro lives at
    // different paths across Anchor versions; `from_str` is stable on
    // the `Pubkey` type itself.
    out.push_str(&format!(
        "                program_id: <anchor_lang::prelude::Pubkey as std::str::FromStr>::from_str(\"{}\").unwrap(),\n",
        program_id,
    ));
    out.push_str("                accounts,\n");
    out.push_str("                data: ix_data,\n");
    out.push_str("            };\n\n");

    out.push_str("            invoke(&ix, &[\n");
    for ia in &iface_handler.accounts {
        let caller_acct = arg_account_lookup.get(ia.name.as_str())?;
        out.push_str(&format!(
            "                self.{}.to_account_info(),\n",
            caller_acct,
        ));
    }
    out.push_str(&format!(
        "                self.{}.to_account_info(),\n",
        program_acct.name,
    ));
    out.push_str("            ])?;\n");

    // v2.24 #11 — when the caller wrote `let X = call …` and the
    // interface declares a return type, capture the callee's return
    // data via Solana's `get_return_data` syscall + Borsh decode.
    // The block opens with `let X = { … }` instead of bare `{ … }`,
    // and the closing emits the binding's value as the block's
    // result. Without a declared return type, the binding name is
    // dropped (matched at the caller side via lint warning at
    // adapt time — TODO v2.25.1).
    if let (Some(bind), Some(ret_dsl_type)) = (&call.result_binding, &iface_handler.return_type) {
        let ret_rust = match map_type(ret_dsl_type, spec) {
            Ok(t) => t,
            Err(_) => return None,
        };
        out.push_str("            use anchor_lang::AnchorDeserialize;\n");
        out.push_str("            use anchor_lang::solana_program::program::get_return_data;\n");
        out.push_str("            use anchor_lang::solana_program::program_error::ProgramError;\n");
        out.push_str(
            "            let (_, ret_bytes) = get_return_data().ok_or(ProgramError::InvalidAccountData)?;\n",
        );
        out.push_str(&format!(
            "            <{ret} as AnchorDeserialize>::deserialize(&mut ret_bytes.as_slice()).map_err(|_| ProgramError::InvalidAccountData)?\n",
            ret = ret_rust,
        ));
        // Close the block and use it as the RHS of the let-binding.
        out.push_str("        };\n");
        // Prepend the `let <bind> = ` to the front of `out` (after
        // the leading indent), since we opened with `        {` and
        // the binding wants `let X = {`.
        let prefix = format!("        let {} = {{\n", bind);
        let body_without_open = out
            .strip_prefix("        {\n")
            .map(String::from)
            .unwrap_or(out.clone());
        return Some(format!("{}{}", prefix, body_without_open));
    }

    out.push_str("        }\n");
    Some(out)
}

/// Emit one Anchor context-builder CPI body — `use <module>::{self,
/// <Struct>}; let cpi_accounts = <Struct> { … }; <alias>::<fn>(
/// CpiContext::new(cpi_program, cpi_accounts)[, <scalar>])?;`. Serves
/// both the SPL Token (`anchor_spl::token`) and System
/// (`anchor_lang::system_program`) shapes: the builder is identical,
/// only the module path and the program account differ.
///
/// `field_to_arg` is `(anchor_field_name, call_arg_name)` pairs. The arg
/// name is the call-site identifier (matches the qedspec interface's
/// account block); the anchor field name is what the target module's
/// accounts struct expects. Most are identity (`("from", "from")`) but
/// some interfaces expose a more semantic name than anchor uses
/// (e.g. `mint_authority` vs `authority`). `scalar_arg` is the optional
/// trailing scalar (e.g. `amount` for transfer / mint_to / burn; absent
/// for initialize_account / close_account).
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_anchor_builder_cpi(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    module_path: &str,
    program_account: &str,
    accounts_struct: &str,
    field_to_arg: &[(&str, &str)],
    scalar_arg: Option<&str>,
    fn_name: &str,
) -> Option<String> {
    // The invocation prefix is the module's trailing segment (`token`,
    // `system_program`) — brought into scope by the `use …::{self, …}`.
    let module_alias = module_path.rsplit("::").next().unwrap_or(module_path);

    // Resolve every account argument via the call site.
    let mut acct_lines: Vec<String> = Vec::with_capacity(field_to_arg.len());
    let max_field = field_to_arg.iter().map(|(f, _)| f.len()).max().unwrap_or(0);
    for (anchor_field, call_arg) in field_to_arg {
        let arg = call.args.iter().find(|a| a.name == *call_arg)?;
        let pad = " ".repeat(max_field - anchor_field.len());
        acct_lines.push(format!(
            "                {}:{} self.{}.to_account_info(),\n",
            anchor_field, pad, arg.rust_expr
        ));
    }

    // Resolve the optional scalar arg (e.g. `amount`).
    let scalar_rhs = match scalar_arg {
        Some(name) => {
            let arg = call.args.iter().find(|a| a.name == name)?;
            Some(resolve_call_arg_for_amount(&arg.rust_expr, handler, spec))
        }
        None => None,
    };

    // When the CPI's `authority` is a PDA of THIS program, the program
    // must sign for it with `invoke_signed` — i.e.
    // `CpiContext::new_with_signer(.., signer_seeds)`. Plain `new` fails
    // at runtime with a missing-signer / privilege error. The seeds are
    // the same ones the PDA account's `#[account(seeds = …, bump)]`
    // constraint uses, so codegen can assemble them deterministically.
    let signer = anchor_pda_signer_seeds(call, field_to_arg, handler);

    let mut out = String::new();
    out.push_str("        {\n");
    out.push_str(&format!(
        "            use {}::{{self, {}}};\n",
        module_path, accounts_struct
    ));
    out.push_str(&format!(
        "            let cpi_accounts = {} {{\n",
        accounts_struct
    ));
    for line in &acct_lines {
        out.push_str(line);
    }
    out.push_str("            };\n");
    out.push_str(&format!(
        "            let cpi_program = self.{}.to_account_info();\n",
        program_account
    ));
    let cpi_ctx = match &signer {
        Some(sig) => {
            out.push_str(&sig.preamble);
            format!(
                "CpiContext::new_with_signer(cpi_program, cpi_accounts, {})",
                sig.signer_var
            )
        }
        None => "CpiContext::new(cpi_program, cpi_accounts)".to_string(),
    };
    let invocation = match scalar_rhs {
        Some(rhs) => format!(
            "            {}::{}({}, {})?;\n",
            module_alias, fn_name, cpi_ctx, rhs
        ),
        None => format!("            {}::{}({})?;\n", module_alias, fn_name, cpi_ctx),
    };
    out.push_str(&invocation);
    out.push_str("        }\n");
    Some(out)
}

/// Assembled `invoke_signed` seeds for a CPI whose `authority` is a
/// program PDA: the `let … = …;` preamble to place before the call and
/// the name of the `&[&[&[u8]]]` signer-seeds binding.
pub(crate) struct PdaSignerSeeds {
    pub(crate) preamble: String,
    pub(crate) signer_var: String,
}

/// If the call's `authority` argument names a handler account that is a
/// program PDA, assemble its `invoke_signed` signer seeds (same seeds as
/// the account's `#[account(seeds = …, bump)]` constraint, plus its
/// bump). `None` when there's no authority arg, it isn't a PDA, or a seed
/// has a shape we can't render safely (the caller then keeps plain
/// `CpiContext::new` rather than emit wrong seeds).
fn anchor_pda_signer_seeds(
    call: &crate::check::ParsedCall,
    field_to_arg: &[(&str, &str)],
    handler: &ParsedHandler,
) -> Option<PdaSignerSeeds> {
    // Resolve the account bound to the `authority` anchor field.
    let (_, authority_arg) = field_to_arg
        .iter()
        .find(|(field, _)| *field == "authority")?;
    let arg = call.args.iter().find(|a| a.name == *authority_arg)?;
    let authority_name = arg.rust_expr.as_str();
    let acct = handler.accounts.iter().find(|a| a.name == authority_name)?;
    let seeds = acct.pda_seeds.as_ref()?;

    let bound_account_names: std::collections::HashSet<&str> =
        handler.accounts.iter().map(|a| a.name.as_str()).collect();

    // Render each seed into an `&[u8]` expression, binding account-key
    // locals up front (the impl body reads accounts as `self.<name>`,
    // and `self.<name>.key()` is an owned `Pubkey` temporary that must be
    // bound before `.as_ref()` borrows it — otherwise E0716).
    let mut preamble = String::new();
    let mut seed_exprs: Vec<String> = Vec::new();
    let mut bound_keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for seed in seeds {
        if let Some(inner) = seed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            seed_exprs.push(format!("b\"{}\"", inner));
        } else if bound_account_names.contains(seed.as_str()) {
            let local = format!("__{}_key", seed);
            if bound_keys.insert(seed.clone()) {
                preamble.push_str(&format!(
                    "            let {} = self.{}.key();\n",
                    local, seed
                ));
            }
            seed_exprs.push(format!("{}.as_ref()", local));
        } else {
            // State-field or otherwise non-account seed — assembling it
            // safely here isn't reliable; bail so the caller keeps plain
            // `new` (a compile-clean fallback) rather than emit wrong
            // seeds.
            return None;
        }
    }

    let signer_var = format!("__{}_signer_seeds", authority_name);
    preamble.push_str(&format!(
        "            let {}: &[&[&[u8]]] = &[&[{}, &[bumps.{}]]];\n",
        signer_var,
        seed_exprs.join(", "),
        authority_name
    ));

    Some(PdaSignerSeeds {
        preamble,
        signer_var,
    })
}

/// Anchor System Program dispatcher. `transfer` gets the idiomatic
/// `anchor_lang::system_program::transfer(CpiContext::new(...), lamports)`
/// shape (mirroring the SPL Token path); `create_account` / `assign`
/// fall back to the generic `solana_program::program::invoke` builder —
/// PDA creation needs `invoke_signed` + seed assembly, which is CPI
/// authority business logic the agent fills, not deterministic codegen
/// (the same boundary that keeps `transfers {}` agent-filled).
pub(crate) fn emit_system_cpi_anchor(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    iface: &crate::check::ParsedInterface,
    spec: &ParsedSpec,
) -> Option<String> {
    if call.target_handler == "transfer" {
        // The CpiContext needs the System program account info; resolve it
        // by the `system_program` convention. If it isn't reachable, fall
        // through to the generic invoke shape (which builds the program id
        // inline) rather than fail.
        if let Some(sp) = find_program_account_for_interface(handler, &call.target_interface) {
            let name = sp.name.clone();
            return emit_anchor_builder_cpi(
                call,
                handler,
                spec,
                "anchor_lang::system_program",
                &name,
                "Transfer",
                &[("from", "from"), ("to", "to")],
                Some("amount"),
                "transfer",
            );
        }
    }
    emit_generic_cpi_anchor(call, handler, iface, spec)
}

/// Quasar SPL Token dispatcher: `transfer`, `mint_to`, `burn`,
/// `close_account` via `quasar_spl::TokenCpi`. `initialize_account` stays
/// `None` — `quasar-spl` exposes only `initialize_account3`, whose
/// `owner: &Address` positional is a raw key (not an account view) and
/// which omits the rent sysvar, so it doesn't fit the uniform
/// `emit_quasar_method_chain` helper.
pub(crate) fn emit_spl_token_cpi_quasar(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Option<String> {
    let token_program_acct = find_token_program_account(handler)?;
    let prog_name = &token_program_acct.name;

    match call.target_handler.as_str() {
        "transfer" => emit_quasar_method_chain(
            call,
            handler,
            spec,
            prog_name,
            "transfer",
            // Quasar's TokenCpi::transfer signature is
            // (from, to, authority, amount). Spec arg names must match
            // the canonical SPL Token interface declared in the spec.
            &["from", "to", "authority"],
            Some("amount"),
        ),
        "mint_to" => emit_quasar_method_chain(
            call,
            handler,
            spec,
            prog_name,
            "mint_to",
            // TokenCpi::mint_to(mint, to, authority, amount). The spec's
            // canonical SPL interface names the authority slot
            // `mint_authority`; it resolves positionally into the trait's
            // `authority` parameter.
            &["mint", "to", "mint_authority"],
            Some("amount"),
        ),
        "burn" => emit_quasar_method_chain(
            call,
            handler,
            spec,
            prog_name,
            "burn",
            // TokenCpi::burn(from, mint, authority, amount).
            &["from", "mint", "authority"],
            Some("amount"),
        ),
        "close_account" => emit_quasar_method_chain(
            call,
            handler,
            spec,
            prog_name,
            "close_account",
            // TokenCpi::close_account(account, destination, authority) —
            // no scalar.
            &["account", "destination", "authority"],
            None,
        ),
        // `initialize_account` has no uniform Quasar shape (see doc);
        // falls through to the caller's `todo!()`.
        _ => None,
    }
}

/// Emit one Quasar CPI as a single-line method chain on a program
/// account: `self.<prog>.<method>(&self.<a>, …, scalar).invoke()?;`.
/// The shape comes from `quasar-spl-0.0.0`'s `TokenCpi` trait (sibling
/// shape to the Anchor builder); `quasar-lang`'s
/// `Program<SystemProgram>` exposes the identical chain, so the System
/// dispatcher (`emit_system_cpi_quasar`) reuses this emitter.
///
/// `account_args_in_order` carries the call-site arg names in the order
/// the Quasar method expects (e.g. `["from", "to", "authority"]`
/// for `transfer`). The function looks each one up in `call.args` and
/// resolves it to `&self.<rust_expr>`. Unrecognized arg names short-
/// circuit to `None`.
pub(crate) fn emit_quasar_method_chain(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    program_account: &str,
    method_name: &str,
    account_args_in_order: &[&str],
    scalar_arg: Option<&str>,
) -> Option<String> {
    let mut args: Vec<String> = Vec::with_capacity(account_args_in_order.len() + 1);
    for call_arg in account_args_in_order {
        let arg = call.args.iter().find(|a| a.name == *call_arg)?;
        args.push(format!("&self.{}", arg.rust_expr));
    }
    if let Some(name) = scalar_arg {
        let arg = call.args.iter().find(|a| a.name == name)?;
        args.push(resolve_call_arg_for_amount(&arg.rust_expr, handler, spec));
    }
    Some(format!(
        "        self.{}.{}({}).invoke()?;\n",
        program_account,
        method_name,
        args.join(", ")
    ))
}

/// Dispatch a `call System.<handler>(...)` site to its Quasar
/// `Program<SystemProgram>` method-chain shape. Only `transfer` is
/// mechanized (same boundary as the Anchor / Pinocchio System emitters):
/// `create_account` / `assign` take an `&Address` owner whose resolution
/// from a spec `Pubkey` arg is a follow-on, so they return `None`.
pub(crate) fn emit_system_cpi_quasar(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Option<String> {
    let sys_prog = find_program_account_for_interface(handler, &call.target_interface)?;
    let prog_name = &sys_prog.name;
    match call.target_handler.as_str() {
        // Program<SystemProgram>::transfer(from, to, lamports) — the
        // spec's `amount` arg binds the `lamports` positional.
        "transfer" => emit_quasar_method_chain(
            call,
            handler,
            spec,
            prog_name,
            "transfer",
            &["from", "to"],
            Some("amount"),
        ),
        _ => None,
    }
}

/// Pinocchio SPL Token dispatcher — all five canonical SPL handlers via
/// `pinocchio_token::instructions::*`. Field-name divergences from
/// canonical SPL naming (`MintTo.account`, `MintTo.mint_authority`,
/// `Burn.account`, `InitializeAccount.rent_sysvar`) are handled per-arm
/// in the `(pinocchio_field, spec_arg)` map. Returns None on
/// unrecognized handlers (caller falls back to comment + `todo!()`).
pub(crate) fn emit_spl_token_cpi_pinocchio(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Option<String> {
    match call.target_handler.as_str() {
        "transfer" => emit_pinocchio_struct_cpi(
            call,
            handler,
            spec,
            "pinocchio_token",
            "Transfer",
            // (pinocchio_struct_field, spec_arg_name)
            &[("from", "from"), ("to", "to"), ("authority", "authority")],
            &[("amount", "amount")],
        ),
        "mint_to" => emit_pinocchio_struct_cpi(
            call,
            handler,
            spec,
            "pinocchio_token",
            "MintTo",
            // pinocchio-token's MintTo names the recipient `account`
            // (canonical SPL `to`) and the signer `mint_authority`.
            &[
                ("mint", "mint"),
                ("account", "to"),
                ("mint_authority", "mint_authority"),
            ],
            &[("amount", "amount")],
        ),
        "burn" => emit_pinocchio_struct_cpi(
            call,
            handler,
            spec,
            "pinocchio_token",
            "Burn",
            // Burn names the source slot `account` (canonical SPL `from`).
            &[
                ("account", "from"),
                ("mint", "mint"),
                ("authority", "authority"),
            ],
            &[("amount", "amount")],
        ),
        "initialize_account" => emit_pinocchio_struct_cpi(
            call,
            handler,
            spec,
            "pinocchio_token",
            "InitializeAccount",
            // InitializeAccount names the rent sysvar `rent_sysvar`
            // (canonical SPL `rent`). No scalar.
            &[
                ("account", "account"),
                ("mint", "mint"),
                ("owner", "owner"),
                ("rent_sysvar", "rent"),
            ],
            &[],
        ),
        "close_account" => emit_pinocchio_struct_cpi(
            call,
            handler,
            spec,
            "pinocchio_token",
            "CloseAccount",
            &[
                ("account", "account"),
                ("destination", "destination"),
                ("authority", "authority"),
            ],
            &[],
        ),
        _ => None,
    }
}

/// Emit one `<crate>::instructions::<Struct> { … }.invoke()?;` Pinocchio
/// CPI — serves both `pinocchio_token` and `pinocchio_system` (the
/// struct-literal + `.invoke()` shape is identical; only the crate path
/// differs).
///
/// `account_fields` map struct fields holding `&AccountInfo` to call-site
/// arg names (emitted as `self.<arg>`), absorbing naming divergences from
/// canonical SPL at the codegen boundary (mirroring
/// `emit_anchor_builder_cpi`); `scalar_fields` map scalar struct fields
/// to call args, resolved through `resolve_call_arg_for_amount` (System's
/// `Transfer.lamports` binds the spec's `amount` arg).
pub(crate) fn emit_pinocchio_struct_cpi(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    crate_path: &str,
    struct_name: &str,
    account_fields: &[(&str, &str)],
    scalar_fields: &[(&str, &str)],
) -> Option<String> {
    let max_field = account_fields
        .iter()
        .chain(scalar_fields.iter())
        .map(|(f, _)| f.len())
        .max()
        .unwrap_or(0);

    let mut out = String::new();
    out.push_str("        ");
    out.push_str(crate_path);
    out.push_str("::instructions::");
    out.push_str(struct_name);
    out.push_str(" {\n");
    for (struct_field, call_arg) in account_fields {
        let arg = call.args.iter().find(|a| a.name == *call_arg)?;
        let pad = " ".repeat(max_field - struct_field.len());
        // The Pinocchio handler struct stores each account as
        // `&'a AccountInfo`, and the CPI structs take `&'a AccountInfo`
        // fields — so `self.<acct>` is already the right type. (A
        // leading `&` would yield `&&AccountInfo`.)
        out.push_str(&format!(
            "            {}:{} self.{},\n",
            struct_field, pad, arg.rust_expr
        ));
    }
    for (struct_field, call_arg) in scalar_fields {
        let arg = call.args.iter().find(|a| a.name == *call_arg)?;
        let rhs = resolve_call_arg_for_amount(&arg.rust_expr, handler, spec);
        let pad = " ".repeat(max_field - struct_field.len());
        out.push_str(&format!("            {}:{} {},\n", struct_field, pad, rhs));
    }
    out.push_str("        }.invoke()?;\n");
    Some(out)
}

/// Dispatch a `call System.<handler>(...)` site to its
/// `pinocchio_system::instructions::*` shape. Only `transfer` is
/// mechanized; `create_account` / `assign` take a `&Pubkey` `owner`
/// whose resolution from a spec `Pubkey` arg is a follow-on, so they
/// return `None` rather than emit code that may not type-check.
pub(crate) fn emit_system_cpi_pinocchio(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Option<String> {
    match call.target_handler.as_str() {
        "transfer" => emit_pinocchio_struct_cpi(
            call,
            handler,
            spec,
            "pinocchio_system",
            "Transfer",
            // (pinocchio_system struct field, spec call-site arg)
            &[("from", "from"), ("to", "to")],
            &[("lamports", "amount")],
        ),
        _ => None,
    }
}
