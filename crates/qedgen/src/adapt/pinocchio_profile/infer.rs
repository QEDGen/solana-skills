//! AST- and source-text inference over handler bodies: recovers accounts,
//! roles, params, PDA derivations, dispatch tags, and verified stubs.

use super::*;

pub(super) fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if !dir.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if matches!(name, "target" | ".git" | "node_modules") {
            continue;
        }
        if path.is_dir() {
            collect_rust_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") && name != "kani_impl.rs"
        {
            out.push(path);
        }
    }
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

pub(super) fn infer_verified_stubs_from_body(
    body: &str,
    contracted_fns: &BTreeMap<String, String>,
    call_graph: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    let calls: Vec<_> = contracted_fns
        .iter()
        .filter(|(name, _path)| !call_arguments(body, name).is_empty())
        .map(|(name, _path)| name.clone())
        .collect();
    verified_stubs_for_calls(calls, contracted_fns, call_graph)
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

pub(super) fn process_fn_bodies(source: &str) -> Vec<(String, String)> {
    let re = Regex::new(r"(?:pub\s+)?fn\s+process_([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap();
    let mut out = Vec::new();
    for cap in re.captures_iter(source) {
        let Some(mat) = cap.get(0) else {
            continue;
        };
        let Some(open_rel) = source[mat.end()..].find('{') else {
            continue;
        };
        let open = mat.end() + open_rel;
        let Some(close) = matching_brace(source, open) else {
            continue;
        };
        out.push((cap[1].to_string(), source[open + 1..close].to_string()));
    }
    out
}

fn matching_brace(source: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, ch) in source[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

pub(super) fn infer_accounts(body: &str) -> Vec<String> {
    let re = Regex::new(r"(?s)let\s*\[([^\]]+),\s*\.\.\]\s*=\s*accounts").unwrap();
    if let Some(cap) = re.captures(body) {
        let accounts: Vec<_> = cap[1]
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        if !accounts.is_empty() {
            return accounts;
        }
    }

    let re = Regex::new(r"let\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*next_account_info\(").unwrap();
    re.captures_iter(body)
        .map(|cap| cap[1].to_string())
        .collect()
}

pub(super) fn infer_account_roles(
    body: &str,
    accounts: &[String],
) -> BTreeMap<String, PinocchioAccountRole> {
    let compact: String = body.chars().filter(|ch| !ch.is_whitespace()).collect();
    let mut roles = BTreeMap::new();
    for account in accounts {
        let ident = normalize_schema_name(account);
        let mut role = PinocchioAccountRole::default();
        let dotted = format!("{ident}.");
        if compact.contains(&format!("{dotted}is_signer()"))
            || compact.contains(&format!("{dotted}is_signer"))
        {
            role.is_signer = Some(true);
        }
        if compact.contains(&format!("{dotted}is_writable()"))
            || compact.contains(&format!("{dotted}is_writable"))
        {
            role.is_writable = Some(true);
        }
        if compact.contains(&format!("{dotted}is_executable()"))
            || compact.contains(&format!("{dotted}executable()"))
            || compact.contains(&format!("{dotted}executable"))
        {
            role.is_program = Some(true);
        }
        if compact.contains(&format!("require_key({ident},&SPL_TOKEN_ID)"))
            || compact.contains(&format!(
                "require_key({ident},&pinocchio_tkn::TOKEN_PROGRAM_ID)"
            ))
            || compact.contains(&format!("{dotted}key()!=&pinocchio_tkn::TOKEN_PROGRAM_ID"))
            || compact.contains(&format!("{dotted}key()!=&SPL_TOKEN_ID"))
        {
            role.is_program = Some(true);
            role.account_type = Some("token".to_string());
        }
        if compact.contains(&format!("read_mint_decimals({ident})"))
            || compact.contains(&format!("Mint::from_account_info({ident})"))
        {
            role.account_type = Some("mint".to_string());
        }
        if compact.contains(&format!("require_token_account({ident},"))
            || compact.contains(&format!("read_token_amount({ident})"))
            || compact.contains(&format!("write_token_amount({ident},"))
            || compact.contains(&format!("TokenAccount::from_account_info({ident})"))
        {
            role.account_type = Some("token".to_string());
        }
        if !role.is_empty() {
            roles.insert(ident, role);
        }
    }
    roles
}

pub(super) fn infer_key_account_aliases(body: &str) -> BTreeMap<String, String> {
    let mut aliases = BTreeMap::new();
    let re = Regex::new(
        r"require_key\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*&\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)",
    )
    .unwrap();
    for cap in re.captures_iter(body) {
        aliases.insert(
            normalize_schema_name(&cap[2]),
            normalize_schema_name(&cap[1]),
        );
    }
    aliases
}

pub(super) fn infer_local_key_derivations(
    body: &str,
) -> BTreeMap<String, PinocchioLocalKeyDerivation> {
    let mut derivations = BTreeMap::new();
    let re = Regex::new(
        r"(?s)let\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*derive_([A-Za-z_][A-Za-z0-9_]*)\s*\((.*?)\)\s*(?:\.0)?\s*;",
    )
    .unwrap();
    for cap in re.captures_iter(body) {
        derivations.insert(
            normalize_schema_name(&cap[1]),
            PinocchioLocalKeyDerivation {
                derivation: normalize_schema_name(&cap[2]),
                args: split_top_level_commas(&cap[3])
                    .into_iter()
                    .map(|arg| arg.trim().to_string())
                    .filter(|arg| !arg.is_empty())
                    .collect(),
            },
        );
    }
    derivations
}

pub(super) fn infer_source_expr_aliases(body: &str) -> BTreeMap<String, String> {
    let mut aliases = BTreeMap::new();

    for (name, value) in infer_wrapper_ctor_aliases(body) {
        aliases.insert(format!("{name}.0"), value);
    }

    let wrapper_re = Regex::new(
        r"(?s)let\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*[A-Za-z_][A-Za-z0-9_]*\s*\(\s*(.*?)\s*\)\s*;",
    )
    .unwrap();
    for cap in wrapper_re.captures_iter(body) {
        let name = normalize_schema_name(&cap[1]);
        let value = normalize_source_expr_alias(&cap[2]);
        if !value.is_empty() {
            aliases.insert(format!("{name}.0"), value);
        }
    }

    let struct_re = Regex::new(
        r"(?s)let\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*[A-Za-z_][A-Za-z0-9_]*\s*\{(.*?)\}\s*;",
    )
    .unwrap();
    for cap in struct_re.captures_iter(body) {
        let name = normalize_schema_name(&cap[1]);
        for field in split_top_level_commas(&cap[2]) {
            let field = field.trim();
            if field.is_empty() {
                continue;
            }
            let (field_name, value) = field
                .split_once(':')
                .map(|(field_name, value)| (field_name.trim(), value.trim()))
                .unwrap_or((field, field));
            let field_name = normalize_schema_name(field_name);
            let value = normalize_source_expr_alias(value);
            if value.is_empty() {
                continue;
            }
            aliases.insert(format!("{name}.{field_name}"), value.clone());
            aliases.insert(format!("{name}.{field_name}.0"), value);
        }
    }

    aliases
}

fn infer_wrapper_ctor_aliases(body: &str) -> Vec<(String, String)> {
    let mut aliases = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = body[pos..].find("let ") {
        let let_start = pos + rel;
        let after_let = let_start + "let ".len();
        let Some(eq_rel) = body[after_let..].find('=') else {
            break;
        };
        let eq = after_let + eq_rel;
        let name = body[after_let..eq].trim();
        if !is_ident(name) {
            pos = after_let;
            continue;
        }
        let rhs_start = eq + 1 + body[eq + 1..].len() - body[eq + 1..].trim_start().len();
        let ctor_len = body[rhs_start..]
            .chars()
            .take_while(|ch| *ch == '_' || ch.is_ascii_alphanumeric())
            .map(char::len_utf8)
            .sum::<usize>();
        if ctor_len == 0 {
            pos = rhs_start;
            continue;
        }
        let open = rhs_start + ctor_len;
        if body.as_bytes().get(open) != Some(&b'(') {
            pos = rhs_start + ctor_len;
            continue;
        }
        let Some(close) = matching_bracket(body, open, '(', ')') else {
            pos = open + 1;
            continue;
        };
        if !body[close + 1..].trim_start().starts_with(';') {
            pos = close + 1;
            continue;
        }
        let value = normalize_source_expr_alias(&body[open + 1..close]);
        if !value.is_empty() {
            aliases.push((normalize_schema_name(name), value));
        }
        pos = close + 1;
    }
    aliases
}

fn normalize_source_expr_alias(expr: &str) -> String {
    let expr = expr
        .trim()
        .trim_start_matches('&')
        .trim()
        .trim_start_matches('*')
        .trim();
    if let Some(open) = expr.find('(') {
        if expr.ends_with(')') && is_ident(&expr[..open]) {
            return normalize_source_expr_alias(&expr[open + 1..expr.len() - 1]);
        }
    }
    expr.to_string()
}

pub(super) fn infer_account_key_derivations(
    body: &str,
    local_key_derivations: &BTreeMap<String, PinocchioLocalKeyDerivation>,
) -> BTreeMap<String, PinocchioLocalKeyDerivation> {
    let mut derivations = BTreeMap::new();
    for args in call_arguments(body, "require_key") {
        if args.len() != 2 {
            continue;
        }
        let account = args[0].trim();
        if !is_ident(account) {
            continue;
        }
        if let Some((derivation, call_args)) = strip_ref_derive_call(&args[1]) {
            derivations.insert(
                normalize_schema_name(account),
                PinocchioLocalKeyDerivation {
                    derivation: normalize_schema_name(derivation),
                    args: call_args,
                },
            );
            continue;
        }
        if let Some(key_name) = strip_ref_ident(&args[1]) {
            if let Some(local) = local_key_derivations.get(key_name) {
                derivations.insert(normalize_schema_name(account), local.clone());
            }
        }
    }
    derivations
}

pub(super) fn infer_token_account_bindings(
    body: &str,
    key_account_aliases: &BTreeMap<String, String>,
    local_key_derivations: &BTreeMap<String, PinocchioLocalKeyDerivation>,
) -> BTreeMap<String, PinocchioTokenAccountBinding> {
    let mut bindings = BTreeMap::new();
    for args in call_arguments(body, "require_token_account") {
        if args.len() != 3 {
            continue;
        }
        let Some(mint_account) = strip_key_call(&args[1]) else {
            continue;
        };
        let owner_account = strip_key_call(&args[2]).map(str::to_string).or_else(|| {
            strip_ref_ident(&args[2]).and_then(|var| key_account_aliases.get(var).cloned())
        });
        let owner_key_derivation =
            strip_ref_ident(&args[2]).and_then(|var| local_key_derivations.get(var).cloned());
        bindings.insert(
            normalize_schema_name(&args[0]),
            PinocchioTokenAccountBinding {
                mint_account: Some(normalize_schema_name(mint_account)),
                owner_account,
                owner_key_derivation,
            },
        );
    }

    let re = Regex::new(
        r"require_token_account\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*([A-Za-z_][A-Za-z0-9_]*)\.key\(\)\s*,\s*([A-Za-z_][A-Za-z0-9_]*)\.key\(\)\s*\)",
    )
    .unwrap();
    for cap in re.captures_iter(body) {
        bindings.insert(
            normalize_schema_name(&cap[1]),
            PinocchioTokenAccountBinding {
                mint_account: Some(normalize_schema_name(&cap[2])),
                owner_account: Some(normalize_schema_name(&cap[3])),
                owner_key_derivation: None,
            },
        );
    }

    let re = Regex::new(
        r"require_token_account\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*([A-Za-z_][A-Za-z0-9_]*)\.key\(\)\s*,\s*&\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)",
    )
    .unwrap();
    for cap in re.captures_iter(body) {
        let owner_account = key_account_aliases
            .get(&normalize_schema_name(&cap[3]))
            .cloned();
        let owner_key_derivation = local_key_derivations
            .get(&normalize_schema_name(&cap[3]))
            .cloned();
        bindings.insert(
            normalize_schema_name(&cap[1]),
            PinocchioTokenAccountBinding {
                mint_account: Some(normalize_schema_name(&cap[2])),
                owner_account,
                owner_key_derivation,
            },
        );
    }

    let re = Regex::new(
        r"require_(?:matching_token_mint|token_mint)\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*([A-Za-z_][A-Za-z0-9_]*)\.key\(\)\s*\)",
    )
    .unwrap();
    for cap in re.captures_iter(body) {
        bindings
            .entry(normalize_schema_name(&cap[1]))
            .or_insert_with(|| PinocchioTokenAccountBinding {
                mint_account: None,
                owner_account: None,
                owner_key_derivation: None,
            })
            .mint_account = Some(normalize_schema_name(&cap[2]));
    }

    bindings
}

pub(super) fn infer_mint_decimal_bindings(body: &str) -> BTreeMap<String, String> {
    let mut bindings = BTreeMap::new();
    let re = Regex::new(
        r"let\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*read_mint_decimals\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)\s*\?",
    )
    .unwrap();
    for cap in re.captures_iter(body) {
        bindings.insert(
            normalize_schema_name(&cap[2]),
            normalize_schema_name(&cap[1]),
        );
    }
    bindings
}

fn call_arguments(body: &str, fn_name: &str) -> Vec<Vec<String>> {
    let mut calls = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = body[cursor..].find(fn_name) {
        let name_start = cursor + offset;
        let mut open = name_start + fn_name.len();
        while body
            .as_bytes()
            .get(open)
            .is_some_and(u8::is_ascii_whitespace)
        {
            open += 1;
        }
        if body.as_bytes().get(open) != Some(&b'(') {
            cursor = open;
            continue;
        }
        let mut depth = 0usize;
        let mut arg_start = open + 1;
        let mut args = Vec::new();
        let mut close = None;
        for (idx, byte) in body[open..].bytes().enumerate() {
            let pos = open + idx;
            match byte {
                b'(' => depth += 1,
                b')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        let arg = body[arg_start..pos].trim();
                        if !arg.is_empty() {
                            args.push(arg.to_string());
                        }
                        close = Some(pos + 1);
                        break;
                    }
                }
                b',' if depth == 1 => {
                    args.push(body[arg_start..pos].trim().to_string());
                    arg_start = pos + 1;
                }
                _ => {}
            }
        }
        if let Some(close) = close {
            calls.push(args);
            cursor = close;
        } else {
            break;
        }
    }
    calls
}

fn strip_key_call(expr: &str) -> Option<&str> {
    expr.trim().strip_suffix(".key()").and_then(|name| {
        let name = name.trim();
        if is_ident(name) {
            Some(name)
        } else {
            None
        }
    })
}

fn strip_ref_ident(expr: &str) -> Option<&str> {
    let ident = expr.trim().strip_prefix('&')?.trim();
    if is_ident(ident) {
        Some(ident)
    } else {
        None
    }
}

fn strip_ref_derive_call(expr: &str) -> Option<(&str, Vec<String>)> {
    let expr = expr.trim().strip_prefix('&')?.trim();
    let after_prefix = expr.strip_prefix("derive_")?;
    let open = after_prefix.find('(')?;
    let derivation = &after_prefix[..open];
    if !is_ident(derivation) {
        return None;
    }
    let arg_start = "derive_".len() + open;
    let close = matching_bracket(expr, arg_start, '(', ')')?;
    let tail = expr[close + 1..].trim();
    if !tail.is_empty() && tail != ".0" {
        return None;
    }
    let args = split_top_level_commas(&expr[arg_start + 1..close])
        .into_iter()
        .map(str::trim)
        .filter(|arg| !arg.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    Some((derivation, args))
}

fn is_ident(input: &str) -> bool {
    let mut chars = input.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

pub(super) fn infer_params(body: &str) -> Vec<PinocchioParamField> {
    let re = Regex::new(
        r"(?s)let\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([ui](?:8|16|32|64|128))::from_le_bytes\(\s*instruction_data\s*\.get\((\d+)\.\.(\d+)\)",
    )
    .unwrap();
    let mut params = Vec::new();
    for cap in re.captures_iter(body) {
        let Ok(start) = cap[3].parse::<usize>() else {
            continue;
        };
        let Ok(end) = cap[4].parse::<usize>() else {
            continue;
        };
        params.push(PinocchioParamField {
            name: cap[1].to_string(),
            rust_type: cap[2].to_string(),
            start,
            end,
        });
    }
    params.sort_by_key(|p| p.start);
    params
}

pub(super) fn infer_dispatch_tags(
    source: &str,
    handlers: &mut BTreeMap<String, PinocchioHandlerProfile>,
) {
    let re = Regex::new(
        r"(?m)(\d+)\s*=>\s*instructions::([A-Za-z_][A-Za-z0-9_]*)::process_([A-Za-z_][A-Za-z0-9_]*)\(",
    )
    .unwrap();
    for cap in re.captures_iter(source) {
        let Ok(tag) = cap[1].parse::<u8>() else {
            continue;
        };
        let name = cap[3].to_string();
        let entry = handlers
            .entry(name.clone())
            .or_insert_with(|| PinocchioHandlerProfile {
                name,
                instruction_tag: None,
                accounts: Vec::new(),
                account_roles: BTreeMap::new(),
                token_account_bindings: BTreeMap::new(),
                mint_decimal_bindings: BTreeMap::new(),
                account_key_derivations: BTreeMap::new(),
                source_expr_aliases: BTreeMap::new(),
                verified_stubs: Vec::new(),
                params: Vec::new(),
                repeats: Vec::new(),
            });
        entry.instruction_tag = Some(tag);
    }
}

pub(super) fn infer_pda_derivations(
    source: &str,
    derivations: &mut BTreeMap<String, PinocchioPdaDerivation>,
) {
    for (name, params, returns_tuple, body) in derive_fn_bodies(source) {
        let Some((seeds, program_id)) = first_find_program_address_call(&body) else {
            continue;
        };
        let param_names = params
            .iter()
            .map(|(name, _ty)| name.clone())
            .collect::<Vec<_>>();
        let param_types = params.into_iter().collect::<BTreeMap<_, _>>();
        let local_key_derivations = infer_local_key_derivations(&body);
        derivations.insert(
            name.clone(),
            PinocchioPdaDerivation {
                name,
                params: param_names,
                param_types,
                local_key_derivations,
                seeds: seeds
                    .into_iter()
                    .map(|expr| PinocchioPdaSeed {
                        expr,
                        literal: None,
                    })
                    .collect(),
                program_id,
                returns_tuple,
            },
        );
    }
}

type DeriveFnBody = (String, Vec<(String, String)>, bool, String);

fn derive_fn_bodies(source: &str) -> Vec<DeriveFnBody> {
    let re = Regex::new(r"pub\s+fn\s+derive_([A-Za-z_][A-Za-z0-9_]*)\s*\(([^)]*)\)\s*(->[^{]+)?")
        .unwrap();
    let mut out = Vec::new();
    for cap in re.captures_iter(source) {
        let Some(mat) = cap.get(0) else {
            continue;
        };
        let Some(open_rel) = source[mat.end()..].find('{') else {
            continue;
        };
        let open = mat.end() + open_rel;
        let Some(close) = matching_brace(source, open) else {
            continue;
        };
        out.push((
            cap[1].to_string(),
            parse_fn_params(&cap[2]),
            cap.get(3).is_some_and(|ret| ret.as_str().contains('(')),
            source[open + 1..close].to_string(),
        ));
    }
    out
}

fn parse_fn_params(params: &str) -> Vec<(String, String)> {
    split_top_level_commas(params)
        .into_iter()
        .filter_map(|param| param.split_once(':'))
        .map(|(name, ty)| (normalize_schema_name(name.trim()), ty.trim().to_string()))
        .filter(|(name, _ty)| !name.is_empty())
        .collect()
}

fn first_find_program_address_call(body: &str) -> Option<(Vec<String>, String)> {
    for needle in ["find_program_address", "try_find_program_address"] {
        let Some(call_start) = body.find(needle) else {
            continue;
        };
        let after_name = &body[call_start + needle.len()..];
        let Some(seed_rel) = after_name.find("&[") else {
            continue;
        };
        let seed_list_start = call_start + needle.len() + seed_rel + 1;
        let Some(seed_list_end) = matching_bracket(body, seed_list_start, '[', ']') else {
            continue;
        };
        let after_seed_list = &body[seed_list_end + 1..];
        let Some(comma) = after_seed_list.find(',') else {
            continue;
        };
        let after_comma = after_seed_list[comma + 1..].trim_start();
        let program_id = after_comma
            .split([')', ';', '\n'])
            .next()
            .unwrap_or("")
            .trim()
            .trim_end_matches(',')
            .trim()
            .trim_start_matches('&')
            .trim()
            .to_string();
        let seeds = split_top_level_commas(&body[seed_list_start + 1..seed_list_end])
            .into_iter()
            .map(normalize_seed_expr)
            .filter(|seed| !seed.is_empty())
            .collect::<Vec<_>>();
        if !seeds.is_empty() && !program_id.is_empty() {
            return Some((seeds, program_id));
        }
    }
    None
}

fn matching_bracket(source: &str, open: usize, left: char, right: char) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, ch) in source[open..].char_indices() {
        if ch == left {
            depth += 1;
        } else if ch == right {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(open + offset);
            }
        }
    }
    None
}

fn split_top_level_commas(source: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    for (index, ch) in source.char_indices() {
        match ch {
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.saturating_sub(1),
            '[' => bracket_depth += 1,
            ']' => bracket_depth = bracket_depth.saturating_sub(1),
            ',' if paren_depth == 0 && bracket_depth == 0 => {
                parts.push(source[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(source[start..].trim());
    parts
}

fn normalize_seed_expr(expr: &str) -> String {
    expr.trim()
        .trim_start_matches('&')
        .trim()
        .trim_start_matches("crate::")
        .trim()
        .to_string()
}
