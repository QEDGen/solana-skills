// Agent-fill prompt emission for v2.4-M4.
//
// `qedgen codegen --fill` scans the generated handler files for `todo!()`
// markers and prints one structured prompt block per handler to stdout. The
// in-session agent (Claude / Codex / similar) reads the prompts and edits
// the corresponding files.
//
// We deliberately do NOT call any LLM API from here. Routing between local
// LLM, Leanstral, and Aristotle is agent-decided per SKILL.md, not
// hardcoded in the CLI (memory: feedback_llm_routing).

use crate::check::{ParsedHandler, ParsedSpec};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const SEP: &str = "═════════════════════════════════════════════════════════════════════";
const HALF: &str = "─────────────────────────────────────────────────────────────────────";

pub struct FillOpts<'a> {
    pub spec: &'a ParsedSpec,
    pub spec_path: &'a Path,
    pub programs_dir: &'a Path,
    pub only_handler: Option<&'a str>,
}

pub struct FillTestsOpts<'a> {
    pub spec: &'a ParsedSpec,
    pub spec_path: &'a Path,
    pub tests_path: &'a Path,
}

pub fn emit_test_prompts(opts: &FillTestsOpts<'_>) -> Result<usize> {
    if !opts.tests_path.exists() {
        eprintln!(
            "qedgen fill-tests — no integration test file at {} (run `qedgen codegen --integration` first).",
            opts.tests_path.display()
        );
        return Ok(0);
    }
    let body = std::fs::read_to_string(opts.tests_path)
        .with_context(|| format!("reading {}", opts.tests_path.display()))?;

    let mut prompts = Vec::new();
    let lines: Vec<&str> = body.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        if !line.contains("todo!(") {
            continue;
        }
        // Skip comment lines that just *mention* todo!() in prose.
        if line.trim_start().starts_with("//") {
            continue;
        }
        let test_name = enclosing_fn(&lines, idx).unwrap_or("<unknown>".to_string());
        prompts.push(build_test_prompt(
            opts.spec,
            opts.spec_path,
            opts.tests_path,
            idx + 1,
            &test_name,
            &lines,
        ));
    }

    if prompts.is_empty() {
        eprintln!(
            "qedgen fill-tests — nothing to fill ({} contains no `todo!(`).",
            opts.tests_path.display()
        );
        return Ok(0);
    }

    println!(
        "qedgen-fill-tests: {} prompt(s) — copy these to your agent or act on them in this session.\n",
        prompts.len()
    );
    for p in &prompts {
        println!("{}", p);
    }
    Ok(prompts.len())
}

fn enclosing_fn(lines: &[&str], idx: usize) -> Option<String> {
    for line in lines[..=idx].iter().rev() {
        if let Some(rest) = line.trim_start().strip_prefix("fn ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

fn build_test_prompt(
    spec: &ParsedSpec,
    spec_path: &Path,
    tests_path: &Path,
    line_no: usize,
    test_name: &str,
    lines: &[&str],
) -> String {
    let mut out = String::new();
    out.push_str(SEP);
    out.push('\n');
    out.push_str(&format!("QEDGEN-FILL-TESTS: {}:{}\n", tests_path.display(), line_no));
    out.push_str(&format!("test: {}    spec: {}\n", test_name, spec_path.display()));
    out.push_str(HALF);
    out.push_str("\n\n");

    out.push_str(&format!(
        "Replace the `todo!(...)` at line {} with the test body.\n\n",
        line_no
    ));

    // Surrounding context: print 8 lines before the todo to anchor the agent.
    let start = line_no.saturating_sub(8);
    out.push_str("Surrounding lines (read-only — do not modify the comments):\n");
    for (i, l) in lines.iter().enumerate().skip(start).take(line_no - start) {
        out.push_str(&format!("  {:>4} | {}\n", i + 1, l));
    }
    out.push('\n');

    // Lifecycle context — most useful for test_lifecycle_sequence.
    if test_name == "test_lifecycle_sequence" {
        out.push_str("Spec lifecycle handlers (in declaration order):\n");
        for h in &spec.handlers {
            let pre = h.pre_status.as_deref().unwrap_or("?");
            let post = h.post_status.as_deref().unwrap_or("?");
            out.push_str(&format!(
                "  {:<14} : {} -> {}\n",
                h.name, pre, post
            ));
        }
        out.push('\n');
    }

    // Handler-specific context if the test name matches a handler.
    let handler = spec.handlers.iter().find(|h| {
        test_name == format!("test_{}", h.name)
            || test_name == format!("test_{}_unauthorized", h.name)
    });
    if let Some(h) = handler {
        out.push_str(&format!("Handler `{}` accepts:\n", h.name));
        if h.takes_params.is_empty() {
            out.push_str("  (no parameters)\n");
        } else {
            for (n, t) in &h.takes_params {
                out.push_str(&format!("  {:<14} : {}\n", n, t));
            }
        }
        out.push_str(&format!("\nHandler `{}` accounts:\n", h.name));
        for acct in &h.accounts {
            out.push_str(&format!("  {:<14} : {}\n", acct.name, describe_account(acct)));
        }
        out.push('\n');
    }

    // Available helpers (always present in the generated file).
    out.push_str("Available helpers (defined at top of file):\n");
    out.push_str("  signer(addr)                                 -> Account\n");
    out.push_str("  empty(addr)                                  -> Account (system-owned, zero data)\n");
    out.push_str("  state_account(addr, ...spec_fields, bump)    -> Account (program-owned, populated)\n");
    out.push_str("  mint_account(addr, authority)                -> Account\n");
    out.push_str("  token_account(addr, mint, owner, amount)     -> Account\n");
    out.push_str("  Instruction builders: instructions::<handler>::Builder::default().<param>(v)…build()\n");
    out.push('\n');

    out.push_str("Constraints:\n");
    out.push_str("  - Use `let result = svm.execute(&instruction, &[<accounts>]);` to invoke.\n");
    out.push_str("  - For lifecycle sequences, chain handlers in the order shown above.\n");
    out.push_str("  - After each step, assert `result.is_ok()` and read the post-state via `svm.account(&pubkey)`.\n");
    out.push_str("  - Replace any inline `/* AGENT: ... */` placeholders nearby with concrete values too.\n");
    out.push_str("  - Do NOT modify the `// ---- GENERATED BY QEDGEN ----` header.\n");

    out.push('\n');
    out.push_str(SEP);
    out.push('\n');
    out
}

pub fn emit_prompts(opts: &FillOpts<'_>) -> Result<usize> {
    let mut prompts = Vec::new();

    for handler in &opts.spec.handlers {
        if let Some(only) = opts.only_handler {
            if handler.name != only {
                continue;
            }
        }
        let handler_file = handler_file_path(opts.programs_dir, &opts.spec.program_name, handler);
        if !handler_file.exists() {
            // Scaffold not generated yet — nothing to fill.
            continue;
        }
        let body = std::fs::read_to_string(&handler_file)
            .with_context(|| format!("reading {}", handler_file.display()))?;
        if !needs_fill(&body) {
            continue;
        }
        prompts.push(build_prompt(opts.spec, opts.spec_path, handler, &handler_file, &body));
    }

    if prompts.is_empty() {
        eprintln!(
            "qedgen fill — nothing to fill (no handler files contain `todo!(`)."
        );
        return Ok(0);
    }

    println!(
        "qedgen-fill: {} prompt(s) — copy these to your agent or act on them in this session.\n",
        prompts.len()
    );
    for p in &prompts {
        println!("{}", p);
    }
    Ok(prompts.len())
}

fn handler_file_path(programs_dir: &Path, program_name: &str, handler: &ParsedHandler) -> PathBuf {
    // Codegen writes to <programs_dir>/<program_name>/src/instructions/<handler>.rs
    // when the spec is multi-program, and <programs_dir>/src/instructions/<handler>.rs
    // for single-program layouts. Try both, prefer the nested one.
    let lower = program_name.to_lowercase().replace('_', "-");
    let nested = programs_dir
        .join(&lower)
        .join("src/instructions")
        .join(format!("{}.rs", handler.name));
    if nested.exists() {
        return nested;
    }
    programs_dir
        .join("src/instructions")
        .join(format!("{}.rs", handler.name))
}

fn needs_fill(body: &str) -> bool {
    // The M3 expander emits a focused `todo!("fill non-mechanical ...")`
    // when something remains; a fully-mechanized handler ends in `Ok(())`.
    // Skip comment lines that merely mention todo!() in prose.
    body.lines().any(|l| {
        l.contains("todo!(") && !l.trim_start().starts_with("//")
    })
}

fn build_prompt(
    spec: &ParsedSpec,
    spec_path: &Path,
    handler: &ParsedHandler,
    handler_file: &Path,
    body: &str,
) -> String {
    let todo_line = body
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("todo!("))
        .map(|(i, _)| i + 1)
        .unwrap_or(0);

    let spec_h = crate::spec_hash::spec_hash_for_handler(
        &std::fs::read_to_string(spec_path).unwrap_or_default(),
        &handler.name,
    )
    .unwrap_or_default();

    let mut out = String::new();
    out.push_str(SEP);
    out.push('\n');
    out.push_str(&format!(
        "QEDGEN-FILL: {}\n",
        handler_file.display()
    ));
    out.push_str(&format!(
        "handler: {}    spec: {}    spec_hash: {}\n",
        handler.name,
        spec_path.display(),
        spec_h
    ));
    out.push_str(HALF);
    out.push_str("\n\n");

    out.push_str(&format!(
        "Replace the `todo!(...)` at line {} with the implementation that\n\
         satisfies the spec contract below.\n\n",
        todo_line
    ));

    // -- Spec contract -------------------------------------------------
    out.push_str("Spec contract:\n");
    if let Some(who) = &handler.who {
        out.push_str(&format!("  who:     {} (signer)\n", who));
    }
    if let Some(pre) = &handler.pre_status {
        out.push_str(&format!("  pre:     {}\n", pre));
    }
    if let Some(post) = &handler.post_status {
        out.push_str(&format!("  post:    {}\n", post));
    }
    if !handler.requires.is_empty() {
        out.push_str("  guards:  ");
        for (i, r) in handler.requires.iter().enumerate() {
            if i > 0 {
                out.push_str("\n           ");
            }
            out.push_str(&r.rust_expr);
            if let Some(err) = &r.error_name {
                out.push_str(&format!("  else {}", err));
            }
        }
        out.push('\n');
    }

    // -- Effects: already mechanized vs needs fill --------------------
    let state_acct_name = state_account_name(handler);
    let mut mechanized_lines = Vec::new();
    let mut unfilled_effects = Vec::new();
    for (field, op_kind, value) in &handler.effects {
        let simple_rhs = value.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-');
        if let (true, Some(acct)) = (simple_rhs, state_acct_name.as_ref()) {
            let line = match op_kind.as_str() {
                "set" => format!("self.{}.{} = {};", acct, field, value),
                "add" => format!(
                    "self.{}.{} = self.{}.{}.wrapping_add({});",
                    acct, field, acct, field, value
                ),
                "sub" => format!(
                    "self.{}.{} = self.{}.{}.wrapping_sub({});",
                    acct, field, acct, field, value
                ),
                _ => format!("// {} {} {}", field, op_kind, value),
            };
            mechanized_lines.push(line);
        } else {
            unfilled_effects.push((field, op_kind, value));
        }
    }
    out.push_str("  effects (already mechanized — keep as-is):\n");
    if mechanized_lines.is_empty() {
        out.push_str("           — none —\n");
    } else {
        for l in &mechanized_lines {
            out.push_str(&format!("           {}\n", l));
        }
    }
    out.push_str("  effects (NEEDS FILL):\n");
    if unfilled_effects.is_empty() {
        out.push_str("           — none —\n");
    } else {
        for (f, o, v) in &unfilled_effects {
            out.push_str(&format!("           {} {} {}\n", f, o, v));
        }
    }

    // -- Events --------------------------------------------------------
    out.push_str("  events (NEEDS FILL):\n");
    if handler.emits.is_empty() {
        out.push_str("           — none —\n");
    } else {
        for e in &handler.emits {
            // Look up the event payload schema from spec.events.
            let payload = spec
                .events
                .iter()
                .find(|ev| ev.name == *e)
                .map(|ev| {
                    let fields: Vec<String> = ev
                        .fields
                        .iter()
                        .map(|(n, _t)| format!("{}: ?", n))
                        .collect();
                    format!("{{ {} }}", fields.join(", "))
                })
                .unwrap_or_else(|| "{ ... }".into());
            out.push_str(&format!("           emit!({} {});\n", e, payload));
        }
    }

    // -- Transfers -----------------------------------------------------
    out.push_str("  transfers (NEEDS FILL):\n");
    if handler.transfers.is_empty() {
        out.push_str("           — none —\n");
    } else {
        for t in &handler.transfers {
            let amt = t.amount.as_deref().unwrap_or("?");
            let auth = t.authority.as_deref().unwrap_or("?");
            out.push_str(&format!(
                "           {} -> {} amount={} authority={}\n",
                t.from, t.to, amt, auth
            ));
        }
    }
    out.push('\n');

    // -- Available accounts -------------------------------------------
    out.push_str("Available accounts in `&mut self`:\n");
    for acct in &handler.accounts {
        let parts = describe_account(acct);
        out.push_str(&format!("  {:<14} : {}\n", acct.name, parts));
    }

    // -- Available state fields ---------------------------------------
    if let Some(acct_name) = &state_acct_name {
        if let Some(at) = matching_account_type(spec, acct_name) {
            out.push_str(&format!(
                "\nAvailable state fields on `self.{}` (type {}):\n",
                acct_name, at.name
            ));
            for (n, t) in &at.fields {
                out.push_str(&format!("  {:<20} : {}\n", n, t));
            }
        }
    }

    // -- Constraints ---------------------------------------------------
    out.push_str("\nConstraints:\n");
    out.push_str("  - Keep the `guards::");
    out.push_str(&handler.name);
    out.push_str("(self, ...)?;` call as the first statement.\n");
    out.push_str("  - Keep mechanically-expanded effect lines exactly as written.\n");
    out.push_str("  - Replace the `todo!(...)` line with the remaining effects + events + transfers, then `Ok(())`.\n");
    out.push_str("  - Do not modify the `#[qed(verified, ...)]` attribute (drift detection).\n");
    out.push_str("  - Use the existing `crate::events::*` and `crate::guards::*` imports.\n");

    out.push('\n');
    out.push_str(SEP);
    out.push('\n');
    out
}

fn state_account_name(handler: &ParsedHandler) -> Option<String> {
    let mut candidates: Vec<&crate::check::ParsedHandlerAccount> = handler
        .accounts
        .iter()
        .filter(|a| a.is_writable && !a.is_signer && !a.is_program)
        .filter(|a| !matches!(a.account_type.as_deref(), Some("token") | Some("mint")))
        .collect();
    let pda: Vec<_> = candidates.iter().copied().filter(|a| a.pda_seeds.is_some()).collect();
    if !pda.is_empty() {
        candidates = pda;
    }
    if candidates.len() == 1 {
        Some(candidates[0].name.clone())
    } else {
        None
    }
}

fn matching_account_type<'a>(
    spec: &'a ParsedSpec,
    acct_name: &str,
) -> Option<&'a crate::check::ParsedAccountType> {
    spec.account_types
        .iter()
        .find(|at| at.name.to_lowercase() == acct_name)
        .or_else(|| {
            spec.account_types.iter().find(|at| {
                acct_name.starts_with(&at.name.to_lowercase())
                    || at.fields.iter().any(|_| true)
            })
        })
}

fn describe_account(acct: &crate::check::ParsedHandlerAccount) -> String {
    let mut parts = Vec::new();
    if acct.is_signer {
        parts.push("Signer".to_string());
    }
    if acct.is_program {
        parts.push("Program".to_string());
    }
    match acct.account_type.as_deref() {
        Some("token") => parts.push("Account<Token>".into()),
        Some("mint") => parts.push("Account<Mint>".into()),
        _ => {}
    }
    if let Some(seeds) = &acct.pda_seeds {
        parts.push(format!("PDA seeds={:?}", seeds));
    }
    if acct.is_writable {
        parts.push("writable".into());
    }
    if let Some(auth) = &acct.authority {
        parts.push(format!("authority={}", auth));
    }
    if parts.is_empty() {
        "Account<()>".to_string()
    } else {
        parts.join(", ")
    }
}
