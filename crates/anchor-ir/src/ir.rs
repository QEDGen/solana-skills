use serde::{Serialize, Deserialize};

#[derive(Debug, Serialize)]
pub struct AnalysisIr {
    pub source_file: String,
    pub idl_file: Option<String>,
    pub test_files: Vec<String>,
    pub instructions: Vec<InstructionIr>,
    pub accounts: Vec<AccountsStructIr>,
    pub test_signals: Vec<TestSignalIr>,
    pub property_candidates: Vec<PropertyCandidateIr>,
    pub proof_plan: ProofPlanIr,
}

#[derive(Debug, Serialize, Clone)]
pub struct InstructionIr {
    pub name: String,
    pub context_type: String,
    pub args: Vec<String>,
    pub pda_seeds: Vec<String>,
    pub closes_accounts: Vec<String>,
    pub auth_signals: Vec<String>,
    pub transfers: Vec<TransferIr>,
    pub evidence_sources: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct TransferIr {
    pub from: Option<String>,
    pub to: Option<String>,
    pub authority: Option<String>,
    pub amount_expr: Option<String>,
    pub uses_pda_signer: bool,
}

#[derive(Debug, Serialize)]
pub struct AccountsStructIr {
    pub name: String,
    pub fields: Vec<AccountFieldIr>,
    pub evidence_sources: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct AccountFieldIr {
    pub name: String,
    pub ty: String,
    pub is_signer: bool,
    pub is_mutable: bool,
    pub constraints: Vec<ConstraintIr>,
}

#[derive(Debug, Serialize)]
pub struct ConstraintIr {
    pub kind: String,
    pub raw: String,
    pub target: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TestSignalIr {
    pub file: String,
    pub name: String,
    pub inferred_properties: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct PropertyCandidateIr {
    pub id: String,
    pub category: String,
    pub title: String,
    pub confidence: String,
    pub relevant_instructions: Vec<String>,
    pub evidence: Vec<String>,
    pub prompt_hint: String,
}

#[derive(Debug, Serialize)]
pub struct ProofPlanIr {
    pub supported_surface: SupportedSurfaceIr,
    pub obligations: Vec<ProofObligationIr>,
    pub coverage: CoverageSummaryIr,
}

#[derive(Debug, Serialize)]
pub struct SupportedSurfaceIr {
    pub lean_support_modules: Vec<String>,
    pub supported_property_categories: Vec<String>,
    pub unsupported_reasons: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ProofObligationIr {
    pub id: String,
    pub title: String,
    pub category: String,
    pub relevant_instructions: Vec<String>,
    pub lean_support_modules: Vec<String>,
    pub theorem_shape: String,
    pub theorem_skeleton: String,
    pub status: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct CoverageSummaryIr {
    pub total_obligations: usize,
    pub supported_obligations: usize,
    pub unsupported_obligations: usize,
}

// LLM-assisted analysis protocol

#[derive(Debug, Serialize)]
pub struct LlmQuery {
    pub id: String,
    pub query_type: String,
    pub instruction: String,
    pub category: String,
    pub transfers: Vec<TransferIr>,
    pub rust_code_snippet: String,
    pub question: String,
}

#[derive(Debug, Serialize)]
pub struct LlmQuerySet {
    pub version: String,
    pub queries: Vec<LlmQuery>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LlmResponse {
    pub query_id: String,
    pub parameters: Vec<LlmParameter>,
    pub distinctness_constraints: Vec<String>,
    pub theorem_signature: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LlmParameter {
    pub name: String,
    pub param_type: String,
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct LlmResponseSet {
    pub version: String,
    pub responses: Vec<LlmResponse>,
}
