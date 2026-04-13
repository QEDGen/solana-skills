// SPEC.md generator from Anchor IDL
//
// Reads the raw IDL JSON and generates a draft verification spec with:
// - Security goals inferred from instruction patterns
// - State model from IDL types
// - Operations with pre/postconditions from instruction accounts/args
// - Formal property skeletons
// - Trust boundary

use anyhow::Result;
use serde::Deserialize;
use std::fmt::Write;
use std::path::Path;

// ── IDL types (richer than analyzer's subset) ──────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct Idl {
    pub metadata: IdlMetadata,
    #[serde(default)]
    pub instructions: Vec<IdlInstruction>,
    #[serde(default)]
    pub types: Vec<IdlTypeDef>,
    #[serde(default)]
    pub errors: Vec<IdlError>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlMetadata {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlInstruction {
    pub name: String,
    #[serde(default)]
    pub docs: Vec<String>,
    #[serde(default)]
    pub accounts: Vec<IdlAccount>,
    #[serde(default)]
    pub args: Vec<IdlArg>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlAccount {
    pub name: String,
    #[serde(default)]
    pub signer: bool,
    #[serde(default)]
    pub writable: bool,
    #[serde(default)]
    pub pda: Option<IdlPda>,
    #[serde(default)]
    pub relations: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlPda {
    #[serde(default)]
    pub seeds: Vec<IdlSeed>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlSeed {
    #[serde(default)]
    #[allow(dead_code)]
    pub kind: String,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlArg {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlTypeDef {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: IdlTypeBody,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlTypeBody {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub fields: Vec<IdlField>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlField {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlError {
    pub name: String,
    #[allow(dead_code)]
    pub msg: String,
}

// ── Inferred patterns ──────────────────────────────────────────────────────

pub(crate) struct InstructionAnalysis {
    pub name: String,
    pub display_name: String,
    pub docs: String,
    pub signers: Vec<String>,
    #[allow(dead_code)]
    pub writable_accounts: Vec<String>,
    #[allow(dead_code)]
    pub pda_accounts: Vec<String>,
    pub has_one_relations: Vec<(String, String)>, // (account, related_to)
    pub args: Vec<(String, String)>,              // (name, type)
    pub has_token_program: bool,
    pub has_close_semantics: bool,
    pub has_numeric_args: bool,
}

// ── Generator ──────────────────────────────────────────────────────────────

pub(crate) fn parse_idl(idl_path: &Path) -> Result<(Idl, Vec<InstructionAnalysis>)> {
    let idl_source = std::fs::read_to_string(idl_path)?;
    let idl: Idl = serde_json::from_str(&idl_source)?;
    let analyses: Vec<InstructionAnalysis> =
        idl.instructions.iter().map(analyze_instruction).collect();
    Ok((idl, analyses))
}

pub fn generate_spec(idl_path: &Path, output_path: &Path) -> Result<()> {
    let (idl, analyses) = parse_idl(idl_path)?;
    let spec = build_spec(&idl, &analyses);

    std::fs::create_dir_all(output_path)?;
    std::fs::write(output_path.join("SPEC.md"), spec)?;
    println!("Wrote {}", output_path.join("SPEC.md").display());

    Ok(())
}

pub(crate) fn analyze_instruction(ix: &IdlInstruction) -> InstructionAnalysis {
    let signers: Vec<String> = ix
        .accounts
        .iter()
        .filter(|a| a.signer)
        .map(|a| a.name.clone())
        .collect();

    let writable_accounts: Vec<String> = ix
        .accounts
        .iter()
        .filter(|a| a.writable)
        .map(|a| a.name.clone())
        .collect();

    let pda_accounts: Vec<String> = ix
        .accounts
        .iter()
        .filter(|a| a.pda.is_some())
        .map(|a| a.name.clone())
        .collect();

    let has_one_relations: Vec<(String, String)> = ix
        .accounts
        .iter()
        .flat_map(|a| a.relations.iter().map(move |r| (a.name.clone(), r.clone())))
        .collect();

    let args: Vec<(String, String)> = ix
        .args
        .iter()
        .map(|a| (a.name.clone(), type_label(&a.ty)))
        .collect();

    let has_token_program = ix.accounts.iter().any(|a| a.name.contains("token_program"));

    // Close semantics: non-init instruction with a writable PDA state account
    // and either has_one relations or no args (terminal operations typically take no args)
    let has_writable_pda = ix.accounts.iter().any(|a| a.writable && a.pda.is_some());
    let has_relations = ix.accounts.iter().any(|a| !a.relations.is_empty());
    let is_init = ix.name.contains("init");
    let has_close_semantics = has_writable_pda && !is_init && (has_relations || ix.args.is_empty());

    let has_numeric_args = args
        .iter()
        .any(|(_, ty)| ty.starts_with('u') || ty.starts_with('i'));

    InstructionAnalysis {
        name: ix.name.clone(),
        display_name: snake_to_title(&ix.name),
        docs: ix.docs.join(" ").trim().to_string(),
        signers,
        writable_accounts,
        pda_accounts,
        has_one_relations,
        args,
        has_token_program,
        has_close_semantics,
        has_numeric_args,
    }
}

fn build_spec(idl: &Idl, analyses: &[InstructionAnalysis]) -> String {
    let mut s = String::new();
    let program_name = snake_to_title(&idl.metadata.name);

    // Header
    writeln!(s, "# {} Verification Spec v1.0\n", program_name).unwrap();

    // Program summary from instruction docs
    let doc_summary: Vec<String> = analyses
        .iter()
        .filter(|a| !a.docs.is_empty())
        .map(|a| format!("- **{}**: {}", a.display_name, a.docs))
        .collect();
    if !doc_summary.is_empty() {
        writeln!(
            s,
            "<!-- TODO: Replace with a 1-2 sentence summary in your own words -->\n"
        )
        .unwrap();
        for line in &doc_summary {
            writeln!(s, "{}", line).unwrap();
        }
        writeln!(s).unwrap();
    }

    // §0 Security Goals
    writeln!(s, "## 0. Security Goals\n").unwrap();
    writeln!(s, "The program MUST provide the following properties:\n").unwrap();

    let mut goal_num = 1;

    // Access control goals
    let signers_instructions: Vec<&InstructionAnalysis> =
        analyses.iter().filter(|a| !a.signers.is_empty()).collect();
    if !signers_instructions.is_empty() {
        let names: Vec<&str> = signers_instructions
            .iter()
            .map(|a| a.display_name.as_str())
            .collect();
        writeln!(
            s,
            "{}. **Authorization**: Only authorized signers MAY execute {}. \
            Each operation MUST verify the signer matches the expected authority.",
            goal_num,
            names.join(", ")
        )
        .unwrap();
        goal_num += 1;
    }

    // CPI correctness goals
    let cpi_instructions: Vec<&InstructionAnalysis> =
        analyses.iter().filter(|a| a.has_token_program).collect();
    if !cpi_instructions.is_empty() {
        let names: Vec<&str> = cpi_instructions
            .iter()
            .map(|a| a.display_name.as_str())
            .collect();
        writeln!(
            s,
            "{}. **CPI parameter correctness**: CPI invocations in {} MUST target \
            the correct program, pass accounts in the correct order with correct \
            signer/writable flags, and use the correct instruction discriminator.",
            goal_num,
            names.join(", ")
        )
        .unwrap();
        goal_num += 1;
    }

    // State machine goals
    let close_instructions: Vec<&InstructionAnalysis> =
        analyses.iter().filter(|a| a.has_close_semantics).collect();
    if !close_instructions.is_empty() {
        let names: Vec<&str> = close_instructions
            .iter()
            .map(|a| a.display_name.as_str())
            .collect();
        writeln!(
            s,
            "{}. **One-shot safety**: After {} completes, the account MUST be closed \
            and MUST NOT be reusable.",
            goal_num,
            names.join(" or ")
        )
        .unwrap();
        goal_num += 1;
    }

    // Arithmetic goals
    let numeric_instructions: Vec<&InstructionAnalysis> =
        analyses.iter().filter(|a| a.has_numeric_args).collect();
    if !numeric_instructions.is_empty() {
        writeln!(
            s,
            "{}. **Arithmetic bounds**: Numeric parameters MUST NOT overflow during processing.",
            goal_num
        )
        .unwrap();
    }

    writeln!(s).unwrap();

    // §1 State Model
    writeln!(s, "## 1. State Model\n").unwrap();
    for ty in &idl.types {
        if ty.ty.kind == "struct" {
            writeln!(s, "```").unwrap();
            writeln!(s, "{} {{", ty.name).unwrap();
            let max_name_len = ty.ty.fields.iter().map(|f| f.name.len()).max().unwrap_or(0);
            for field in &ty.ty.fields {
                writeln!(
                    s,
                    "  {:<width$}  {}",
                    format!("{}:", field.name),
                    type_label(&field.ty),
                    width = max_name_len + 1
                )
                .unwrap();
            }
            writeln!(s, "}}").unwrap();
            writeln!(s, "```\n").unwrap();
        }
    }

    // PDA derivations
    // Extract PDAs from the raw IDL
    let mut seen_pdas = std::collections::HashSet::new();
    for ix in &idl.instructions {
        for acct in &ix.accounts {
            if let Some(pda) = &acct.pda {
                if seen_pdas.insert(acct.name.clone()) {
                    let seeds: Vec<String> = pda
                        .seeds
                        .iter()
                        .map(|seed| {
                            if let Some(path) = &seed.path {
                                path.to_string()
                            } else if let Some(serde_json::Value::Array(bytes)) = &seed.value {
                                let values: Vec<u8> = bytes
                                    .iter()
                                    .filter_map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
                                    .collect();
                                String::from_utf8(values).unwrap_or_else(|_| "const".into())
                            } else {
                                "const".into()
                            }
                        })
                        .collect();
                    writeln!(
                        s,
                        "`{}` is a PDA derived from `[\"{}\"]`.",
                        acct.name,
                        seeds.join("\", \"")
                    )
                    .unwrap();
                }
            }
        }
    }
    writeln!(s).unwrap();

    // Lifecycle
    let has_lifecycle = analyses.iter().any(|a| a.has_close_semantics);
    if has_lifecycle {
        writeln!(s, "### Lifecycle\n").unwrap();
        writeln!(
            s,
            "<!-- TODO: Describe the lifecycle states and transitions -->\n"
        )
        .unwrap();
        writeln!(s, "```").unwrap();
        let init = analyses.iter().find(|a| a.name.contains("init"));
        let terminals: Vec<&str> = analyses
            .iter()
            .filter(|a| a.has_close_semantics)
            .map(|a| a.name.as_str())
            .collect();
        if let Some(init_ix) = init {
            write!(s, "{}  →  [open]", init_ix.name).unwrap();
            for (i, t) in terminals.iter().enumerate() {
                if i == 0 {
                    writeln!(s, "  →  {}  →  [closed]", t).unwrap();
                } else {
                    writeln!(
                        s,
                        "{}→  {}  →  [closed]",
                        " ".repeat(init_ix.name.len() + 12),
                        t
                    )
                    .unwrap();
                }
            }
        }
        writeln!(s, "```\n").unwrap();
    }

    // §2 Operations
    writeln!(s, "## 2. Operations\n").unwrap();
    for (i, analysis) in analyses.iter().enumerate() {
        writeln!(s, "### 2.{} {}\n", i + 1, analysis.display_name).unwrap();

        if !analysis.docs.is_empty() {
            writeln!(s, "{}\n", analysis.docs).unwrap();
        }

        // Signers
        if !analysis.signers.is_empty() {
            writeln!(
                s,
                "**Signers**: {} (MUST sign)\n",
                analysis
                    .signers
                    .iter()
                    .map(|s| format!("`{}`", s))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .unwrap();
        }

        // Preconditions from errors, relations, args
        writeln!(s, "**Preconditions**:").unwrap();
        for (acct, rel) in &analysis.has_one_relations {
            writeln!(
                s,
                "- `{}` MUST match `{}.{}` (enforced by `has_one`)",
                rel, acct, rel
            )
            .unwrap();
        }
        // Check if there are related errors
        for err in &idl.errors {
            // Heuristic: associate errors with instructions that have matching arg patterns
            let err_lower = err.name.to_lowercase();
            let args_lower: Vec<String> = analysis
                .args
                .iter()
                .map(|(n, _)| n.to_lowercase())
                .collect();
            if args_lower.iter().any(|a| err_lower.contains(a))
                || err_lower.contains(&analysis.name.to_lowercase())
            {
                writeln!(s, "- `{}`: {}", err.name, err.msg).unwrap();
            }
        }
        writeln!(s).unwrap();

        // Effects
        writeln!(s, "**Effects**:").unwrap();
        let mut effect_num = 1;
        if analysis.has_token_program {
            // Find the corresponding IDL instruction for full account details
            if let Some(idl_ix) = idl.instructions.iter().find(|i| i.name == analysis.name) {
                let writable_token: Vec<&IdlAccount> = idl_ix
                    .accounts
                    .iter()
                    .filter(|a| {
                        a.writable && a.name.contains("token") && !a.name.contains("program")
                    })
                    .collect();
                // Separate PDA vaults from user accounts
                let vaults: Vec<&&IdlAccount> =
                    writable_token.iter().filter(|a| a.pda.is_some()).collect();
                let user_accounts: Vec<&&IdlAccount> =
                    writable_token.iter().filter(|a| a.pda.is_none()).collect();

                if analysis.name.contains("init") {
                    // Init: user → vault
                    for (u, v) in user_accounts.iter().zip(vaults.iter()) {
                        writeln!(
                            s,
                            "{}. Transfer tokens: `{}` → `{}`",
                            effect_num, u.name, v.name
                        )
                        .unwrap();
                        effect_num += 1;
                    }
                } else if vaults.len() == 1 && user_accounts.len() == 1 {
                    // Terminal with one transfer: vault → user (e.g., cancel)
                    writeln!(
                        s,
                        "{}. Transfer tokens: `{}` → `{}`",
                        effect_num, vaults[0].name, user_accounts[0].name
                    )
                    .unwrap();
                    effect_num += 1;
                } else {
                    // Multiple transfers: list all writable token accounts as
                    // TODO for the user to specify direction
                    for pair in writable_token.chunks(2) {
                        if pair.len() == 2 {
                            writeln!(
                                s,
                                "{}. Transfer tokens: `{}` ↔ `{}` <!-- TODO: confirm direction -->",
                                effect_num, pair[0].name, pair[1].name
                            )
                            .unwrap();
                            effect_num += 1;
                        }
                    }
                }
            }
        }
        if analysis.has_close_semantics {
            writeln!(s, "{}. Close account, return rent to signer", effect_num).unwrap();
        }
        if !analysis.has_token_program && !analysis.has_close_semantics {
            // Creation or state change
            if analysis.name.contains("init") {
                writeln!(s, "{}. Create account and initialize state", effect_num).unwrap();
            } else {
                writeln!(s, "{}. <!-- TODO: Describe effects -->", effect_num).unwrap();
            }
        }
        writeln!(s).unwrap();

        // Postconditions
        writeln!(s, "**Postconditions**:").unwrap();
        if analysis.has_close_semantics {
            writeln!(s, "- Account MUST NOT exist (closed)").unwrap();
        }
        if analysis.name.contains("init") {
            for (arg_name, _) in &analysis.args {
                writeln!(s, "- State `{}` MUST equal the provided argument", arg_name).unwrap();
            }
        }
        writeln!(s, "- <!-- TODO: Add postconditions -->\n").unwrap();
    }

    // §3 Formal Properties
    writeln!(s, "## 3. Formal Properties\n").unwrap();

    // Access control
    if !signers_instructions.is_empty() {
        writeln!(s, "### 3.1 Access Control\n").unwrap();
        for analysis in &signers_instructions {
            writeln!(
                s,
                "**{}_access_control**: For all states `s` and signers `p`,",
                analysis.name
            )
            .unwrap();
            writeln!(
                s,
                "if `{}Transition(s, p) ≠ none` then `p = s.{}`.  ",
                analysis.name,
                analysis
                    .has_one_relations
                    .first()
                    .map(|(_, r)| r.as_str())
                    .unwrap_or("<!-- TODO: authority field -->")
            )
            .unwrap();
            writeln!(s).unwrap();
        }
    }

    // CPI correctness
    if !cpi_instructions.is_empty() {
        writeln!(s, "### 3.2 CPI Parameter Correctness\n").unwrap();
        for analysis in &cpi_instructions {
            writeln!(
                s,
                "**{}_cpi_correct**: For all contexts `ctx`,",
                analysis.name
            )
            .unwrap();
            writeln!(s, "`{}_build_cpi(ctx)` MUST target the correct program, pass accounts in the correct order with correct signer/writable flags, and use the correct instruction discriminator.  ",
                analysis.name).unwrap();
            writeln!(s).unwrap();
        }
    }

    // State machine
    if !close_instructions.is_empty() {
        writeln!(s, "### 3.3 State Machine Safety\n").unwrap();
        for analysis in &close_instructions {
            writeln!(
                s,
                "**{}_closes_account**: For all states `s, s'`,",
                analysis.name
            )
            .unwrap();
            writeln!(
                s,
                "if `{}Transition(s) = some s'` then `s'.lifecycle = closed`.  ",
                analysis.name
            )
            .unwrap();
            writeln!(s).unwrap();
        }
    }

    // Arithmetic
    if !numeric_instructions.is_empty() {
        writeln!(s, "### 3.4 Arithmetic Safety\n").unwrap();
        writeln!(s, "**arithmetic_bounds**: All numeric parameters MUST satisfy `value ≤ MAX` after any computation.  ").unwrap();
        writeln!(s).unwrap();
    }

    // §4 Trust Boundary
    writeln!(s, "## 4. Trust Boundary\n").unwrap();
    writeln!(s, "The following are axiomatic (not verified):\n").unwrap();
    writeln!(s, "- **SPL Token program**: Transfer semantics are correct. We verify parameters passed, not the transfer itself.").unwrap();
    writeln!(
        s,
        "- **Solana runtime**: PDA derivation, account ownership enforcement, rent collection."
    )
    .unwrap();
    writeln!(s, "- **Anchor framework**: Constraint enforcement (`has_one`, `signer`, account deserialization, close semantics).").unwrap();
    writeln!(s).unwrap();

    // §5 Verification Results (empty table)
    writeln!(s, "## 5. Verification Results\n").unwrap();
    writeln!(s, "| Property | Status | Proof |").unwrap();
    writeln!(s, "|---|---|---|").unwrap();
    for analysis in analyses {
        if !analysis.signers.is_empty() {
            writeln!(s, "| {}_access_control | **Open** | |", analysis.name).unwrap();
        }
        if analysis.has_token_program {
            writeln!(s, "| {}_cpi_correct | **Open** | |", analysis.name).unwrap();
        }
        if analysis.has_close_semantics {
            writeln!(s, "| {}_closes_account | **Open** | |", analysis.name).unwrap();
        }
    }
    if analyses.iter().any(|a| a.has_numeric_args) {
        writeln!(s, "| arithmetic_bounds | **Open** | |").unwrap();
    }
    writeln!(s).unwrap();

    s
}

pub(crate) fn type_label(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ── From-spec generator ─────────────────────────────────────────────────

/// Generate SPEC.md from a .qedspec file.
/// Optionally includes proof status from a proofs directory.
pub fn generate_spec_from_qedspec(
    spec_path: &Path,
    proofs_dir: Option<&Path>,
    output_path: &Path,
) -> Result<()> {
    let parsed = crate::check::parse_spec_file(spec_path)?;

    let program_name = parsed.program_name.clone();
    let state_fields = parsed.state_fields.clone();
    let lifecycle_states = parsed.lifecycle_states.clone();

    // Optionally check proof status
    let proof_results = if let Some(proofs) = proofs_dir {
        crate::check::check(spec_path, proofs).ok()
    } else {
        None
    };

    // Build SPEC.md
    let mut s = String::new();

    // Header
    writeln!(s, "# {} Verification Spec\n", program_name).unwrap();

    // §1 State Model
    writeln!(s, "## 1. State Model\n").unwrap();
    writeln!(s, "| Field | Type |").unwrap();
    writeln!(s, "|-------|------|").unwrap();
    for (name, typ) in &state_fields {
        writeln!(s, "| {} | {} |", name, typ).unwrap();
    }
    writeln!(s).unwrap();

    if !lifecycle_states.is_empty() {
        writeln!(s, "**Lifecycle:** {}\n", lifecycle_states.join(" → ")).unwrap();
    }

    // §2 Operations
    writeln!(s, "## 2. Operations\n").unwrap();
    for op in &parsed.operations {
        writeln!(s, "### {}\n", op.name).unwrap();
        if let Some(ref doc) = op.doc {
            writeln!(s, "{}\n", doc).unwrap();
        }
        if let Some(ref signer) = op.who {
            writeln!(s, "- **Signer:** `{}`", signer).unwrap();
        }
        if op.has_when {
            let pre = op.pre_status.as_deref().unwrap_or("?");
            let post = op.post_status.as_deref().unwrap_or("?");
            writeln!(s, "- **Lifecycle:** {} → {}", pre, post).unwrap();
        }
        if op.has_guard {
            if let Some(ref guard) = op.guard_str {
                writeln!(s, "- **Guard:** `{}`", guard).unwrap();
            }
        }
        if op.has_calls {
            let program = op.program_id.as_deref().unwrap_or("?");
            writeln!(s, "- **CPI:** `{}`", program).unwrap();
        }
        writeln!(s).unwrap();
    }

    // §3 Formal Properties
    writeln!(s, "## 3. Formal Properties\n").unwrap();

    if let Some(ref results) = proof_results {
        writeln!(s, "| Property | Intent | Status |").unwrap();
        writeln!(s, "|----------|--------|--------|").unwrap();
        for r in results {
            let icon = match r.status {
                crate::check::Status::Proven => "✓",
                crate::check::Status::Sorry => "✗",
                crate::check::Status::Missing => "—",
            };
            let intent = r.intent.as_deref().unwrap_or("");
            writeln!(s, "| {} | {} | {} {} |", r.name, intent, icon, r.status).unwrap();
        }
    } else {
        // No proof status available — list expected properties
        writeln!(s, "| Property | Intent |").unwrap();
        writeln!(s, "|----------|--------|").unwrap();
        for op in &parsed.operations {
            if op.who.is_some() {
                writeln!(
                    s,
                    "| {}.access_control | Only {} can call {} |",
                    op.name,
                    op.who.as_deref().unwrap_or("?"),
                    op.name
                )
                .unwrap();
            }
            if op.has_when {
                writeln!(
                    s,
                    "| {}.state_machine | {} → {} |",
                    op.name,
                    op.pre_status.as_deref().unwrap_or("?"),
                    op.post_status.as_deref().unwrap_or("?")
                )
                .unwrap();
            }
            if op.has_calls {
                writeln!(
                    s,
                    "| {}.cpi_correct | CPI targets {} |",
                    op.name,
                    op.program_id.as_deref().unwrap_or("?")
                )
                .unwrap();
            }
        }
        for (name, desc) in &parsed.invariants {
            writeln!(s, "| {} | {} |", name, desc).unwrap();
        }
    }
    writeln!(s).unwrap();

    // §4 Trust Boundary
    writeln!(s, "## 4. Trust Boundary\n").unwrap();
    writeln!(s, "The following are axiomatic (not verified):\n").unwrap();

    // Detect which programs are referenced
    let mut programs = Vec::new();
    for op in &parsed.operations {
        if let Some(ref pid) = op.program_id {
            if !programs.contains(pid) {
                programs.push(pid.clone());
            }
        }
    }
    for pid in &programs {
        if pid.contains("TOKEN") {
            writeln!(s, "- **SPL Token program**: Transfer semantics are correct. We verify parameters passed, not the transfer itself.").unwrap();
        } else if pid.contains("SYSTEM") {
            writeln!(
                s,
                "- **System Program**: Account creation and SOL transfer semantics."
            )
            .unwrap();
        }
    }
    writeln!(
        s,
        "- **Solana runtime**: PDA derivation, account ownership enforcement."
    )
    .unwrap();
    writeln!(s, "- **CPI mechanics**: `invoke_signed` faithfulness.").unwrap();
    writeln!(s).unwrap();

    writeln!(
        s,
        "---\n\n*Generated from `{}` by `qedgen spec --from-spec`*",
        spec_path.display()
    )
    .unwrap();

    std::fs::create_dir_all(output_path)?;
    let out_file = output_path.join("SPEC.md");
    std::fs::write(&out_file, &s)?;
    eprintln!("Wrote {}", out_file.display());

    Ok(())
}

pub(crate) fn snake_to_title(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
