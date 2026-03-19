use crate::ir::{
    AccountFieldIr, AccountsStructIr, AnalysisIr, ConstraintIr, InstructionIr, PropertyCandidateIr,
    TestSignalIr, TransferIr, CoverageSummaryIr, ProofObligationIr, ProofPlanIr,
    SupportedSurfaceIr,
};
use anchor_syn::{AccountField, AccountsStruct, ConstraintGroup, Program, Ty};
use quote::ToTokens;
use regex::Regex;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use syn::{Attribute, File, Item};

pub fn analyze_project(
    idl_path: Option<&Path>,
    input: Option<&Path>,
    tests: &[PathBuf],
    output_dir: Option<&Path>,
) -> Result<String, String> {
    let source = if let Some(input) = input {
        Some(fs::read_to_string(input).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let parsed = if let Some(source) = &source {
        Some(syn::parse_file(source).map_err(|e| e.to_string())?)
    } else {
        None
    };

    let mut instructions = Vec::new();
    let mut accounts = Vec::new();

    if let Some(idl_path) = idl_path {
        let idl_source = fs::read_to_string(idl_path).map_err(|e| e.to_string())?;
        let idl: AnchorIdl = serde_json::from_str(&idl_source).map_err(|e| e.to_string())?;
        instructions = parse_idl_instructions(&idl);
        accounts = parse_idl_accounts(&idl);
    }

    if let Some(parsed) = &parsed {
        merge_instructions(&mut instructions, parse_program(parsed)?);
        merge_accounts(&mut accounts, parse_accounts(parsed)?);
    }

    let test_signals = parse_tests(tests)?;
    let property_candidates = derive_property_candidates(&instructions, &accounts, &test_signals);
    let proof_plan = build_proof_plan(&property_candidates, &instructions, &accounts);

    let ir = AnalysisIr {
        source_file: input
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        idl_file: idl_path.map(|path| path.display().to_string()),
        test_files: tests.iter().map(|p| p.display().to_string()).collect(),
        instructions,
        accounts,
        test_signals,
        property_candidates,
        proof_plan,
    };

    let json = serde_json::to_string_pretty(&ir).map_err(|e| e.to_string())?;
    if let Some(output_dir) = output_dir {
        fs::create_dir_all(output_dir).map_err(|e| e.to_string())?;
        fs::write(output_dir.join("analysis.json"), &json).map_err(|e| e.to_string())?;
        let proof_plan_json =
            serde_json::to_string_pretty(&ir.proof_plan).map_err(|e| e.to_string())?;
        fs::write(output_dir.join("proof_plan.json"), proof_plan_json)
            .map_err(|e| e.to_string())?;

        for obligation in &ir.proof_plan.obligations {
            let prompt = build_prompt(
                source.as_deref().unwrap_or(""),
                obligation,
                &ir.proof_plan.supported_surface,
            );
            fs::write(output_dir.join(format!("{}.prompt.txt", obligation.id)), prompt)
                .map_err(|e| e.to_string())?;
        }

        Ok(format!(
            "Wrote analysis to {}",
            output_dir.join("analysis.json").display()
        ))
    } else {
        Ok(json)
    }
}

fn build_proof_plan(
    candidates: &[PropertyCandidateIr],
    instructions: &[InstructionIr],
    accounts: &[AccountsStructIr],
) -> ProofPlanIr {
    let mut unsupported_reasons = Vec::new();

    if instructions.iter().any(|ix| ix.transfers.iter().any(|t| t.uses_pda_signer)) {
        unsupported_reasons.push(
            "PDA signer semantics are approximated at the instruction level, not modeled from runtime semantics.".into(),
        );
    }

    if accounts.iter().any(|account| account.fields.iter().any(|field| field.ty.contains("AccountInfo"))) {
        unsupported_reasons.push(
            "Unchecked AccountInfo fields require conservative assumptions unless explicit invariants are extracted.".into(),
        );
    }

    let obligations: Vec<ProofObligationIr> = candidates
        .iter()
        .map(|candidate| ProofObligationIr {
            id: candidate.id.clone(),
            title: candidate.title.clone(),
            category: candidate.category.clone(),
            relevant_instructions: candidate.relevant_instructions.clone(),
            lean_support_modules: support_modules_for_category(&candidate.category),
            theorem_shape: theorem_shape_for_category(&candidate.category).into(),
            status: "planned".into(),
            notes: candidate.evidence.clone(),
        })
        .collect();

    let unsupported_obligations = obligations
        .iter()
        .filter(|obligation| obligation.lean_support_modules.is_empty())
        .count();

    ProofPlanIr {
        supported_surface: SupportedSurfaceIr {
            lean_support_modules: vec![
                "Leanstral.Solana.Account".into(),
                "Leanstral.Solana.Authority".into(),
                "Leanstral.Solana.Token".into(),
                "Leanstral.Solana.State".into(),
            ],
            supported_property_categories: vec![
                "access_control".into(),
                "conservation".into(),
                "state_machine".into(),
                "arithmetic_safety".into(),
            ],
            unsupported_reasons,
        },
        coverage: CoverageSummaryIr {
            total_obligations: obligations.len(),
            supported_obligations: obligations.len().saturating_sub(unsupported_obligations),
            unsupported_obligations,
        },
        obligations,
    }
}

fn support_modules_for_category(category: &str) -> Vec<String> {
    match category {
        "access_control" => vec![
            "Leanstral.Solana.Account".into(),
            "Leanstral.Solana.Authority".into(),
        ],
        "conservation" => vec![
            "Leanstral.Solana.Account".into(),
            "Leanstral.Solana.Token".into(),
        ],
        "state_machine" => vec![
            "Leanstral.Solana.Account".into(),
            "Leanstral.Solana.State".into(),
        ],
        "arithmetic_safety" => vec!["Leanstral.Solana.Token".into()],
        _ => Vec::new(),
    }
}

fn theorem_shape_for_category(category: &str) -> &'static str {
    match category {
        "access_control" => "transition_non_none_implies_signer_equals_initializer",
        "conservation" => "direct_balance_equality_preserves_tracked_total",
        "state_machine" => "cancel_transition_sets_lifecycle_closed",
        "arithmetic_safety" => "transition_preserves_numeric_bounds",
        _ => "custom",
    }
}

#[derive(Debug, Deserialize)]
struct AnchorIdl {
    #[serde(default)]
    instructions: Vec<IdlInstruction>,
    #[serde(default)]
    accounts: Vec<IdlAccountDef>,
}

#[derive(Debug, Deserialize)]
struct IdlInstruction {
    name: String,
    #[serde(default)]
    args: Vec<IdlArg>,
    #[serde(default)]
    accounts: Vec<IdlInstructionAccount>,
}

#[derive(Debug, Deserialize)]
struct IdlArg {
    name: String,
    #[serde(rename = "type")]
    ty: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct IdlInstructionAccount {
    name: String,
    #[serde(default)]
    signer: bool,
    #[serde(default)]
    _writable: bool,
    #[serde(default)]
    pda: Option<IdlPda>,
}

#[derive(Debug, Deserialize)]
struct IdlPda {
    #[serde(default)]
    seeds: Vec<IdlSeed>,
}

#[derive(Debug, Deserialize)]
struct IdlSeed {
    #[serde(default)]
    _kind: String,
    #[serde(default)]
    value: Option<serde_json::Value>,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IdlAccountDef {
    name: String,
    #[serde(default)]
    _discriminator: Vec<u8>,
}

fn parse_idl_instructions(idl: &AnchorIdl) -> Vec<InstructionIr> {
    idl.instructions
        .iter()
        .map(|ix| InstructionIr {
            name: ix.name.clone(),
            context_type: infer_context_type(&ix.name),
            args: ix
                .args
                .iter()
                .map(|arg| format!("{}: {}", arg.name, idl_type_label(&arg.ty)))
                .collect(),
            pda_seeds: ix
                .accounts
                .iter()
                .flat_map(|acct| acct.pda.iter())
                .flat_map(|pda| pda.seeds.iter())
                .filter_map(seed_label)
                .collect(),
            closes_accounts: Vec::new(),
            auth_signals: ix
                .accounts
                .iter()
                .filter(|acct| acct.signer)
                .map(|acct| format!("idl signer: {}", acct.name))
                .collect(),
            transfers: Vec::new(),
            evidence_sources: vec!["idl".into()],
        })
        .collect()
}

fn seed_label(seed: &IdlSeed) -> Option<String> {
    if let Some(path) = &seed.path {
        return Some(path.clone());
    }
    match &seed.value {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Array(bytes)) => {
            let values: Vec<u8> = bytes
                .iter()
                .filter_map(|value| value.as_u64().and_then(|n| u8::try_from(n).ok()))
                .collect();
            String::from_utf8(values).ok().or_else(|| Some("const_seed".into()))
        }
        Some(other) => Some(other.to_string()),
        None => None,
    }
}

fn idl_type_label(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn parse_idl_accounts(idl: &AnchorIdl) -> Vec<AccountsStructIr> {
    idl.accounts
        .iter()
        .map(|account| AccountsStructIr {
            name: account.name.clone(),
            fields: Vec::new(),
            evidence_sources: vec!["idl".into()],
        })
        .collect()
}

fn infer_context_type(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn merge_instructions(existing: &mut Vec<InstructionIr>, incoming: Vec<InstructionIr>) {
    for instruction in incoming {
        if let Some(current) = existing.iter_mut().find(|ix| ix.name == instruction.name) {
            merge_string_vec(&mut current.args, instruction.args);
            merge_string_vec(&mut current.pda_seeds, instruction.pda_seeds);
            merge_string_vec(&mut current.closes_accounts, instruction.closes_accounts);
            merge_string_vec(&mut current.auth_signals, instruction.auth_signals);
            current.transfers.extend(instruction.transfers);
            merge_string_vec(&mut current.evidence_sources, instruction.evidence_sources);
            if current.context_type.is_empty() {
                current.context_type = instruction.context_type;
            }
        } else {
            existing.push(instruction);
        }
    }
}

fn merge_accounts(existing: &mut Vec<AccountsStructIr>, incoming: Vec<AccountsStructIr>) {
    for account in incoming {
        if let Some(current) = existing.iter_mut().find(|a| a.name == account.name) {
            current.fields = account.fields;
            merge_string_vec(&mut current.evidence_sources, account.evidence_sources);
        } else {
            existing.push(account);
        }
    }
}

fn merge_string_vec(current: &mut Vec<String>, incoming: Vec<String>) {
    let mut set: BTreeSet<String> = current.iter().cloned().collect();
    set.extend(incoming);
    *current = set.into_iter().collect();
}

fn parse_program(file: &File) -> Result<Vec<InstructionIr>, String> {
    let mut instructions = Vec::new();

    for item in &file.items {
        if let Item::Mod(item_mod) = item {
            if has_attr(&item_mod.attrs, "program") {
                let program: Program = syn::parse2(item_mod.to_token_stream())
                    .map_err(|e| format!("failed to parse #[program] module: {e}"))?;

                for ix in program.ixs {
                    let body = ix.raw_method.block.to_token_stream().to_string();
                    instructions.push(InstructionIr {
                        name: ix.ident.to_string(),
                        context_type: ix.anchor_ident.to_string(),
                        args: ix
                            .args
                            .iter()
                            .map(|arg| format!("{}: {}", arg.name, arg.raw_arg.ty.to_token_stream()))
                            .collect(),
                        pda_seeds: extract_seed_literals(&body),
                        closes_accounts: extract_close_targets(&body),
                        auth_signals: extract_auth_signals(&body),
                        transfers: extract_transfers(&body),
                        evidence_sources: vec!["rust".into()],
                    });
                }
            }
        }
    }

    Ok(instructions)
}

fn parse_accounts(file: &File) -> Result<Vec<AccountsStructIr>, String> {
    let mut accounts = Vec::new();

    for item in &file.items {
        if let Item::Struct(item_struct) = item {
            if has_derive_accounts(&item_struct.attrs) {
                let parsed: AccountsStruct = syn::parse2(item_struct.to_token_stream())
                    .map_err(|e| format!("failed to parse Accounts struct {}: {e}", item_struct.ident))?;
                accounts.push(AccountsStructIr {
                    name: parsed.ident.to_string(),
                    fields: parsed.fields.iter().map(account_field_to_ir).collect(),
                    evidence_sources: vec!["rust".into()],
                });
            }
        }
    }

    Ok(accounts)
}

fn parse_tests(test_files: &[PathBuf]) -> Result<Vec<TestSignalIr>, String> {
    let re = Regex::new(r#"(it|test)\s*\(\s*["'`](.*?)["'`]"#).map_err(|e| e.to_string())?;
    let mut signals = Vec::new();

    for test_file in test_files {
        let source = fs::read_to_string(test_file).map_err(|e| e.to_string())?;
        for capture in re.captures_iter(&source) {
            let name = capture.get(2).map(|m| m.as_str()).unwrap_or_default().to_string();
            signals.push(TestSignalIr {
                file: test_file.display().to_string(),
                inferred_properties: infer_properties_from_text(&name),
                name,
            });
        }
    }

    Ok(signals)
}

fn derive_property_candidates(
    instructions: &[InstructionIr],
    accounts: &[AccountsStructIr],
    test_signals: &[TestSignalIr],
) -> Vec<PropertyCandidateIr> {
    let mut candidates = Vec::new();
    let test_props: BTreeSet<String> = test_signals
        .iter()
        .flat_map(|signal| signal.inferred_properties.iter().cloned())
        .collect();

    for instruction in instructions {
        let account_struct = accounts
            .iter()
            .find(|account| account.name == instruction.context_type);

        let has_auth_constraint = account_struct.map(|account| {
            account.fields.iter().any(|field| {
                field.is_signer
                    || field
                        .constraints
                        .iter()
                        .any(|constraint| matches!(constraint.kind.as_str(), "has_one" | "owner" | "address"))
            })
        });
        let has_close_constraint = account_struct.map(|account| {
            account.fields.iter().any(|field| {
                field.constraints
                    .iter()
                    .any(|constraint| constraint.kind == "close")
            })
        });

        if has_auth_constraint.unwrap_or(false)
            || !instruction.auth_signals.is_empty()
            || test_props.contains("access_control")
        {
            candidates.push(PropertyCandidateIr {
                id: format!("{}_access_control", instruction.name),
                category: "access_control".into(),
                title: format!("{}: access control", instruction.name),
                confidence: "high".into(),
                relevant_instructions: vec![instruction.name.clone()],
                evidence: instruction.auth_signals.clone(),
                prompt_hint: format!(
                    "Model only the authorization condition for {}. Prove success implies the caller matches the required authority relation.",
                    instruction.name
                ),
            });
        }

        if !instruction.transfers.is_empty() || test_props.contains("conservation") {
            candidates.push(PropertyCandidateIr {
                id: format!("{}_conservation", instruction.name),
                category: "conservation".into(),
                title: format!("{}: token conservation", instruction.name),
                confidence: if instruction.transfers.len() > 1 {
                    "high".into()
                } else {
                    "medium".into()
                },
                relevant_instructions: vec![instruction.name.clone()],
                evidence: instruction
                    .transfers
                    .iter()
                    .map(|transfer| {
                        format!(
                            "transfer {:?} -> {:?} amount {:?}",
                            transfer.from, transfer.to, transfer.amount_expr
                        )
                    })
                    .collect(),
                prompt_hint: format!(
                    "Model only the balances touched by {}. Prove the total tracked amount is preserved across the transition.",
                    instruction.name
                ),
            });
        }

        if has_close_constraint.unwrap_or(false)
            || !instruction.closes_accounts.is_empty()
            || test_props.contains("state_machine")
        {
            candidates.push(PropertyCandidateIr {
                id: format!("{}_state_machine", instruction.name),
                category: "state_machine".into(),
                title: format!("{}: close / one-shot safety", instruction.name),
                confidence: "medium".into(),
                relevant_instructions: vec![instruction.name.clone()],
                evidence: instruction.closes_accounts.clone(),
                prompt_hint: format!(
                    "Model only the lifecycle flag for the state account. Prove {} moves it to a terminal closed state.",
                    instruction.name
                ),
            });
        }
    }

    if instructions
        .iter()
        .flat_map(|ix| ix.args.iter())
        .any(|arg| arg.contains("u8") || arg.contains("u16") || arg.contains("u32") || arg.contains("u64") || arg.contains("u128"))
        || test_props.contains("arithmetic_safety")
    {
        candidates.push(PropertyCandidateIr {
            id: "program_arithmetic_safety".into(),
            category: "arithmetic_safety".into(),
            title: "Program arithmetic safety".into(),
            confidence: "medium".into(),
            relevant_instructions: instructions.iter().map(|ix| ix.name.clone()).collect(),
            evidence: instructions
                .iter()
                .flat_map(|ix| ix.args.iter().cloned())
                .filter(|arg| arg.contains('u'))
                .collect(),
            prompt_hint:
                "Model only the numeric parameters and arithmetic preconditions that matter. Avoid token/account semantics unless required."
                    .into(),
        });
    }

    candidates
}

fn build_prompt(
    source: &str,
    obligation: &ProofObligationIr,
    supported_surface: &SupportedSurfaceIr,
) -> String {
    let support_api = support_api_for_modules(&obligation.lean_support_modules);
    format!(
        "I need a single Lean 4 module that compiles under Lean 4.15 + Mathlib 4.15.\n\n\
		Return Lean code only.\n\
		Do not duplicate declarations.\n\
	Do not leave theorem bodies empty after `:= by`.\n\
	Do not redefine any APIs listed in the Support API section below. You may define NEW helpers not listed there.\n\
	If a proof is incomplete, use `sorry` inside the proof body.\n\
	Prefer a smaller explicit model that compiles over a larger broken one.\n\n\
IMPORTANT: The Support API section below lists definitions that are ALREADY IMPORTED from the support modules.\n\
You MUST use these existing definitions. DO NOT redefine any function, type, or lemma listed in the Support API.\n\
If you need a definition not in the Support API, you may define it yourself.\n\n\
You MUST import the following support modules and use their definitions:\n{support_modules}\n\
Write 'open Leanstral.Solana' at the top of your file.\n\n\
## Source Code\n\n```rust\n{source}\n```\n\n\
## Proof Obligation\n\n{title}\n\n\
Category: {category}\n\
Theorem Shape: {theorem_shape}\n\
Relevant Instructions: {relevant_instructions}\n\n\
Evidence:\n{evidence}\n\n\
	## Supported Semantic Surface\n\n{surface}\n\n\
	## Support API (Already Imported - DO NOT Redefine)\n\n\
The following definitions are available after 'open Leanstral.Solana':\n\n\
```lean\n{support_api}\n```\n\n\
	## Context\n\n{hint}\n\n\
		## Output Requirements\n\
			1. Define the model types and executable transition functions first\n\
			2. Import the listed support modules and write `open Leanstral.Solana`; use that surface consistently\n\
			3. State the theorem only after the semantics are defined\n\
			4. Use only Lean 4.15 / Mathlib 4.15 identifiers you are confident exist\n\
			5. Prefer concrete definitions over placeholders\n\
			6. Prove this one property only\n\
			7. Do not name a declaration exactly `initialize`; use names like `initializeTransition`, `exchangeTransition`, or `cancelTransition` instead\n\
			8. Do not define or use unqualified global aliases outside the `Leanstral.Solana` surface\n\
			9. Do not use tactic combinators such as `all_goals`, `try`, `repeat`, `first |`, or `admit`; prefer short direct proofs with `simp`, `cases`, `rcases`, `constructor`, and `exact`\n",
        source = source.trim(),
        title = obligation.title,
        category = obligation.category,
        theorem_shape = obligation.theorem_shape,
        relevant_instructions = obligation.relevant_instructions.join(", "),
        evidence = obligation
            .notes
            .iter()
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n"),
        support_modules = obligation
            .lean_support_modules
            .iter()
            .map(|module| format!("- import {module}"))
            .collect::<Vec<_>>()
            .join("\n"),
        surface = supported_surface
            .supported_property_categories
            .iter()
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n"),
        support_api = support_api,
        hint = match obligation.category.as_str() {
        "access_control" =>
		                "Model only the authorization condition that matters for this instruction. Import the relevant support modules, write `open Leanstral.Solana`, and use that surface consistently. Use one local program state structure, typically `EscrowState`, plus `Pubkey`; do not define extra local types like `AccountState`, `CancelPreState`, or helper state wrappers for this v1 access-control theorem. Define `cancelTransition : EscrowState -> Pubkey -> Option Unit` or an equally small transition. Define authorization as a direct `Prop` equality like `signer = preState.initializer`; do not define authorization as an existential over post-state reachability. In authorization predicates and theorem statements, use propositional equality `=` and never boolean equality `==`. Do not use `decide` for v1 access-control proofs. Do not mix propositional equality with boolean equality. In record updates, use Lean syntax `field := value`, never `field = value`. Prefer theorem statements of the exact form `cancelTransition preState signer ≠ none -> signer = preState.initializer` or an equivalent direct authorization predicate. When proving an `if`-based theorem, unfold the transition, split on the `if`, and use the equality hypothesis from the true branch directly with `exact` or `simpa`; do not use `rfl` unless both sides are definitionally equal. Avoid tactic combinators like `all_goals` and `try`.",
        "conservation" =>
		                "You MUST use 'trackedTotal' from Leanstral.Solana.Token - DO NOT redefine it.\nYou MUST use conservation lemmas from the support library: 'trackedTotal_map_id', 'transfer_preserves_total', etc.\nDO NOT prove your own versions of these lemmas.\n\n\
IMPORTANT: Here is how to use transfer_preserves_total correctly with all required arguments:\n\n\
The lemma signature is:\n\
  transfer_preserves_total (p_accounts : List Account) (p_from_authority p_to_authority : Pubkey) (p_amount : Nat) (p_h_distinct : p_from_authority ≠ p_to_authority)\n\n\
Example: If you need to prove conservation after transferring 100 tokens from authority A to authority B:\n\
```lean\n\
theorem example (p_accounts : List Account) (p_auth_from p_auth_to : Pubkey)\n    (h_distinct : p_auth_from ≠ p_auth_to) :\n    let post := p_accounts.map (fun acc =>\n      if acc.authority = p_auth_from then { acc with balance := acc.balance - 100 }\n      else if acc.authority = p_auth_to then { acc with balance := acc.balance + 100 }\n      else acc)\n    trackedTotal post = trackedTotal p_accounts := by\n  exact transfer_preserves_total p_accounts p_auth_from p_auth_to 100 h_distinct\n```\n\n\
For TWO transfers (like in escrow exchange), apply twice using intermediate steps:\n\
```lean\n\
theorem two_transfers (p_accounts : List Account) :\n    ... := by\n  have h1 := transfer_preserves_total p_accounts auth1 auth2 amount1 (by ...)\n  have h2 := transfer_preserves_total (updated_after_first) auth3 auth4 amount2 (by ...)\n  rw [<- h1, <- h2]\n```\n\n\
Model only the three or four tracked balances touched by this instruction. Import the relevant support modules, write `open Leanstral.Solana`, and use that surface consistently. Use the `trackedTotal` function and conservation lemmas from the support library: `trackedTotal_map_id` for balance-preserving updates, `transfer_preserves_total` for two-account transfers. Do not redefine `trackedTotal` or basic lemmas. Prefer a direct theorem over numeric balances and `trackedTotal`, not a large account-state machine. Do not invent helpers like `transfer`, `transferWithSigner`, `state.accounts`, seed lists, or signer arrays unless you define them in the file. Do not wrap the conservation theorem in an `EscrowState` record update unless the theorem truly depends on a record field update. Prefer a shape like: given pre-balances and nonnegativity/precondition inequalities, define post-balances directly and prove `trackedTotal [pre accounts] = trackedTotal [post accounts]`. Apply the support library lemmas to simplify the proof. In record updates, use Lean syntax `field := value`, never `field = value`. If subtraction over `Nat` makes the goal awkward, state enough preconditions and prove the equality with a small explicit arithmetic argument rather than relying on `omega` or `ring` blindly.",
        "state_machine" =>
		                "Model only the lifecycle flag or closed/open state that matters. Import the relevant support modules, write `open Leanstral.Solana`, and use that surface consistently. Use the `Lifecycle` type and lemmas from the support library: `closes_is_closed`, `closes_was_open`, `closed_irreversible`. Define one small local state structure, typically `EscrowState`, with a `lifecycle : Lifecycle` field. Do not define a custom local `AccountState` when the theorem is really about lifecycle. Prefer a direct theorem shape like `(cancelTransition st).lifecycle = Lifecycle.closed` or `closes st.lifecycle (cancelTransition st).lifecycle`. Apply the support library lemmas to simplify the proof. Do not write theorem statements using placeholders like `some _`; introduce any post-state explicitly if needed.",
        "arithmetic_safety" =>
	                "Model only the numeric parameters and bounds that matter for this obligation. Import the relevant support modules, write `open Leanstral.Solana`, and use that surface consistently. Avoid unrelated account semantics. Do not write theorem statements using placeholders like `some _`.",
        _ => "Keep the model small and explicit.",
        }
    )
}

fn support_api_for_modules(modules: &[String]) -> String {
    let mut lines = vec!["open Leanstral.Solana".to_string()];

    if modules.iter().any(|m| m == "Leanstral.Solana.Account") {
        lines.extend([
            "-- Account surface".to_string(),
            "Pubkey : Type".to_string(),
            "U64 : Type".to_string(),
            "U8 : Type".to_string(),
            "Account : Type".to_string(),
            "AccountState := Account".to_string(),
            "Account.key : Pubkey".to_string(),
            "Account.authority : Pubkey".to_string(),
            "Account.balance : Nat".to_string(),
            "Account.writable : Bool".to_string(),
            "canWrite : Pubkey -> Account -> Prop".to_string(),
            "findByKey : List Account -> Pubkey -> Option Account".to_string(),
            "findByAuthority : List Account -> Pubkey -> Option Account".to_string(),
            "-- Lemmas:".to_string(),
            "find_map_update_other : find by authority after updating different account is unchanged".to_string(),
            "find_map_update_same : find by authority after updating target account returns updated account".to_string(),
            "find_by_key_map_update_other : find by key after updating different account is unchanged".to_string(),
            "find_by_key_map_update_same : find by key after updating target account returns updated account".to_string(),
        ]);
    }

    if modules.iter().any(|m| m == "Leanstral.Solana.Authority") {
        lines.extend([
            "-- Authority surface".to_string(),
            "Authorized : Pubkey -> Pubkey -> Prop".to_string(),
        ]);
    }

    if modules.iter().any(|m| m == "Leanstral.Solana.Token") {
        lines.extend([
            "-- Token surface".to_string(),
            "TokenAccount := Account".to_string(),
            "Mint : Type".to_string(),
            "Program : Type".to_string(),
            "trackedTotal : List Account -> Nat".to_string(),
            "-- Lemmas:".to_string(),
            "trackedTotal_nil : trackedTotal [] = 0".to_string(),
            "trackedTotal_cons : cons preserves total".to_string(),
            "trackedTotal_append : append distributes over total".to_string(),
            "trackedTotal_map_id : mapping preserving balance preserves total".to_string(),
            "balance_update_preserves_total : zero-delta update preserves total".to_string(),
            "transfer_preserves_total : two-account transfer preserves total".to_string(),
        ]);
    }

    if modules.iter().any(|m| m == "Leanstral.Solana.State") {
        lines.extend([
            "-- State surface".to_string(),
            "Lifecycle : Type".to_string(),
            "closes : Lifecycle -> Lifecycle -> Prop".to_string(),
            "-- Lemmas:".to_string(),
            "closed_irreversible : closed cannot transition to open".to_string(),
            "closes_is_closed : closes implies result is closed".to_string(),
            "closes_was_open : closes implies original was open".to_string(),
        ]);
    }

    lines.join("\n")
}

fn account_field_to_ir(field: &AccountField) -> AccountFieldIr {
    match field {
        AccountField::Field(field) => AccountFieldIr {
            name: field.ident.to_string(),
            ty: ty_to_string(&field.ty),
            is_signer: field.constraints.is_signer() || matches!(field.ty, Ty::Signer),
            is_mutable: field.constraints.is_mutable(),
            constraints: constraints_to_ir(&field.constraints),
        },
        AccountField::CompositeField(field) => AccountFieldIr {
            name: field.ident.to_string(),
            ty: field.symbol.clone(),
            is_signer: field.constraints.is_signer(),
            is_mutable: field.constraints.is_mutable(),
            constraints: constraints_to_ir(&field.constraints),
        },
    }
}

fn constraints_to_ir(group: &ConstraintGroup) -> Vec<ConstraintIr> {
    let mut constraints = Vec::new();

    if group.is_signer() {
        constraints.push(ConstraintIr {
            kind: "signer".into(),
            raw: "signer".into(),
            target: None,
        });
    }
    if group.is_mutable() {
        constraints.push(ConstraintIr {
            kind: "mut".into(),
            raw: "mut".into(),
            target: None,
        });
    }
    for has_one in &group.has_one {
        constraints.push(ConstraintIr {
            kind: "has_one".into(),
            raw: has_one.join_target.to_token_stream().to_string(),
            target: Some(has_one.join_target.to_token_stream().to_string()),
        });
    }
    if let Some(close) = &group.close {
        constraints.push(ConstraintIr {
            kind: "close".into(),
            raw: close.sol_dest.to_token_stream().to_string(),
            target: Some(close.sol_dest.to_token_stream().to_string()),
        });
    }
    if let Some(owner) = &group.owner {
        constraints.push(ConstraintIr {
            kind: "owner".into(),
            raw: owner.owner_address.to_token_stream().to_string(),
            target: Some(owner.owner_address.to_token_stream().to_string()),
        });
    }
    if let Some(address) = &group.address {
        constraints.push(ConstraintIr {
            kind: "address".into(),
            raw: address.address.to_token_stream().to_string(),
            target: Some(address.address.to_token_stream().to_string()),
        });
    }
    if let Some(seeds) = &group.seeds {
        constraints.push(ConstraintIr {
            kind: "seeds".into(),
            raw: seeds.seeds.to_token_stream().to_string(),
            target: None,
        });
    }
    if let Some(token) = &group.token_account {
        constraints.push(ConstraintIr {
            kind: "token_account".into(),
            raw: format!(
                "mint={}, authority={}",
                token.mint.to_token_stream(),
                token.authority.to_token_stream()
            ),
            target: None,
        });
    }
    if let Some(associated) = &group.associated_token {
        constraints.push(ConstraintIr {
            kind: "associated_token".into(),
            raw: format!(
                "mint={}, wallet={}",
                associated.mint.to_token_stream(),
                associated.wallet.to_token_stream()
            ),
            target: None,
        });
    }
    if let Some(init) = &group.init {
        constraints.push(ConstraintIr {
            kind: "init".into(),
            raw: format!("init(if_needed={})", init.if_needed),
            target: None,
        });
    }

    constraints
}

fn ty_to_string(ty: &Ty) -> String {
    match ty {
        Ty::Signer => "Signer".into(),
        Ty::UncheckedAccount => "UncheckedAccount".into(),
        Ty::AccountInfo => "AccountInfo".into(),
        Ty::SystemAccount => "SystemAccount".into(),
        Ty::ProgramData => "ProgramData".into(),
        Ty::Account(account) => account.account_type_path.to_token_stream().to_string(),
        Ty::LazyAccount(account) => account.account_type_path.to_token_stream().to_string(),
        Ty::AccountLoader(account) => account.account_type_path.to_token_stream().to_string(),
        Ty::Program(program) => program.account_type_path.to_token_stream().to_string(),
        Ty::Interface(interface) => interface.account_type_path.to_token_stream().to_string(),
        Ty::InterfaceAccount(account) => account.account_type_path.to_token_stream().to_string(),
        Ty::Sysvar(sysvar) => format!("{sysvar:?}"),
    }
}

fn extract_seed_literals(body: &str) -> Vec<String> {
    Regex::new(r#"b"([^"]+)""#)
        .unwrap()
        .captures_iter(body)
        .filter_map(|capture| capture.get(1).map(|m| m.as_str().to_string()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn extract_close_targets(body: &str) -> Vec<String> {
    Regex::new(r#"close\s*=\s*(\w+)"#)
        .unwrap()
        .captures_iter(body)
        .filter_map(|capture| capture.get(1).map(|m| m.as_str().to_string()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn extract_auth_signals(body: &str) -> Vec<String> {
    let mut signals = BTreeSet::new();
    for capture in Regex::new(r#"authority\s*:\s*ctx\s*\.\s*accounts\s*\.\s*(\w+)"#)
        .unwrap()
        .captures_iter(body)
    {
        if let Some(name) = capture.get(1) {
            signals.insert(format!("authority: {}", name.as_str()));
        }
    }
    if body.contains("initializer") {
        signals.insert("mentions initializer".into());
    }
    signals.into_iter().collect()
}

fn extract_transfers(body: &str) -> Vec<TransferIr> {
    let mut transfers = Vec::new();
    let from_re = Regex::new(r#"from\s*:\s*ctx\s*\.\s*accounts\s*\.\s*(\w+)"#).unwrap();
    let to_re = Regex::new(r#"to\s*:\s*ctx\s*\.\s*accounts\s*\.\s*(\w+)"#).unwrap();
    let authority_re = Regex::new(r#"authority\s*:\s*ctx\s*\.\s*accounts\s*\.\s*(\w+)"#).unwrap();
    let amount_re = Regex::new(r#"token\s*::\s*transfer\s*\(\s*[^,]+,\s*([A-Za-z0-9_\.]+)\s*\)"#).unwrap();

    let blocks: Vec<&str> = body.split("let cpi_accounts = Transfer").collect();
    for block in blocks.into_iter().skip(1) {
        transfers.push(TransferIr {
            from: from_re
                .captures(block)
                .and_then(|m| m.get(1).map(|v| v.as_str().to_string())),
            to: to_re
                .captures(block)
                .and_then(|m| m.get(1).map(|v| v.as_str().to_string())),
            authority: authority_re
                .captures(block)
                .and_then(|m| m.get(1).map(|v| v.as_str().to_string())),
            amount_expr: amount_re
                .captures(block)
                .and_then(|m| m.get(1).map(|v| v.as_str().to_string())),
            uses_pda_signer: block.contains("new_with_signer"),
        });
    }

    transfers
}

fn infer_properties_from_text(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut properties = Vec::new();

    if lower.contains("unauthorized")
        || lower.contains("permission")
        || lower.contains("only")
        || lower.contains("fail")
        || lower.contains("reject")
    {
        properties.push("access_control".into());
    }
    if lower.contains("balance") || lower.contains("transfer") || lower.contains("token") {
        properties.push("conservation".into());
    }
    if lower.contains("close") || lower.contains("reuse") || lower.contains("closed") {
        properties.push("state_machine".into());
    }
    if lower.contains("overflow") || lower.contains("underflow") || lower.contains("u64") {
        properties.push("arithmetic_safety".into());
    }

    properties
}

fn has_attr(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().any(|attr| attr.path.is_ident(name))
}

fn has_derive_accounts(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path.is_ident("derive") {
            return false;
        }
        attr.to_token_stream().to_string().contains("Accounts")
    })
}
