//! AST inference over handler bodies: recovers accounts, roles, params,
//! PDA derivations, dispatch tags, and verified stubs. Files that fail
//! `syn::parse_file` are a hard error at the `mod.rs` accumulation seam —
//! there is deliberately no lossy text-scraping fallback.

use super::*;

/// Shared walker + the profile-specific exclusion of generated
/// `kani_impl.rs` harnesses (they'd be inferred as handlers).
pub(super) fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    out.extend(
        crate::fs_walk::collect_rs_files(dir, crate::fs_walk::DEFAULT_SKIP_DIRS)
            .into_iter()
            .filter(|p| p.file_name().and_then(|n| n.to_str()) != Some("kani_impl.rs")),
    );
}

pub(super) fn collect_item_fns(items: &[Item]) -> Vec<&ItemFn> {
    let mut out = Vec::new();
    for item in items {
        match item {
            Item::Fn(item_fn) => out.push(item_fn),
            Item::Mod(item_mod) => {
                if let Some((_brace, items)) = &item_mod.content {
                    out.extend(collect_item_fns(items));
                }
            }
            _ => {}
        }
    }
    out
}

pub(super) fn item_fn_has_kani_contract(item_fn: &ItemFn) -> bool {
    item_fn.attrs.iter().any(|attr| {
        let path = attr.path();
        if path.is_ident("requires") || path.is_ident("ensures") || path.is_ident("modifies") {
            return true;
        }
        let tokens = attr.to_token_stream().to_string();
        tokens.contains("kani :: requires")
            || tokens.contains("kani::requires")
            || tokens.contains("kani :: ensures")
            || tokens.contains("kani::ensures")
            || tokens.contains("kani :: modifies")
            || tokens.contains("kani::modifies")
    })
}

pub(super) fn crate_fn_path(src_dir: &Path, file_path: &Path, fn_name: &str) -> String {
    let rel = file_path.strip_prefix(src_dir).unwrap_or(file_path);
    let mut modules = Vec::new();
    for component in rel.components() {
        let Some(part) = component.as_os_str().to_str() else {
            continue;
        };
        let part = part.trim_end_matches(".rs");
        if matches!(part, "lib" | "main" | "mod") {
            continue;
        }
        modules.push(part.replace('-', "_"));
    }
    if modules.is_empty() {
        format!("crate::{fn_name}")
    } else {
        format!("crate::{}::{fn_name}", modules.join("::"))
    }
}

pub(super) fn infer_verified_stubs_from_block(
    block: &syn::Block,
    contracted_fns: &BTreeMap<String, String>,
    call_graph: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    verified_stubs_for_calls(
        infer_called_fn_names_from_stmts(&block.stmts),
        contracted_fns,
        call_graph,
    )
}

pub(super) fn infer_called_fn_names_from_block(block: &syn::Block) -> Vec<String> {
    infer_called_fn_names_from_stmts(&block.stmts)
}

fn infer_called_fn_names_from_stmts(stmts: &[Stmt]) -> Vec<String> {
    let mut calls = Vec::new();
    walk_exprs_in_stmts(stmts, &mut |expr| {
        let Expr::Call(call) = expr else {
            return;
        };
        let Some(name) = call_name(&call.func) else {
            return;
        };
        if !calls.contains(&name) {
            calls.push(name);
        }
    });
    calls
}

fn verified_stubs_for_calls(
    calls: Vec<String>,
    contracted_fns: &BTreeMap<String, String>,
    call_graph: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    let mut stubs = Vec::new();
    let mut stack = calls;
    let mut seen = std::collections::BTreeSet::new();
    while let Some(name) = stack.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(path) = contracted_fns.get(&name) {
            if !stubs.contains(path) {
                stubs.push(path.clone());
            }
        }
        if let Some(callees) = call_graph.get(&name) {
            stack.extend(callees.iter().cloned());
        }
    }
    stubs.sort();
    stubs
}

pub(super) fn process_handler_name(item_fn: &ItemFn) -> Option<String> {
    let name = item_fn.sig.ident.to_string();
    name.strip_prefix("process_")
        .filter(|handler| *handler != "instruction")
        .map(ToOwned::to_owned)
}

pub(super) fn infer_accounts_from_block(block: &syn::Block) -> Vec<String> {
    let mut accounts = Vec::new();
    collect_accounts_from_stmts(&block.stmts, &mut accounts);
    accounts
}

fn collect_accounts_from_stmts(stmts: &[Stmt], accounts: &mut Vec<String>) {
    for stmt in stmts {
        if let Stmt::Local(local) = stmt {
            if let Some(from_destructure) = accounts_from_destructure_pat(&local.pat) {
                if !from_destructure.is_empty() {
                    *accounts = from_destructure;
                    return;
                }
            }
            if local_init_calls(&local.init, "next_account_info") {
                if let Some(name) = simple_pat_ident(&local.pat) {
                    accounts.push(name);
                }
            }
        }
        if let Some(expr) = stmt_expr(stmt) {
            collect_accounts_from_expr(expr, accounts);
        }
    }
}

fn collect_accounts_from_expr(expr: &Expr, accounts: &mut Vec<String>) {
    match expr {
        Expr::Block(block) => collect_accounts_from_stmts(&block.block.stmts, accounts),
        Expr::If(expr_if) => {
            collect_accounts_from_stmts(&expr_if.then_branch.stmts, accounts);
            if let Some((_else, else_expr)) = &expr_if.else_branch {
                collect_accounts_from_expr(else_expr, accounts);
            }
        }
        Expr::Match(expr_match) => {
            for arm in &expr_match.arms {
                collect_accounts_from_expr(&arm.body, accounts);
            }
        }
        _ => {}
    }
}

fn accounts_from_destructure_pat(pat: &Pat) -> Option<Vec<String>> {
    let Pat::Slice(slice) = pat else {
        return None;
    };
    let mut accounts = Vec::new();
    for elem in &slice.elems {
        match elem {
            Pat::Ident(ident) => accounts.push(normalize_schema_name(&ident.ident.to_string())),
            Pat::Rest(_) => break,
            _ => return None,
        }
    }
    Some(accounts)
}

pub(super) fn infer_account_roles_from_block(
    block: &syn::Block,
    accounts: &[String],
) -> BTreeMap<String, PinocchioAccountRole> {
    let mut roles = BTreeMap::<String, PinocchioAccountRole>::new();
    walk_exprs_in_stmts(&block.stmts, &mut |expr| {
        infer_role_from_expr(expr, accounts, &mut roles);
    });
    roles.retain(|_, role| !role.is_empty());
    roles
}

fn infer_role_from_expr(
    expr: &Expr,
    accounts: &[String],
    roles: &mut BTreeMap<String, PinocchioAccountRole>,
) {
    match expr {
        Expr::MethodCall(call) => {
            let receiver = normalize_expr_tokens(&call.receiver);
            let account = normalize_schema_name(&receiver);
            if accounts.iter().any(|candidate| candidate == &account) {
                let role = roles.entry(account).or_default();
                match call.method.to_string().as_str() {
                    "is_signer" => role.is_signer = Some(true),
                    "is_writable" => role.is_writable = Some(true),
                    "is_executable" | "executable" => role.is_program = Some(true),
                    _ => {}
                }
            }
        }
        Expr::Call(call) => {
            let Some(fn_name) = call_name(&call.func) else {
                return;
            };
            let args: Vec<_> = call.args.iter().collect();
            match fn_name.as_str() {
                "require_key" if args.len() >= 2 => {
                    if let Some(account) = expr_ident(args[0]) {
                        if accounts.iter().any(|candidate| candidate == &account)
                            && expr_mentions_token_program(args[1])
                        {
                            let role = roles.entry(account).or_default();
                            role.is_program = Some(true);
                            role.account_type = Some("token".to_string());
                        }
                    }
                }
                "read_mint_decimals" | "from_mint_account" => {
                    if let Some(account) = args.first().and_then(|arg| expr_ident(arg)) {
                        let role = roles.entry(account).or_default();
                        role.account_type = Some("mint".to_string());
                    }
                }
                "require_token_account" | "read_token_amount" | "write_token_amount" => {
                    if let Some(account) = args.first().and_then(|arg| expr_ident(arg)) {
                        let role = roles.entry(account).or_default();
                        role.account_type = Some("token".to_string());
                    }
                }
                "from_account_info" => {
                    if let Some(account) = args.first().and_then(|arg| expr_ident(arg)) {
                        let rendered = normalize_expr_tokens(expr);
                        let role = roles.entry(account).or_default();
                        if rendered.contains("Mint") {
                            role.account_type = Some("mint".to_string());
                        } else if rendered.contains("TokenAccount") {
                            role.account_type = Some("token".to_string());
                        }
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

pub(super) fn infer_key_account_aliases_from_block(block: &syn::Block) -> BTreeMap<String, String> {
    let mut aliases = BTreeMap::new();
    walk_exprs_in_stmts(&block.stmts, &mut |expr| {
        if let Expr::Call(call) = expr {
            if call_name(&call.func).as_deref() == Some("require_key") && call.args.len() == 2 {
                let args: Vec<_> = call.args.iter().collect();
                if let (Some(account), Some(key)) = (expr_ident(args[0]), expr_ref_ident(args[1])) {
                    aliases.insert(normalize_schema_name(&key), normalize_schema_name(&account));
                }
            }
        }
    });
    aliases
}

pub(super) fn infer_local_key_derivations_from_block(
    block: &syn::Block,
) -> BTreeMap<String, PinocchioLocalKeyDerivation> {
    let mut derivations = BTreeMap::new();
    collect_local_key_derivations_from_stmts(&block.stmts, &mut derivations);
    derivations
}

fn collect_local_key_derivations_from_stmts(
    stmts: &[Stmt],
    out: &mut BTreeMap<String, PinocchioLocalKeyDerivation>,
) {
    for stmt in stmts {
        if let Stmt::Local(local) = stmt {
            if let (Some(name), Some(init)) = (simple_pat_ident(&local.pat), local.init.as_ref()) {
                if let Some(derivation) = derive_call_from_expr(&init.expr) {
                    out.insert(name, derivation);
                }
            }
        }
        if let Some(expr) = stmt_expr(stmt) {
            match expr {
                Expr::Block(block) => {
                    collect_local_key_derivations_from_stmts(&block.block.stmts, out)
                }
                Expr::If(expr_if) => {
                    collect_local_key_derivations_from_stmts(&expr_if.then_branch.stmts, out);
                    if let Some((_else, else_expr)) = &expr_if.else_branch {
                        if let Expr::Block(block) = &**else_expr {
                            collect_local_key_derivations_from_stmts(&block.block.stmts, out);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

pub(super) fn infer_account_key_derivations_from_block(
    block: &syn::Block,
    local_key_derivations: &BTreeMap<String, PinocchioLocalKeyDerivation>,
) -> BTreeMap<String, PinocchioLocalKeyDerivation> {
    let mut derivations = BTreeMap::new();
    walk_exprs_in_stmts(&block.stmts, &mut |expr| {
        let Expr::Call(call) = expr else {
            return;
        };
        if call_name(&call.func).as_deref() != Some("require_key") || call.args.len() != 2 {
            return;
        }
        let args: Vec<_> = call.args.iter().collect();
        let Some(account) = expr_ident(args[0]) else {
            return;
        };
        if let Some(derivation) = derive_call_from_expr(args[1]) {
            derivations.insert(account, derivation);
        } else if let Some(key_name) = expr_ref_ident(args[1]) {
            if let Some(local) = local_key_derivations.get(&key_name) {
                derivations.insert(account, local.clone());
            }
        }
    });
    derivations
}

pub(super) fn infer_token_account_bindings_from_block(
    block: &syn::Block,
    key_account_aliases: &BTreeMap<String, String>,
    local_key_derivations: &BTreeMap<String, PinocchioLocalKeyDerivation>,
) -> BTreeMap<String, PinocchioTokenAccountBinding> {
    let mut bindings = BTreeMap::new();
    walk_exprs_in_stmts(&block.stmts, &mut |expr| {
        let Expr::Call(call) = expr else {
            return;
        };
        let Some(fn_name) = call_name(&call.func) else {
            return;
        };
        let args: Vec<_> = call.args.iter().collect();
        match fn_name.as_str() {
            "require_token_account" if args.len() == 3 => {
                let Some(account) = expr_ident(args[0]) else {
                    return;
                };
                let mint_account =
                    expr_key_receiver(args[1]).map(|name| normalize_schema_name(&name));
                let owner_account = expr_key_receiver(args[2])
                    .map(|name| normalize_schema_name(&name))
                    .or_else(|| {
                        expr_ref_ident(args[2])
                            .and_then(|var| key_account_aliases.get(&var).cloned())
                    });
                let owner_key_derivation = expr_ref_ident(args[2])
                    .and_then(|var| local_key_derivations.get(&var).cloned());
                bindings.insert(
                    account,
                    PinocchioTokenAccountBinding {
                        mint_account,
                        owner_account,
                        owner_key_derivation,
                    },
                );
            }
            "require_matching_token_mint" | "require_token_mint" if args.len() == 2 => {
                let (Some(account), Some(mint)) = (expr_ident(args[0]), expr_key_receiver(args[1]))
                else {
                    return;
                };
                bindings
                    .entry(account)
                    .or_insert_with(|| PinocchioTokenAccountBinding {
                        mint_account: None,
                        owner_account: None,
                        owner_key_derivation: None,
                    })
                    .mint_account = Some(normalize_schema_name(&mint));
            }
            _ => {}
        }
    });
    bindings
}

pub(super) fn infer_mint_decimal_bindings_from_block(
    block: &syn::Block,
) -> BTreeMap<String, String> {
    let mut bindings = BTreeMap::new();
    for stmt in &block.stmts {
        let Stmt::Local(local) = stmt else {
            continue;
        };
        let Some(param) = simple_pat_ident(&local.pat).map(|name| normalize_schema_name(&name))
        else {
            continue;
        };
        let Some(init) = &local.init else {
            continue;
        };
        let Some(account) = read_mint_decimals_arg(&init.expr) else {
            continue;
        };
        bindings.insert(account, param);
    }
    bindings
}

fn read_mint_decimals_arg(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Try(expr_try) => read_mint_decimals_arg(&expr_try.expr),
        Expr::Call(call) => {
            let fn_name = call_name(&call.func)?;
            if fn_name != "read_mint_decimals" {
                return None;
            }
            call.args
                .first()
                .and_then(expr_ident)
                .map(|name| normalize_schema_name(&name))
        }
        _ => None,
    }
}

pub(super) fn infer_source_expr_aliases_from_block(block: &syn::Block) -> BTreeMap<String, String> {
    let mut aliases = BTreeMap::new();
    collect_source_expr_aliases_from_stmts(&block.stmts, &mut aliases);
    aliases
}

fn collect_source_expr_aliases_from_stmts(stmts: &[Stmt], aliases: &mut BTreeMap<String, String>) {
    for stmt in stmts {
        if let Stmt::Local(local) = stmt {
            let Some(name) = simple_pat_ident(&local.pat) else {
                continue;
            };
            let Some(init) = &local.init else {
                continue;
            };
            if let Some(value) = wrapper_ctor_arg(&init.expr) {
                aliases.insert(format!("{name}.0"), value);
            }
            if let Expr::Struct(expr_struct) = &*init.expr {
                for field in &expr_struct.fields {
                    let field_name = field.member.to_token_stream().to_string();
                    let value = normalize_ast_expr_alias(&field.expr);
                    if !value.is_empty() {
                        aliases.insert(
                            format!("{name}.{}", normalize_schema_name(&field_name)),
                            value.clone(),
                        );
                        aliases.insert(
                            format!("{name}.{}.0", normalize_schema_name(&field_name)),
                            value,
                        );
                    }
                }
            }
        }
    }
}

pub(super) fn infer_params_from_block(block: &syn::Block) -> Vec<PinocchioParamField> {
    let mut params = Vec::new();
    collect_params_from_stmts(&block.stmts, &mut params);
    params.sort_by_key(|p| p.start);
    params
}

fn collect_params_from_stmts(stmts: &[Stmt], params: &mut Vec<PinocchioParamField>) {
    for stmt in stmts {
        if let Stmt::Local(local) = stmt {
            if let (Some(name), Some(init)) = (simple_pat_ident(&local.pat), local.init.as_ref()) {
                if let Some((rust_type, start, end)) = from_le_bytes_instruction_slice(&init.expr) {
                    params.push(PinocchioParamField {
                        name,
                        rust_type,
                        start,
                        end,
                    });
                }
            }
        }
        if let Some(expr) = stmt_expr(stmt) {
            match expr {
                Expr::Block(block) => collect_params_from_stmts(&block.block.stmts, params),
                Expr::If(expr_if) => {
                    collect_params_from_stmts(&expr_if.then_branch.stmts, params);
                    if let Some((_else, else_expr)) = &expr_if.else_branch {
                        if let Expr::Block(block) = &**else_expr {
                            collect_params_from_stmts(&block.block.stmts, params);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

pub(super) fn infer_dispatch_tags_from_items(
    items: &[Item],
    handlers: &mut BTreeMap<String, PinocchioHandlerProfile>,
) {
    for item in items {
        match item {
            Item::Fn(item_fn) if item_fn.sig.ident == "process_instruction" => {
                walk_exprs_in_stmts(&item_fn.block.stmts, &mut |expr| {
                    let Expr::Match(expr_match) = expr else {
                        return;
                    };
                    for arm in &expr_match.arms {
                        let Some(tag) = pat_u8_literal(&arm.pat) else {
                            continue;
                        };
                        let Some(name) = first_process_callee(&arm.body) else {
                            continue;
                        };
                        let entry = handlers
                            .entry(name.clone())
                            .or_insert_with(|| empty_handler_profile(name));
                        entry.instruction_tag = Some(tag);
                    }
                });
            }
            Item::Mod(item_mod) => {
                if let Some((_brace, items)) = &item_mod.content {
                    infer_dispatch_tags_from_items(items, handlers);
                }
            }
            _ => {}
        }
    }
}

pub(super) fn infer_pda_derivations_from_fns(
    item_fns: &[&ItemFn],
    derivations: &mut BTreeMap<String, PinocchioPdaDerivation>,
) {
    for item_fn in item_fns {
        let fn_name = item_fn.sig.ident.to_string();
        let Some(name) = fn_name.strip_prefix("derive_").map(normalize_schema_name) else {
            continue;
        };
        let Some((seeds, program_id)) = first_find_program_address_call_from_block(&item_fn.block)
        else {
            continue;
        };
        let params = parse_syn_fn_params(&item_fn.sig);
        let param_names = params.iter().map(|(name, _ty)| name.clone()).collect();
        let param_types = params.into_iter().collect();
        derivations.insert(
            name.clone(),
            PinocchioPdaDerivation {
                name,
                params: param_names,
                param_types,
                local_key_derivations: infer_local_key_derivations_from_block(&item_fn.block),
                seeds: seeds
                    .into_iter()
                    .map(|expr| PinocchioPdaSeed {
                        expr,
                        literal: None,
                    })
                    .collect(),
                program_id,
                returns_tuple: pda_derivation_returns_tuple(item_fn),
            },
        );
    }
}

fn pda_derivation_returns_tuple(item_fn: &ItemFn) -> bool {
    matches!(
        &item_fn.sig.output,
        syn::ReturnType::Type(_, ty) if matches!(ty.as_ref(), syn::Type::Tuple(_))
    )
}
