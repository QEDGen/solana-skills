use super::*;

/// Emit the Context/instruction harness (#169/G5): drive the REAL Anchor
/// `#[derive(Accounts)]` constraint gate — `<Ctx>::try_accounts` — with
/// symbolic `AccountInfo`s, then (agent-fill) the real instruction fn through a
/// `Context`, and assert the instruction-level authorization property. This is
/// the shape neither existing mode reaches: the state-struct harness (#162)
/// sidesteps accounts entirely, and the greenfield Context shape assumes a
/// struct layout real Anchor programs don't have.
///
/// Mechanics (de-risked under real `cargo kani`, see #169):
///   - `AccountInfo`s are built over `Box::leak`ed backing ('static sidesteps
///     the lifetime plumbing; a leak is free under BMC).
///   - The Borsh wall (T3, #182) is bypassed by stubbing the account type's
///     `try_deserialize` with the spec-generated symbolic ctor
///     (`state_ctor`) — data buffers stay tiny and unread.
///   - Composes with the #182 stubs: T1 Pubkey `==` (has_one/key compares
///     close at a small unwind), T2 PDA, Clock, log/CPI no-ops.
///
/// Generated: account-info construction (signer flags symbolic), the
/// `try_accounts` call, pre-snapshots, requires-assume, signer-gate asserts,
/// ensures-assert. Agent-fill: the instruction fn call through
/// `Context::new` (the fn path + arg shape are real-source knowledge).
pub(crate) fn emit_kani_impl_anchor_context(
    spec: &ParsedSpec,
    output_path: &Path,
    emit_targets: &[&ParsedHandler],
    explicit_flag: bool,
) -> Result<()> {
    let fp = crate::fingerprint::compute_fingerprint(spec);

    let mut out = String::new();
    out.push_str(&crate::codegen_shared::marker_unlabeled(
        &fp,
        "tests/kani_impl.rs",
    ));
    out.push_str("//\n");
    out.push_str("// Impl-targeted Kani harnesses — CONTEXT/instruction mode (#169). Drives\n");
    out.push_str("// the REAL `#[derive(Accounts)]` constraint gate (`try_accounts`) with\n");
    out.push_str("// symbolic `AccountInfo`s, then the real instruction fn through a\n");
    out.push_str("// `Context` (agent-fill), and asserts the instruction-level authorization\n");
    out.push_str("// property — signer / has_one / owner / seeds checks that the state-struct\n");
    out.push_str("// mode cannot reach.\n");
    out.push_str("//\n");
    let in_module = super::state_ctor::is_in_module(spec);
    if in_module {
        out.push_str("// PLACEMENT (pragma state_module): this harness names types with BARE\n");
        out.push_str("// identifiers via `use super::*` — place it INSIDE the module that\n");
        out.push_str("// DEFINES them (append e.g.\n");
        out.push_str("//   #[cfg(kani)] #[path = \"kani_impl.rs\"] mod kani_impl;\n");
        out.push_str("// to that module's `.rs`). See docs/toolchain-backlog.md G17.\n");
    } else {
        out.push_str("// PLACEMENT: this file must live INSIDE the program crate (e.g.\n");
        out.push_str("// `src/kani_impl.rs` + `#[cfg(kani)] mod kani_impl;` in lib.rs) — a\n");
        out.push_str("// standalone harness crate hits cargo dependency-hell. See\n");
        out.push_str("// docs/toolchain-backlog.md G3.\n");
    }
    if !explicit_flag {
        out.push_str("//\n");
        out.push_str("// Auto-triggered (a handler declares `modifies` fields absent from its\n");
        out.push_str("// `effect` block). Pass `--kani-impl-context` to force emission.\n");
    }
    out.push_str("//\n");
    out.push_str("// To run:  cargo kani -Z stubbing --harness <name>   (requires cargo-kani)\n");
    out.push_str("// ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ----\n");
    out.push_str("#![cfg(kani)]\n");
    if in_module {
        out.push_str("use super::*;\n");
    } else {
        out.push_str("#[allow(unused_imports)]\nuse crate::*;\n");
    }
    let extra_uses = spec.pragma_values("harness_use");
    if !extra_uses.is_empty() {
        out.push_str("#[allow(unused_imports)]\n");
        for path in extra_uses {
            out.push_str(&format!("use {};\n", path));
        }
    }
    out.push('\n');

    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Instruction-level authorization proofs (Context mode, #169)\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    // Symbolic account-info plumbing — emitted once per file.
    out.push_str(&account_info_support_fns());

    // Symbolic-state ctor + the per-type deserialize stub that injects it.
    // When the State isn't fully constructible the stub falls back to an
    // agent-fill `todo!()` BODY (fail-loud under Kani, never vacuous).
    let ctor_ctx = super::state_ctor::CtorCtx::from_spec(spec);
    let type_path = super::state_ctor::type_path_of(spec);
    let state_struct: Option<String> = match super::state_ctor::resolve_state_struct(spec) {
        Some((name, fields)) => {
            match super::state_ctor::emit_state_ctor(name, fields, &ctor_ctx) {
                Some(ctor) => {
                    out.push_str(&ctor);
                    out.push('\n');
                    out.push_str(&deserialize_stub_fn(
                        name, &type_path, /*generated=*/ true,
                    ));
                    out.push('\n');
                    Some(name.to_string())
                }
                None => {
                    out.push_str(&deserialize_stub_fn(
                        name, &type_path, /*generated=*/ false,
                    ));
                    out.push('\n');
                    Some(name.to_string())
                }
            }
        }
        None => None,
    };

    // #182 stub support fns — same opt-ins as the state-struct mode.
    if super::state_ctor::wants_pubkey_abstraction(spec) {
        out.push_str(&super::state_ctor::pubkey_eq_abstract_fn());
        out.push('\n');
    }
    if super::state_ctor::wants_pda_abstraction(spec) {
        out.push_str(&super::state_ctor::pda_stub_fn());
        out.push('\n');
    }
    if super::state_ctor::wants_hash_stub(spec) {
        out.push_str(&super::state_ctor::hash_stub_fn());
        out.push('\n');
    }
    if super::state_ctor::wants_secp256k1_stub(spec) {
        out.push_str(&super::state_ctor::secp256k1_stub_fn());
        out.push('\n');
    }
    if super::state_ctor::wants_clock_stub(spec) {
        out.push_str(&super::state_ctor::clock_stub_fn());
        out.push('\n');
    }
    if super::state_ctor::wants_log_stub(spec) {
        out.push_str(&super::state_ctor::log_stub_fn());
        out.push('\n');
    }
    if super::state_ctor::wants_cpi_stub(spec) {
        out.push_str(&super::state_ctor::cpi_stub_fn());
        out.push('\n');
    }
    if super::state_ctor::wants_div_abstraction(spec) {
        out.push_str(&super::state_ctor::div_abstract_fn());
        out.push('\n');
    }

    let mut emitted_count = 0;
    for handler in emit_targets {
        for (idx, ensures) in handler.ensures.iter().enumerate() {
            emit_context_handler_harness(
                &mut out,
                handler,
                idx,
                ensures,
                spec,
                state_struct.as_deref(),
                &type_path,
            )?;
            emitted_count += 1;
        }
    }

    out.push_str("// ---- GENERATED BY QEDGEN — DO NOT EDIT BELOW THIS LINE ----\n");

    crate::codegen_shared::write_generated_file(output_path, &out)?;

    eprintln!(
        "Generated {} Context-mode impl-targeted Kani harness(es) in {}",
        emitted_count,
        output_path.display()
    );

    Ok(())
}

/// The leaked-backing `AccountInfo` builder + symbolic key helper, emitted once
/// per file. 'static backing (via `Box::leak`) dissolves the `AccountInfo`
/// lifetime plumbing — the historical wall for driving real account-reading
/// code under Kani — and a leak is free under BMC (the proof ends at fn exit).
fn account_info_support_fns() -> String {
    "// Symbolic account-info plumbing (#169): 'static leaked backing sidesteps\n\
     // the AccountInfo lifetime web; leaking is free under BMC.\n\
     fn symbolic_pubkey() -> anchor_lang::prelude::Pubkey {\n\
     \x20   anchor_lang::prelude::Pubkey::new_from_array(kani::any())\n\
     }\n\
     #[allow(clippy::too_many_arguments)]\n\
     fn leak_account_info(\n\
     \x20   key: anchor_lang::prelude::Pubkey,\n\
     \x20   is_signer: bool,\n\
     \x20   is_writable: bool,\n\
     \x20   executable: bool,\n\
     \x20   owner: &'static anchor_lang::prelude::Pubkey,\n\
     \x20   data: &'static mut [u8],\n\
     ) -> anchor_lang::prelude::AccountInfo<'static> {\n\
     \x20   let key: &'static anchor_lang::prelude::Pubkey = Box::leak(Box::new(key));\n\
     \x20   let lamports: &'static mut u64 = Box::leak(Box::new(1_000_000u64));\n\
     \x20   anchor_lang::prelude::AccountInfo::new(\n\
     \x20       key, is_signer, is_writable, lamports, data, owner, executable, 0,\n\
     \x20   )\n\
     }\n\n"
        .to_string()
}

/// The per-account-type `try_deserialize` stub (T3 escape hatch): Anchor's
/// `Account::try_from` calls `<T>::try_deserialize` on the account data —
/// stubbing it with the symbolic ctor bypasses the Borsh-on-symbolic-bytes
/// wall (#182 T3) entirely; the data buffer is never read. When the ctor
/// couldn't be generated the body is a `todo!()` — fail-loud under Kani
/// (a reachable panic is a FAILURE, never a vacuous pass).
fn deserialize_stub_fn(struct_name: &str, type_path: &str, generated: bool) -> String {
    let snake = to_snake_case(struct_name);
    let body = if generated {
        format!("    Ok(symbolic_{snake}())")
    } else {
        format!(
            "    // AGENT-FILL: return a symbolic `{type_path}{struct_name}` (the spec's State\n\
             \x20   // isn't fully constructible — an imported/unresolved or `Map` field).\n\
             \x20   todo!(\"symbolic {struct_name}\")"
        )
    };
    format!(
        "// T3 escape hatch (#169): `Account::try_from` calls `{struct_name}::try_deserialize`\n\
         // on the account data; stub it with the symbolic ctor so the Borsh wall\n\
         // (#182 T3) is never hit and the data buffer is never read.\n\
         fn stub_try_deserialize_{snake}(\n\
         \x20   _buf: &mut &[u8],\n\
         ) -> anchor_lang::Result<{type_path}{struct_name}> {{\n\
         {body}\n\
         }}\n"
    )
}

/// The stub attr redirecting `<T>::try_deserialize` (per proof).
fn deserialize_stub_attr(struct_name: &str, type_path: &str) -> String {
    let snake = to_snake_case(struct_name);
    format!(
        "#[kani::stub({type_path}{struct_name}::try_deserialize, stub_try_deserialize_{snake})]\n"
    )
}

/// Resolve the real `#[derive(Accounts)]` struct name for a handler.
/// `pragma context_struct = <handler>::<Struct>` binds per handler;
/// `pragma context_struct = <Struct>` is a spec-wide default; absent both,
/// fall back to `PascalCase(handler)` — the dominant Anchor convention
/// (`fn execute_transaction(ctx: Context<ExecuteTransaction>, …)`).
fn context_struct_name(spec: &ParsedSpec, handler: &ParsedHandler) -> String {
    let values = spec.pragma_values("context_struct");
    for v in &values {
        if let Some((h, s)) = v.split_once("::") {
            if h == handler.name {
                return s.to_string();
            }
        }
    }
    for v in &values {
        if !v.contains("::") {
            return v.to_string();
        }
    }
    to_pascal_case(&handler.name)
}

/// `true` when this account carries program state the harness deserializes —
/// i.e. it is the account whose data the spec's State mirrors.
fn is_state_account(acct: &ParsedHandlerAccount, state_acct: Option<&str>) -> bool {
    state_acct == Some(acct.name.as_str())
}

/// Emit one Context-mode `#[kani::proof]` for a (handler, ensures) pair.
fn emit_context_handler_harness(
    out: &mut String,
    handler: &ParsedHandler,
    idx: usize,
    ensures: &crate::check::ParsedEnsures,
    spec: &ParsedSpec,
    state_struct: Option<&str>,
    type_path: &str,
) -> Result<()> {
    let ctx_struct = context_struct_name(spec, handler);
    let state_acct = find_state_account_name(handler);

    emit_impl_proof_attrs(out, handler, spec);
    if let Some(s) = state_struct {
        out.push_str(&deserialize_stub_attr(s, type_path));
    }
    out.push_str(&format!(
        "fn verify_{}_ctx_ensures_{}() {{\n",
        handler.name, idx
    ));

    // ── 1. Symbolic instruction accounts. ORDER = the spec's `accounts` block;
    // `try_accounts` consumes the slice in the REAL struct's field order — the
    // agent must confirm they agree (a mismatch fails loudly: wrong owner /
    // missing signer, never a silent wrong proof).
    out.push_str(&format!(
        "    // Symbolic accounts for `{ctx_struct}` — order must match the real\n\
         \x20   // `#[derive(Accounts)]` field order (try_accounts consumes in order).\n"
    ));
    let mut info_names: Vec<String> = Vec::with_capacity(handler.accounts.len());
    let mut signer_flags: Vec<String> = Vec::new();
    if handler.accounts.is_empty() {
        out.push_str(
            "    // No accounts declared in the spec for this handler — AGENT-FILL:\n\
             \x20   // build one `leak_account_info(...)` per real struct field.\n\
             \x20   let infos: &'static [anchor_lang::prelude::AccountInfo<'static>] =\n\
             \x20       todo!(\"build the instruction's AccountInfos\");\n",
        );
    } else {
        for acct in &handler.accounts {
            emit_account_info(out, acct, state_acct, &mut signer_flags, spec);
            info_names.push(format!("{}_info", acct.name));
        }
        out.push_str(&format!(
            "    let infos: &'static [anchor_lang::prelude::AccountInfo<'static>] =\n\
             \x20       Box::leak(Box::new([{}]));\n",
            info_names.join(", ")
        ));
    }

    // ── 2. The real constraint gate.
    out.push_str(&format!(
        "    let mut acct_slice: &[anchor_lang::prelude::AccountInfo] = infos;\n\
         \x20   let mut bumps = {type_path}{ctx_struct}Bumps::default();\n\
         \x20   let mut reallocs = std::collections::BTreeSet::new();\n\
         \x20   // Anchor's generated constraint gate — signer / owner / has_one / seeds /\n\
         \x20   // mut checks all run HERE. ix_data is `&[]`: `#[instruction(..)]` args\n\
         \x20   // are not modeled (AGENT: thread real ix_data if the struct reads it).\n\
         \x20   let res = <{type_path}{ctx_struct} as anchor_lang::Accounts<_>>::try_accounts(\n\
         \x20       &crate::ID, &mut acct_slice, &[], &mut bumps, &mut reallocs,\n\
         \x20   );\n\
         \x20   let Ok(mut ctx_accounts) = res else {{ return }};\n\n"
    ));

    // ── 3. Pre-snapshot + symbolic params + requires-assume (same lowering as
    // the state-struct mode; state reads resolve through the deserialized
    // state account).
    let guard_predot = crate::rust_codegen_util::collect_full_guard(handler, false)
        .map(|g| rewrite_state_var_to_pre(&g));
    let snapshot = collect_snapshot_fields_split(handler, guard_predot.as_deref(), ensures);
    let state_path = state_acct.map(|a| format!("ctx_accounts.{a}"));
    if !snapshot.pre.is_empty() {
        out.push_str(
            "    // Pre-state snapshot — fields the requires/ensures read via `pre.<x>`.\n",
        );
        for field in &snapshot.pre {
            match &state_path {
                Some(path) => {
                    let rhs = if state_field_needs_clone(spec, field) {
                        format!("{path}.{field}.clone()")
                    } else {
                        format!("{path}.{field}")
                    };
                    out.push_str(&format!("    let pre_{field} = {rhs};\n"));
                }
                None => {
                    out.push_str(&format!(
                        "    let pre_{field} = todo!(\"snapshot pre.{field} from the state account\");\n"
                    ));
                }
            }
        }
    }
    for (pname, ptype) in &handler.takes_params {
        if ptype == "Pubkey" {
            out.push_str(&format!(
                "    let {pname}: anchor_lang::prelude::Pubkey = \
                 anchor_lang::prelude::Pubkey::new_from_array(kani::any());\n"
            ));
        } else {
            out.push_str(&format!(
                "    let {}: {} = kani::any();\n",
                pname,
                map_type(ptype, spec)?
            ));
        }
    }
    if let Some(guard) = &guard_predot {
        out.push_str(&format!(
            "    kani::assume({});\n",
            rewrite_pre_post_paths(guard)
        ));
    }

    // ── 4. Drive the real instruction fn — agent-fill (the fn path and arg
    // shape are real-source knowledge the spec doesn't carry).
    let params: Vec<&str> = handler
        .takes_params
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    out.push_str(&format!(
        "\n    // AGENT-FILL: drive the real instruction through a Context, e.g.\n\
         \x20   //   let ctx = anchor_lang::context::Context::new(\n\
         \x20   //       &crate::ID, &mut ctx_accounts, &[], bumps);\n\
         \x20   //   let ok = crate::<program_mod>::{}(ctx{}{}).is_ok();\n\
         \x20   let ok: bool = todo!(\"call the real instruction fn via Context::new\");\n",
        handler.name,
        if params.is_empty() { "" } else { ", " },
        params.join(", ")
    ));

    // ── 5. The authorization gate asserts. Signer-gate asserts are GENERATED:
    // the spec marks the account `signer`, so a successful instruction without
    // that signature is the crown-jewel violation. If the real struct binds the
    // account as `AccountInfo`/`UncheckedAccount` instead of `Signer`, this
    // assert FIRES — that is the finding.
    out.push_str("    if ok {\n");
    out.push_str(
        "        kani::cover!(true, \"instruction success path reachable (non-vacuity)\");\n",
    );
    for flag in &signer_flags {
        out.push_str(&format!(
            "        assert!({flag}, \"instruction succeeded without `{}`'s signature\");\n",
            flag.trim_end_matches("_signer")
        ));
    }
    let lowered = match &state_path {
        Some(path) => rewrite_ensures_post_to_path(&ensures.rust_expr_binary, path),
        None => rewrite_pre_post_paths(&ensures.rust_expr_binary),
    };
    out.push_str(&format!("        assert!(\n            {},\n", lowered));
    out.push_str(&format!(
        "            \"ensures clause {} on {} (impl, context) violated\"\n",
        idx, handler.name
    ));
    out.push_str("        );\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
    Ok(())
}

/// Emit the `let <name>_info = leak_account_info(...)` line(s) for one spec
/// account. Signer accounts get a SYMBOLIC `is_signer` bound to a local (the
/// gate assert reads it); program accounts get their well-known id +
/// `executable: true` (unknown program ids are a fail-loud `todo!()` — a
/// symbolic id would make `Program::try_accounts` always fail → vacuous);
/// the state account gets an 8-byte buffer (never read — deserialize is
/// stubbed); everything else gets empty data.
fn emit_account_info(
    out: &mut String,
    acct: &ParsedHandlerAccount,
    state_acct: Option<&str>,
    signer_flags: &mut Vec<String>,
    spec: &ParsedSpec,
) {
    let name = &acct.name;
    if acct.is_program {
        let key_expr = program_key_expr(acct, spec);
        out.push_str(&format!(
            "    let {name}_info = leak_account_info(\n\
             \x20       {key_expr},\n\
             \x20       false, false, /*executable=*/ true,\n\
             \x20       Box::leak(Box::new(anchor_lang::solana_program::bpf_loader::ID)),\n\
             \x20       Box::leak(Box::new([0u8; 0])),\n\
             \x20   );\n"
        ));
        return;
    }
    if acct.is_signer {
        out.push_str(&format!(
            "    let {name}_signer: bool = kani::any(); // the gate under proof\n\
             \x20   let {name}_info = leak_account_info(\n\
             \x20       symbolic_pubkey(), {name}_signer, {writable}, false,\n\
             \x20       Box::leak(Box::new(anchor_lang::solana_program::system_program::ID)),\n\
             \x20       Box::leak(Box::new([0u8; 0])),\n\
             \x20   );\n",
            writable = acct.is_writable,
        ));
        signer_flags.push(format!("{name}_signer"));
        return;
    }
    if is_state_account(acct, state_acct) {
        out.push_str(&format!(
            "    // `{name}`: the state account — program-owned; data never read\n\
             \x20   // (try_deserialize is stubbed to the symbolic ctor).\n\
             \x20   let {name}_info = leak_account_info(\n\
             \x20       symbolic_pubkey(), kani::any(), {writable}, false, &crate::ID,\n\
             \x20       Box::leak(Box::new([0u8; 8])),\n\
             \x20   );\n",
            writable = acct.is_writable,
        ));
        return;
    }
    // Other (non-state, non-signer, non-program) account — symbolic key,
    // program-owned by default; the agent adjusts owner/data for token/mint
    // accounts (slice 1 doesn't model SPL account layouts).
    out.push_str(&format!(
        "    // `{name}`: symbolic account (AGENT: adjust owner/data if this is a\n\
         \x20   // token/mint/foreign account — default assumes program-owned).\n\
         \x20   let {name}_info = leak_account_info(\n\
         \x20       symbolic_pubkey(), kani::any(), {writable}, false, &crate::ID,\n\
         \x20       Box::leak(Box::new([0u8; 8])),\n\
         \x20   );\n",
        writable = acct.is_writable,
    ));
}

/// The key expression for a program account. `system`/`token` map to their
/// well-known ids; anything else is a fail-loud `todo!()` (a symbolic id would
/// make `Program::try_accounts` unconditionally fail → every proof vacuous).
fn program_key_expr(acct: &ParsedHandlerAccount, _spec: &ParsedSpec) -> String {
    let n = acct.name.to_ascii_lowercase();
    let t = acct.account_type.as_deref().unwrap_or("");
    if t == "token" || n.contains("token_program") {
        "anchor_spl::token::ID".to_string()
    } else if n.contains("system_program") || n == "system" {
        "anchor_lang::solana_program::system_program::ID".to_string()
    } else {
        format!("todo!(\"real program id for `{}`\")", acct.name)
    }
}

/// Ensures lowering for the Context harness: `pre.X` stays its snapshot local;
/// `post.X` reads the (mutated-in-place) deserialized state account directly —
/// `ctx_accounts.<state_acct>.X`. Same rationale as the state-struct mode's
/// direct-read: no owned post-snapshot, no extra drop of a nested container.
fn rewrite_ensures_post_to_path(expr: &str, state_path: &str) -> String {
    expr.replace("pre.", "pre_")
        .replace("post.", &format!("{state_path}."))
        .replace("match (", "match &(")
        .replace(").clone() {", ") {")
}
