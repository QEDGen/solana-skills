use anyhow::{Context, Result};
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

/// Check whether `needle` appears in `haystack` as a whole word (not as a substring
/// of a longer identifier). Word boundaries are: start/end of string, or any character
/// that is not alphanumeric or underscore.
fn contains_word(haystack: &str, needle: &str) -> bool {
    for (i, _) in haystack.match_indices(needle) {
        let before_ok = i == 0 || {
            let b = haystack.as_bytes()[i - 1];
            !b.is_ascii_alphanumeric() && b != b'_'
        };
        let after = i + needle.len();
        let after_ok = after >= haystack.len() || {
            let b = haystack.as_bytes()[after];
            !b.is_ascii_alphanumeric() && b != b'_'
        };
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

#[derive(Debug)]
pub struct PropertyStatus {
    pub name: String,
    pub status: Status,
    /// Human-readable intent description (from doc: clause or auto-generated)
    pub intent: Option<String>,
    /// Suggestion when property is not proven
    pub suggestion: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum Status {
    Proven,
    Sorry,
    Missing,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Proven => write!(f, "proven"),
            Status::Sorry => write!(f, "sorry"),
            Status::Missing => write!(f, "missing"),
        }
    }
}

/// A named account type with its own fields and optional lifecycle.
/// In single-account specs, there's one account matching the program name.
/// In multi-account specs, each `account` block produces one of these.
#[derive(Debug, Clone)]
pub struct ParsedAccountType {
    pub name: String,
    pub fields: Vec<(String, String)>,
    pub lifecycle: Vec<String>,
    /// Reference to a PDA name (if this account is PDA-derived)
    pub pda_ref: Option<String>,
}

/// Plain record type (no variants). Declared as `type T = { field : Type, ... }`.
/// Used as the value type of a `Map[N] T` field and for grouping account-level state.
#[derive(Debug, Clone)]
pub struct ParsedRecordType {
    pub name: String,
    pub fields: Vec<(String, String)>,
}

/// Sum type with named variants; used when the ADT carries real alternatives
/// (e.g. `type Account | Inactive | Active of { ... }`). Lean codegen emits
/// this as an `inductive` with a payload-carrying constructor referencing a
/// separate `structure` per variant that has fields.
#[derive(Debug, Clone)]
pub struct ParsedSumType {
    pub name: String,
    pub variants: Vec<ParsedVariant>,
}

#[derive(Debug, Clone)]
pub struct ParsedVariant {
    pub name: String,
    /// Empty for no-payload variants like `| Inactive`.
    pub fields: Vec<(String, String)>,
}

/// Parsed aborts_if clause: condition → error name.
#[derive(Debug, Clone)]
pub struct ParsedAbort {
    pub lean_expr: String,
    pub rust_expr: String,
    pub error_name: String,
}

/// Parsed requires clause: guard condition with optional abort error.
/// When `error_name` is Some, generates both a guard (positive form in transition)
/// and an abort theorem (negated form).
#[derive(Debug, Clone)]
pub struct ParsedRequires {
    pub lean_expr: String,
    pub rust_expr: String,
    pub error_name: Option<String>,
}

/// Parsed ensures clause: post-condition relating pre and post state.
/// In lean_expr, `old(state.x)` is rendered as `s.x` (pre-state) and
/// `state.x` as `s'.x` (post-state).
#[derive(Debug, Clone)]
pub struct ParsedEnsures {
    pub lean_expr: String,
    #[allow(dead_code)]
    pub rust_expr: String,
}

/// Parsed cover block (reachability).
#[derive(Debug, Clone)]
pub struct ParsedCover {
    pub name: String,
    pub traces: Vec<Vec<String>>,
    pub reachable: Vec<(String, Option<String>)>, // (op, when_lean_expr)
}

/// Parsed liveness block (leads-to).
#[derive(Debug, Clone)]
pub struct ParsedLiveness {
    pub name: String,
    pub from_state: String,
    pub leads_to_state: String,
    pub via_ops: Vec<String>,
    pub within_steps: Option<u64>,
}

/// Parsed environment block (external state).
#[derive(Debug, Clone)]
pub struct ParsedEnvironment {
    pub name: String,
    pub mutates: Vec<(String, String)>, // (field, type)
    pub constraints: Vec<String>,       // lean form
    pub constraints_rust: Vec<String>,  // rust form
}

/// Parsed operation from a qedspec block with its clauses.
///
/// Scaffolding: many fields are parsed out of the qedspec operation block
/// but consumed only by specific backends (kani/proptest/lean/codegen). We
/// keep them on the shared struct so downstream passes can reach them without
/// re-parsing. The struct-level `allow(dead_code)` covers fields that the
/// active binary feature set doesn't touch yet.
#[derive(Debug)]
#[allow(dead_code)]
pub struct ParsedOperation {
    pub name: String,
    pub doc: Option<String>,
    pub who: Option<String>,
    /// Which account type this operation targets (from `on` clause).
    /// None means the default (first/only) account.
    pub on_account: Option<String>,
    pub has_when: bool,
    pub pre_status: Option<String>,
    pub post_status: Option<String>,
    pub has_calls: bool,
    pub program_id: Option<String>,
    pub has_u64_fields: bool,
    pub has_takes: bool,
    pub has_guard: bool,
    pub guard_str: Option<String>,
    pub has_effect: bool,
    pub takes_params: Vec<(String, String)>,
    pub effects: Vec<(String, String, String)>,
    pub calls_accounts: Vec<(String, String)>,
    pub calls_discriminator: Option<String>,
    pub emits: Vec<String>,
    /// Abort conditions: (lean_expr, rust_expr, error_name)
    pub aborts_if: Vec<ParsedAbort>,
}

/// Parsed property from a qedspec block.
#[derive(Debug, Clone)]
pub struct ParsedProperty {
    pub name: String,
    pub expression: Option<String>,
    pub preserved_by: Vec<String>,
}

/// PDA seed declaration from a qedspec block.
#[derive(Debug)]
#[allow(dead_code)]
pub struct ParsedPda {
    pub name: String,
    pub seeds: Vec<String>,
}

/// Event declaration from a qedspec block.
#[derive(Debug)]
#[allow(dead_code)]
pub struct ParsedEvent {
    pub name: String,
    pub fields: Vec<(String, String)>,
}

/// Account entry within an operation's context: block.
#[derive(Debug)]
#[allow(dead_code)]
pub struct ParsedAccountEntry {
    pub name: String,
    pub account_type: String,
    pub inner_type: Option<String>,
    pub is_mut: bool,
    pub is_init: bool,
    pub is_init_if_needed: bool,
    pub payer: Option<String>,
    pub seeds_ref: Option<String>,
    pub has_bump: bool,
    pub close_target: Option<String>,
    pub has_one: Option<String>,
    pub token_mint: Option<String>,
    pub token_authority: Option<String>,
}

/// Per-operation account context.
#[derive(Debug)]
#[allow(dead_code)]
pub struct ParsedContext {
    pub operation: String,
    pub accounts: Vec<ParsedAccountEntry>,
}

// ============================================================================
// sBPF-specific structures
// ============================================================================

/// Known pubkey as 4-chunk U64 representation.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedPubkey {
    pub name: String,
    pub chunks: Vec<String>, // 4 U64 values as strings
}

/// A field in an input/instruction layout with byte offset.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedLayoutField {
    pub name: String,
    pub field_type: String,
    pub offset: i64,
    pub description: Option<String>,
}

/// An sBPF validation guard.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedGuard {
    pub name: String,
    pub doc: Option<String>,
    pub checks: Option<String>, // guard expression (constants resolved to values)
    pub checks_raw: Option<String>, // guard expression (original constant names preserved)
    pub error: String,          // error code name
    pub fuel: Option<u64>,      // sBPF: fuel steps needed for this guard
}

/// An sBPF property (memory safety, data flow, CPI correctness, etc).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedSbpfProperty {
    pub name: String,
    pub doc: Option<String>,
    pub kind: SbpfPropertyKind,
}

/// The different kinds of sBPF properties.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum SbpfPropertyKind {
    /// Memory safety — scope over guards or named list
    Scope { targets: Vec<String> },
    /// Data flow — a value derived from seeds or flowing through accounts
    Flow { target: String, kind: FlowKind },
    /// CPI correctness — a cross-program invocation with expected fields
    Cpi {
        program: String,
        instruction: String,
        fields: Vec<(String, String)>,
    },
    /// Happy path — after all guards pass, expect exit code
    HappyPath { exit_code: String },
    /// Generic (has expr + preserved_by, from state-machine properties)
    Generic,
}

/// Sub-kinds of data flow properties.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum FlowKind {
    FromSeeds(Vec<String>),
    Through(Vec<String>),
}

/// A single instruction handler in an sBPF program.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedInstruction {
    pub name: String,
    pub doc: Option<String>,
    pub discriminant: Option<String>,
    pub entry: Option<u64>,
    pub constants: Vec<(String, String)>,
    pub errors: Vec<ParsedErrorCode>,
    pub input_layout: Vec<ParsedLayoutField>,
    pub insn_layout: Vec<ParsedLayoutField>,
    pub guards: Vec<ParsedGuard>,
    pub properties: Vec<ParsedSbpfProperty>,
}

/// Error code with optional numeric value and description.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedErrorCode {
    pub name: String,
    pub value: Option<u64>,
    pub description: Option<String>,
}

// ============================================================================
// Unified handler types (v3 — target-agnostic)
// ============================================================================

/// A unified handler — replaces both ParsedOperation (Quasar) and
/// ParsedInstruction (sBPF). Represents any callable entry point with
/// guards, effects, accounts, and properties.
#[derive(Debug, Clone)]
pub struct ParsedHandler {
    pub name: String,
    pub doc: Option<String>,
    /// Who can invoke this handler (access control actor).
    pub who: Option<String>,
    /// Which account type this handler targets (multi-account specs).
    pub on_account: Option<String>,
    /// Pre-state lifecycle requirement.
    pub pre_status: Option<String>,
    /// Post-state lifecycle transition.
    pub post_status: Option<String>,
    /// Input parameters.
    pub takes_params: Vec<(String, String)>,
    /// Legacy guard expression (Lean form). Deprecated: use `requires` instead.
    pub guard_str: Option<String>,
    /// Legacy guard expression (Rust form). Deprecated: use `requires` instead.
    #[allow(dead_code)]
    pub guard_str_rust: Option<String>,
    /// Legacy abort conditions. Deprecated: use `requires ... else` instead.
    pub aborts_if: Vec<ParsedAbort>,
    /// Requires clauses: guard + optional abort. When error_name is Some,
    /// generates both transition guard and abort theorem.
    pub requires: Vec<ParsedRequires>,
    /// Post-conditions (ensures clauses). Uses s' for post-state, s for old().
    pub ensures: Vec<ParsedEnsures>,
    /// Frame condition: fields that may be modified. All others must stay unchanged.
    pub modifies: Option<Vec<String>>,
    /// Handler-level let bindings: (name, lean_expr, rust_expr).
    pub let_bindings: Vec<(String, String, String)>,
    /// All abort conditions are exhaustive — generates ↔ theorem instead of per-abort.
    pub aborts_total: bool,
    /// State effects: (field, op, value) where op is "set"|"add"|"sub".
    pub effects: Vec<(String, String, String)>,
    /// IDL-level account descriptors.
    pub accounts: Vec<ParsedHandlerAccount>,
    /// Token transfer intents.
    pub transfers: Vec<ParsedTransfer>,
    /// Events emitted.
    pub emits: Vec<String>,
    /// Per-handler invariant references (names of invariants this handler must preserve).
    pub invariants: Vec<String>,
    /// Per-handler properties (from inline property/invariant clauses).
    pub properties: Vec<String>,
    /// `call Interface.handler(name = expr, ...)` sites — CPI invocations
    /// resolved against a top-level `interface` block. Empty for handlers
    /// that don't CPI. Consumed by Rust codegen (slice 5) and the
    /// `[shape_only_cpi]` lint (slice 4).
    #[allow(dead_code)]
    pub calls: Vec<ParsedCall>,
}

/// A resolved `call Target.handler(...)` site inside a handler body. The
/// target is split into interface + handler name for easier lookup; args
/// carry both Lean and Rust renderings so backends can pick their form.
#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct ParsedCall {
    pub target_interface: String,
    pub target_handler: String,
    pub args: Vec<ParsedCallArg>,
}

#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct ParsedCallArg {
    pub name: String,
    pub lean_expr: String,
    pub rust_expr: String,
}

impl ParsedHandler {
    pub fn has_guard(&self) -> bool {
        self.guard_str.is_some() || !self.requires.is_empty()
    }
    pub fn has_effect(&self) -> bool {
        !self.effects.is_empty()
    }
    /// Whether this handler initiates a CPI. True if the handler has a
    /// `transfers { }` block (legacy sugar for Token.transfer) OR any
    /// `call Interface.handler(...)` site (v2.5 uniform CPI surface).
    pub fn has_calls(&self) -> bool {
        !self.transfers.is_empty() || !self.calls.is_empty()
    }
    pub fn has_when(&self) -> bool {
        self.pre_status.is_some()
    }
    #[allow(dead_code)]
    pub fn has_takes(&self) -> bool {
        !self.takes_params.is_empty()
    }
    /// Find the first signer account in this handler.
    pub fn signer_account(&self) -> Option<&ParsedHandlerAccount> {
        self.accounts.iter().find(|a| a.is_signer)
    }
    /// Check if any account has a token type.
    pub fn has_token_accounts(&self) -> bool {
        self.accounts
            .iter()
            .any(|a| a.account_type.as_deref() == Some("token"))
    }
    /// Check if any account has a token program.
    pub fn has_token_program(&self) -> bool {
        self.accounts
            .iter()
            .any(|a| a.is_program && a.account_type.as_deref() == Some("token"))
            || self
                .accounts
                .iter()
                .any(|a| a.name.contains("token_program"))
    }
    /// Check if any account has bumps (PDA seeds).
    pub fn has_bumps(&self) -> bool {
        self.accounts.iter().any(|a| a.pda_seeds.is_some())
    }
}

impl ParsedHandlerAccount {
    /// Infer the Quasar/Anchor field type string for codegen.
    pub fn quasar_field_type(&self) -> String {
        if self.is_signer {
            "Signer".to_string()
        } else if self.is_program {
            "Program<()>".to_string()
        } else if self.account_type.as_deref() == Some("token") {
            "Account<Token>".to_string()
        } else {
            "Account<()>".to_string()
        }
    }

    /// Generate the #[account(...)] attribute for codegen.
    pub fn quasar_account_attr(&self, handler: &ParsedHandler, state_name: &str) -> String {
        let mut parts = Vec::new();

        if self.is_writable {
            parts.push("mut".to_string());
        }

        // Infer init from lifecycle: handler creates the account
        let is_init = (handler.pre_status.as_deref() == Some("Uninitialized")
            || handler.pre_status.as_deref() == Some("Empty"))
            && !self.is_signer
            && self.pda_seeds.is_some();

        if is_init {
            parts.push("init".to_string());
            if let Some(signer) = handler.signer_account() {
                parts.push(format!("payer = {}", signer.name));
            }
        }

        if let Some(ref _seeds) = self.pda_seeds {
            let struct_name = if state_name.ends_with("Account") {
                state_name.to_string()
            } else {
                format!("{}Account", state_name)
            };
            parts.push(format!("seeds = {}::seeds({})", struct_name, self.name));
            parts.push("bump".to_string());
        }

        if let Some(ref auth) = self.authority {
            parts.push(format!("token::authority = {}", auth));
        }

        if parts.is_empty() {
            String::new()
        } else {
            format!("    #[account({})]\n", parts.join(", "))
        }
    }
}

/// An account descriptor within a handler's `accounts` block.
/// IDL-level: no framework-specific annotations.
#[derive(Debug, Clone)]
pub struct ParsedHandlerAccount {
    pub name: String,
    pub is_signer: bool,
    pub is_writable: bool,
    pub is_program: bool,
    /// PDA seeds if this account is program-derived.
    pub pda_seeds: Option<Vec<String>>,
    /// Account type constraint (e.g., "token").
    pub account_type: Option<String>,
    /// Authority constraint (e.g., "escrow").
    pub authority: Option<String>,
}

/// A token transfer intent within a handler's `transfers` block.
#[derive(Debug, Clone)]
pub struct ParsedTransfer {
    pub from: String,
    pub to: String,
    pub amount: Option<String>,
    pub authority: Option<String>,
}

/// Full parsed spec context.
#[derive(Debug, Default)]
pub struct ParsedSpec {
    /// Unified handlers (v3). Populated from handler/operation/instruction blocks.
    pub handlers: Vec<ParsedHandler>,

    // Legacy fields — populated by forward bridge for backward compat.
    #[allow(dead_code)]
    pub operations: Vec<ParsedOperation>,
    pub invariants: Vec<(String, String)>, // (name, description)
    pub properties: Vec<ParsedProperty>,
    #[allow(dead_code)]
    pub has_u64_fields: bool,
    #[allow(dead_code)]
    pub u64_field_names: Vec<String>,
    #[allow(dead_code)]
    pub program_id: Option<String>,
    #[allow(dead_code)]
    pub program_name: String,
    /// Flat list of all state fields (union across all account types).
    /// For single-account specs, this is the account's fields.
    /// For multi-account specs, this is the primary account's fields.
    #[allow(dead_code)]
    pub state_fields: Vec<(String, String)>,
    /// Flat lifecycle states (union across all account types for backward compat).
    #[allow(dead_code)]
    pub lifecycle_states: Vec<String>,
    #[allow(dead_code)]
    pub pdas: Vec<ParsedPda>,
    #[allow(dead_code)]
    pub events: Vec<ParsedEvent>,
    #[allow(dead_code)]
    pub error_codes: Vec<String>,
    #[allow(dead_code)]
    pub contexts: Vec<ParsedContext>,
    /// Named account types with per-account fields and lifecycle.
    /// Empty for single-account specs that use bare `state {}`.
    pub account_types: Vec<ParsedAccountType>,

    /// Plain record types declared with `type T = { ... }`.
    /// Used as value types of Map fields and for structured state entries.
    pub records: Vec<ParsedRecordType>,

    /// Sum types used as Map-value types (not as handler pre/post states).
    /// These are emitted as proper Lean `inductive` — with one `structure`
    /// per payload-carrying variant — rather than flattened into a single
    /// record with a discriminator field. `type Account | Inactive | Active
    /// of { ... }` referenced from `Map[N] Account` ends up here.
    pub sum_types: Vec<ParsedSumType>,

    /// Target mode: "assembly" (sBPF) or "quasar" (Rust).
    #[allow(dead_code)]
    pub target: Option<String>,

    // sBPF-specific fields
    /// Assembly source path (present means sBPF mode).
    #[allow(dead_code)]
    pub assembly_path: Option<String>,
    /// Known pubkeys as 4-chunk U64 representations.
    #[allow(dead_code)]
    pub pubkeys: Vec<ParsedPubkey>,
    /// Instruction handlers (sBPF mode).
    #[allow(dead_code)]
    pub instructions: Vec<ParsedInstruction>,
    /// Global error codes with values (sBPF mode).
    /// Populated when errors use `Name = value "desc"` syntax.
    #[allow(dead_code)]
    pub valued_errors: Vec<ParsedErrorCode>,
    /// Global named constants (`const NAME = VALUE`).
    #[allow(dead_code)]
    pub constants: Vec<(String, String)>,
    /// Type aliases: `type AccountIdx = Fin[MAX_ACCOUNTS]` etc.
    /// Stored as (alias_name, rendered_target). Target is `Fin[N]`, `Nat`,
    /// a record name, etc. — whatever `TypeRef` the source points at.
    pub type_aliases: Vec<(String, String)>,
    /// Cover blocks (reachability properties).
    #[allow(dead_code)]
    pub covers: Vec<ParsedCover>,
    /// Liveness properties (leads-to).
    #[allow(dead_code)]
    pub liveness_props: Vec<ParsedLiveness>,
    /// Environment blocks (external state).
    #[allow(dead_code)]
    pub environments: Vec<ParsedEnvironment>,

    /// Interface declarations — callee contracts for CPI. See
    /// docs/design/spec-composition.md §2. Tier-0 interfaces have no
    /// `requires`/`ensures` on their handlers; Tier-1/Tier-2 do.
    pub interfaces: Vec<ParsedInterface>,

    /// Names of `pragma <name> { ... }` blocks that appeared in the spec.
    /// Used for target inference (`sbpf` → assembly target) and for
    /// platform-scoped feature flags in backends.
    pub pragmas: Vec<String>,
}

impl ParsedSpec {
    /// True iff the spec declared `pragma <name> { ... }`. Consumer lands
    /// in the next commit (target-inference from pragma presence).
    #[allow(dead_code)]
    pub fn has_pragma(&self, name: &str) -> bool {
        self.pragmas.iter().any(|p| p == name)
    }
}

/// Callee contract: program ID + per-handler shape (and optional effects).
/// Downstream consumers (lint, codegen) land in later v2.5 slices, hence
/// `allow(dead_code)` on fields without readers yet.
#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct ParsedInterface {
    pub name: String,
    pub doc: Option<String>,
    pub program_id: Option<String>,
    pub upstream: Option<ParsedUpstream>,
    pub handlers: Vec<ParsedInterfaceHandler>,
}

/// Upstream version pin for a library interface — `binary_hash` is
/// authoritative; the rest is informational. `verified_with` lists only
/// backends that were actually run; `"lean"` appears only when the callee is
/// genuinely proven, not axiomatized.
#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct ParsedUpstream {
    pub package: Option<String>,
    pub version: Option<String>,
    pub source: Option<String>,
    pub binary_hash: Option<String>,
    pub idl_hash: Option<String>,
    pub verified_with: Vec<String>,
    pub verified_at: Option<String>,
}

/// One handler inside an interface block. The `requires`/`ensures` vectors
/// are empty for Tier-0 (shape-only) interfaces. Populated for Tier-1
/// (hand-authored) and Tier-2 (imported) interfaces.
#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct ParsedInterfaceHandler {
    pub name: String,
    pub doc: Option<String>,
    pub params: Vec<(String, String)>,
    pub discriminant: Option<String>,
    pub accounts: Vec<ParsedHandlerAccount>,
    pub requires: Vec<ParsedRequires>,
    pub ensures: Vec<ParsedEnsures>,
}

/// Check spec coverage: which properties have proofs, which have sorry, which are missing.
pub fn check(spec_path: &Path, proofs_dir: &Path) -> Result<Vec<PropertyStatus>> {
    let parsed = parse_spec_file(spec_path)?;

    // Generate expected properties with intent annotations
    let properties = generate_properties(&parsed);

    if properties.is_empty() {
        eprintln!("No properties found in {}", spec_path.display());
        return Ok(vec![]);
    }

    // Collect all .lean files in the proofs directory (recursively)
    let mut proof_content = String::new();
    collect_lean_files(proofs_dir, &mut proof_content)?;

    // For each property, determine status
    let results: Vec<PropertyStatus> = properties
        .into_iter()
        .map(|(name, intent, suggestion)| {
            let status = check_property_status(&name, &proof_content);
            let suggestion = if status != Status::Proven {
                suggestion
            } else {
                None
            };
            PropertyStatus {
                name,
                status,
                intent: Some(intent),
                suggestion,
            }
        })
        .collect();

    Ok(results)
}

/// Parse a spec from disk. Only .qedspec format is supported.
///
/// `path` may be either:
///   - a single `.qedspec` file (original behaviour), or
///   - a directory containing one or more `.qedspec` files. Every file in the
///     directory (recursively) must declare the same `spec Name`; their top
///     items are merged in alphabetically-sorted source-path order.
///
/// The multi-file form is convention-based: no new grammar, no `import`/
/// `module` keywords. A program's spec is simply spread across files that all
/// start with `spec <Name>`. Fragments live naturally under `handlers/`,
/// `properties/`, etc.
pub fn parse_spec_file(path: &Path) -> Result<ParsedSpec> {
    if path.is_dir() {
        return parse_spec_dir(path);
    }

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext != "qedspec" {
        anyhow::bail!(
            "Unsupported spec format: .{}. Only .qedspec files are supported.\n\
             Convert Lean specs to .qedspec format (see examples/).",
            ext
        );
    }

    let src =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let typed = crate::chumsky_parser::parse(&src).map_err(|errs| {
        let msg = errs
            .iter()
            .map(|e| format!("  {:?}", e))
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::anyhow!("parse error in {}:\n{}", path.display(), msg)
    })?;
    Ok(crate::chumsky_adapter::adapt(&typed))
}

/// Load every `.qedspec` file under `dir` (recursively), parse each, validate
/// they all declare the same `spec Name`, and merge their top items into a
/// single typed AST. Files are visited in alphabetically-sorted path order so
/// the resulting `ParsedSpec` — and every artifact downstream of it — is
/// deterministic.
fn parse_spec_dir(dir: &Path) -> Result<ParsedSpec> {
    let mut files = Vec::new();
    collect_qedspec_files(dir, &mut files)?;
    files.sort();

    anyhow::ensure!(
        !files.is_empty(),
        "no .qedspec files found under {}",
        dir.display()
    );

    let mut merged_name: Option<String> = None;
    let mut merged_items: Vec<crate::ast::Node<crate::ast::TopItem>> = Vec::new();

    for file in &files {
        let src =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let typed = crate::chumsky_parser::parse(&src).map_err(|errs| {
            let msg = errs
                .iter()
                .map(|e| format!("  {:?}", e))
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::anyhow!("parse error in {}:\n{}", file.display(), msg)
        })?;

        match &merged_name {
            None => merged_name = Some(typed.name.clone()),
            Some(existing) if existing != &typed.name => {
                anyhow::bail!(
                    "spec name mismatch in {}: declared `spec {}`, but a sibling \
                     file declares `spec {}`. Every .qedspec fragment in a \
                     multi-file spec directory must declare the same name.",
                    file.display(),
                    typed.name,
                    existing,
                );
            }
            _ => {}
        }

        merged_items.extend(typed.items);
    }

    let merged = crate::ast::Spec {
        name: merged_name.expect("non-empty files implies non-empty name"),
        items: merged_items,
    };
    Ok(crate::chumsky_adapter::adapt(&merged))
}

/// Read the source text of a spec path — single file or directory of
/// fragments — as one contiguous string, joining fragments in the same
/// sorted-path order the loader uses. Consumers that scan the raw text
/// (e.g. `spec_hash_for_handler`) must go through this helper so the hash
/// they compute is identical to what the proc-macro will compute at compile
/// time.
pub fn read_spec_source(path: &Path) -> Result<String> {
    if path.is_dir() {
        let mut files = Vec::new();
        collect_qedspec_files(path, &mut files)?;
        files.sort();
        let mut out = String::new();
        for f in &files {
            let src =
                std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?;
            out.push_str(&src);
            if !src.ends_with('\n') {
                out.push('\n');
            }
        }
        Ok(out)
    } else {
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
    }
}

/// Recursive collector for `.qedspec` files under a directory, depth-first.
/// Silently skips non-UTF8 paths (pathologically rare in a source tree).
fn collect_qedspec_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("reading dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry in {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("stat {}", path.display()))?;
        if file_type.is_dir() {
            collect_qedspec_files(&path, out)?;
        } else if file_type.is_file()
            && path.extension().and_then(|e| e.to_str()) == Some("qedspec")
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Generate the full list of expected properties with intent descriptions.
/// Returns (property_name, intent_description, optional_suggestion).
///
/// Uses the unified `spec.handlers` to work across all target types.
/// Also preserves legacy paths for CPI, invariants, and property preservation.
fn generate_properties(spec: &ParsedSpec) -> Vec<(String, String, Option<String>)> {
    let mut props = Vec::new();

    // ── Handler-level proof obligations (unified, works for all targets) ──

    for handler in &spec.handlers {
        // CPI correctness: handler has transfers → needs CPI proof
        if !handler.transfers.is_empty() {
            let intent = format!("{} transfers tokens — verify CPI correctness", handler.name);
            let suggestion = Some(
                "Prove CPI targets the correct program with correct accounts and discriminator."
                    .to_string(),
            );
            props.push((format!("{}.cpi_correct", handler.name), intent, suggestion));
        }

        // Per-handler properties (from sBPF instruction guards/properties)
        for prop_name in &handler.properties {
            let intent = format!("{}: {}", handler.name, prop_name);
            let suggestion =
                Some("Prove with wp_exec. See SKILL.md sBPF proof workflow.".to_string());
            props.push((
                format!("{}.{}", handler.name, prop_name),
                intent,
                suggestion,
            ));
        }

        // Per-handler invariant obligations
        for inv_name in &handler.invariants {
            let intent = format!("{} preserves invariant {}", handler.name, inv_name);
            let suggestion = Some(format!("unfold {} at h_inv ⊢; omega", inv_name));
            props.push((
                format!("{}.preserves_{}", handler.name, inv_name),
                intent,
                suggestion,
            ));
        }
    }

    // ── Top-level invariants ──

    for (name, desc) in &spec.invariants {
        let intent = format!("Invariant: {}", desc);
        let suggestion = Some(
            "This invariant stub is generated as `True` by the DSL. \
             For a meaningful conservation proof, define the predicate and prove it \
             is preserved by all operations."
                .to_string(),
        );
        props.push((name.clone(), intent, suggestion));
    }

    // ── Per-handler property preservation (state-machine properties) ──

    for prop in &spec.properties {
        for op_name in &prop.preserved_by {
            let intent = format!(
                "{} is preserved by {}. Prove by unfold/omega.",
                prop.name, op_name
            );
            let suggestion = Some(format!(
                "unfold {} {}Transition at h_inv h ⊢; split_ifs at h with h_eq; simp_all; omega",
                prop.name, op_name
            ));
            props.push((
                format!("{}_preserved_by_{}", prop.name, op_name),
                intent,
                suggestion,
            ));
        }
    }

    props
}

/// Check whether a property is proven, sorry, or missing in the proof content.
fn check_property_status(property_name: &str, proof_content: &str) -> Status {
    // The property name uses dots (e.g., "Initialize.rejects_wrong_data_len").
    // Proofs may use either dots (DSL-generated sorry stubs) or underscores
    // (proof namespace, e.g., "initialize_rejects_wrong_data_len").
    // Also handle «»-quoted names (e.g., «initialize».rejects_wrong_data_len).
    // For hand-written proofs, also try the bare name without prefix
    // (e.g., "init_rejects_wrong_data_len" or just "rejects_wrong_data_len").
    let leaf = property_name;
    let leaf_underscore = property_name.replace('.', "_");

    // Try dot form, underscore form, and «»-quoted form
    let escaped_dot = regex::escape(leaf);
    let escaped_under = regex::escape(&leaf_underscore);
    // For «»-quoted: initialize.access_control → «initialize»\.access_control
    let quoted = leaf.splitn(2, '.').collect::<Vec<_>>();
    let escaped_quoted = if quoted.len() == 2 {
        format!(
            r"«{}»\.{}",
            regex::escape(quoted[0]),
            regex::escape(quoted[1])
        )
    } else {
        escaped_dot.clone()
    };

    // Also try the bare property name without instruction prefix, but with word boundary
    // e.g., "Initialize.rejects_wrong_data_len" → match "theorem rejects_wrong_data_len"
    // This handles hand-written proofs that don't use namespace prefixes.
    // We also try a lowercase prefix match: "Initialize.X" → "init_X" or "initialize_X".
    let extra_patterns = if quoted.len() == 2 {
        let prefix = quoted[0].to_lowercase();
        let short_prefix = if prefix.len() > 4 {
            &prefix[..4]
        } else {
            &prefix
        };
        let bare = regex::escape(quoted[1]);
        let prefixed_short = format!("{}_{}", regex::escape(short_prefix), bare);
        let prefixed_full = format!("{}_{}", regex::escape(&prefix), bare);
        format!("{}|{}|{}", bare, prefixed_short, prefixed_full)
    } else {
        String::new()
    };

    let theorem_pattern = if extra_patterns.is_empty() {
        format!(
            r"theorem\s+(?:{}|{}|{})\b",
            escaped_dot, escaped_under, escaped_quoted
        )
    } else {
        format!(
            r"theorem\s+(?:{}|{}|{}|{})\b",
            escaped_dot, escaped_under, escaped_quoted, extra_patterns
        )
    };
    let theorem_re = Regex::new(&theorem_pattern).unwrap();

    let Some(m) = theorem_re.find(proof_content) else {
        return Status::Missing;
    };

    // Extract theorem body: from the match to the next top-level keyword
    let rest = &proof_content[m.start()..];
    static BODY_END_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\n(?:theorem|def|noncomputable def|namespace|end|section|#)").unwrap()
    });
    let body = match BODY_END_RE.find(&rest[1..]) {
        Some(end_match) => &rest[..end_match.start() + 1],
        None => rest, // last theorem in file
    };

    // Check for sorry or trivial placeholder in just this theorem's body
    if body.contains("sorry") || body.contains(":= trivial") {
        return Status::Sorry;
    }

    Status::Proven
}

/// Recursively collect all .lean file contents from a directory.
fn collect_lean_files(dir: &Path, out: &mut String) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_lean_files(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("lean") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                out.push_str(&content);
                out.push('\n');
            }
        }
    }
    Ok(())
}

// ============================================================================
// Unified drift detection (qedgen check --code --kani)
// ============================================================================

/// Severity of a completeness warning.
#[derive(Debug, PartialEq, Clone, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// A concrete counterexample showing how an operation breaks a property.
/// Structured as data so the agent can reason about it and present it clearly.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Counterexample {
    /// The property that breaks
    pub property: String,
    /// The handler that breaks it
    pub handler: String,
    /// Pre-state field values (boundary case where invariant barely holds)
    pub pre_state: Vec<(String, i64)>,
    /// The invariant expression evaluated on pre-state (e.g., "3 ≤ 3")
    pub pre_check: String,
    /// Effects applied (e.g., ["member_count -= 1"])
    pub effects: Vec<String>,
    /// Post-state field values
    pub post_state: Vec<(String, i64)>,
    /// The invariant expression evaluated on post-state (e.g., "3 ≤ 2")
    pub post_check: String,
    /// Whether the invariant holds after the operation
    pub invariant_holds: bool,
}

/// A structured fix option for a lint warning.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FixOption {
    /// Short label (e.g., "Add guard", "Strengthen property", "Add compensating effect")
    pub label: String,
    /// Explanation of why this fix works
    pub rationale: String,
    /// The concrete DSL code to add/change
    pub snippet: String,
}

/// A spec completeness finding — structured for agent consumption.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompletenessWarning {
    /// Rule identifier (e.g., "no_access_control", "unguarded_arithmetic")
    pub rule: String,
    pub severity: Severity,
    /// Priority: 1=security, 2=correctness, 3=completeness, 4=quality, 5=polish
    pub priority: u8,
    pub message: String,
    /// The operation or field this warning relates to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Concrete fix the agent can offer to apply
    pub fix: String,
    /// Example DSL snippet showing the fix
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,
    /// Structured counterexample (when applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterexample: Option<Counterexample>,
    /// Structured fix options (when applicable)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub fix_options: Vec<FixOption>,
}

/// Drift status for a generated code file.
#[derive(Debug, PartialEq)]
pub enum DriftStatus {
    InSync,
    NoHash,
    SpecChanged,
    Missing,
    Orphaned,
}

/// Result of checking a single generated file.
#[derive(Debug)]
pub struct DriftResult {
    pub file: String,
    pub status: DriftStatus,
    pub detail: Option<String>,
}

/// Drift status for a Kani harness.
#[derive(Debug, PartialEq)]
pub enum KaniDriftStatus {
    InSync,
    Missing,
    Orphaned,
    FileStale,
}

/// Result of checking a single Kani harness.
#[derive(Debug)]
pub struct KaniDriftResult {
    pub harness_name: String,
    pub status: KaniDriftStatus,
}

/// Full unified report.
pub struct UnifiedReport {
    pub completeness: Vec<CompletenessWarning>,
    pub code_drift: Option<Vec<DriftResult>>,
    pub kani_drift: Option<Vec<KaniDriftResult>>,
    pub lean_coverage: Vec<PropertyStatus>,
}

impl UnifiedReport {
    pub fn issue_count(&self) -> usize {
        let comp = self
            .completeness
            .iter()
            .filter(|w| w.severity == Severity::Warning)
            .count();
        let code = self.code_drift.as_ref().map_or(0, |v| {
            v.iter().filter(|d| d.status != DriftStatus::InSync).count()
        });
        let kani = self.kani_drift.as_ref().map_or(0, |v| {
            v.iter()
                .filter(|d| d.status != KaniDriftStatus::InSync)
                .count()
        });
        let lean = self
            .lean_coverage
            .iter()
            .filter(|r| r.status != Status::Proven)
            .count();
        comp + code + kani + lean
    }
}

fn fields_for_handler<'a>(spec: &'a ParsedSpec, handler: &ParsedHandler) -> &'a [(String, String)] {
    if let Some(account_name) = handler.on_account.as_deref() {
        if let Some(account) = spec
            .account_types
            .iter()
            .find(|acct| acct.name == account_name)
        {
            return &account.fields;
        }
    }
    &spec.state_fields
}

fn suggested_effect_lines(
    spec: &ParsedSpec,
    handler: &ParsedHandler,
    is_init_like: bool,
) -> Vec<String> {
    handler
        .takes_params
        .iter()
        .map(|(name, _)| name.as_str())
        .take(3)
        .map(|param| {
            let matching_field = fields_for_handler(spec, handler)
                .iter()
                .find(|(field, _)| field.contains(param) || param.contains(field.as_str()));
            if let Some((field, _)) = matching_field {
                if is_init_like {
                    format!("    {} = {}", field, param)
                } else {
                    format!("    {} += {}", field, param)
                }
            } else if is_init_like {
                format!("    <field> = {}", param)
            } else {
                format!("    <field> += {}", param)
            }
        })
        .collect()
}

fn reachable_lifecycle_states(spec: &ParsedSpec) -> std::collections::HashSet<String> {
    let mut reachable: std::collections::HashSet<String> = spec
        .account_types
        .iter()
        .filter_map(|acct| acct.lifecycle.first().cloned())
        .collect();
    // Always include the global initial state — account-level lifecycles
    // may start at a later state (e.g. "Active") while the true entry
    // state (e.g. "Uninitialized") is only declared globally.
    if let Some(initial) = spec.lifecycle_states.first() {
        reachable.insert(initial.clone());
    }

    let mut changed = true;
    while changed {
        changed = false;
        for op in &spec.handlers {
            let next_state = match op.post_status.as_ref() {
                Some(post) => post,
                None => continue,
            };
            let can_reach = match op.pre_status.as_ref() {
                Some(pre) => reachable.contains(pre),
                None => true,
            };
            if can_reach && reachable.insert(next_state.clone()) {
                changed = true;
            }
        }
    }

    reachable
}

/// Look up the declared type of a field, checking the handler's target account
/// first, then falling back to the global state_fields.
fn find_field_type(spec: &ParsedSpec, op: &ParsedHandler, field: &str) -> Option<String> {
    // Check the handler's target account type first
    if let Some(ref acct_name) = op.on_account {
        if let Some(acct) = spec.account_types.iter().find(|a| a.name == *acct_name) {
            if let Some((_, t)) = acct.fields.iter().find(|(n, _)| n == field) {
                return Some(t.clone());
            }
        }
    }
    // Fall back to global state fields
    spec.state_fields
        .iter()
        .find(|(n, _)| n == field)
        .map(|(_, t)| t.clone())
}

/// Detect the comparison operator and LHS/RHS in a property expression.
/// Returns (lhs_field, operator, rhs_ref) where rhs_ref is either a field name
/// or "__const" for constant comparisons (e.g., `s.V ≤ 10000`).
fn parse_property_relation<'a>(
    expr: &'a str,
    prop_fields: &[&'a str],
) -> Option<(&'a str, &'a str, &'a str)> {
    // Look for common relational operators in the Lean-form expression
    for op in &[" ≤ ", " ≥ ", " < ", " > ", " = "] {
        if let Some(pos) = expr.find(op) {
            let lhs = &expr[..pos];
            let rhs = &expr[pos + op.len()..];
            // Find which prop field is on each side
            let lhs_field = prop_fields
                .iter()
                .find(|f| lhs.contains(&format!("s.{}", f)));
            let rhs_field = prop_fields
                .iter()
                .find(|f| rhs.contains(&format!("s.{}", f)));
            match (lhs_field, rhs_field) {
                (Some(lf), Some(rf)) => return Some((lf, op.trim(), rf)),
                // Single field vs constant (e.g., s.V ≤ 10000000)
                (Some(lf), None) => return Some((lf, op.trim(), "__const")),
                (None, Some(rf)) => return Some(("__const", op.trim(), rf)),
                _ => {}
            }
        }
    }
    None
}

/// Build a structured counterexample showing why a handler breaks a property.
fn build_counterexample(
    expr: &str,
    prop_name: &str,
    prop_fields: &[&str],
    op: &ParsedHandler,
    modified_fields: &[&str],
) -> Option<Counterexample> {
    let relation = parse_property_relation(expr, prop_fields);

    // Collect effects on modified fields
    let effect_triples: Vec<(&str, &str, &str)> = op
        .effects
        .iter()
        .filter(|(f, _, _)| modified_fields.contains(&f.as_str()))
        .map(|(f, k, v)| (f.as_str(), k.as_str(), v.as_str()))
        .collect();

    if effect_triples.is_empty() {
        return None;
    }

    let (lhs, op_sym, rhs) = relation?;

    // Build a boundary pre-state where the invariant barely holds
    let (lhs_val, rhs_val): (i64, i64) = match op_sym {
        "≤" | "<=" => (3, 3),
        "≥" | ">=" => (3, 3),
        "<" => (2, 3),
        ">" => (3, 2),
        _ => (3, 3),
    };

    let mut pre_state = Vec::new();
    if lhs != "__const" {
        pre_state.push((lhs.to_string(), lhs_val));
    }
    if rhs != "__const" {
        pre_state.push((rhs.to_string(), rhs_val));
    }

    let pre_check = format!("{} {} {}", lhs_val, op_sym, rhs_val);

    // Apply each effect
    let mut post_lhs = lhs_val;
    let mut post_rhs = rhs_val;
    let mut effects = Vec::new();
    for (field, kind, value) in &effect_triples {
        let v: i64 = value.parse().unwrap_or(1);
        let desc = match *kind {
            "add" => format!("{} += {}", field, value),
            "sub" => format!("{} -= {}", field, value),
            "set" => format!("{} = {}", field, value),
            _ => continue,
        };
        effects.push(desc);
        if *field == lhs {
            match *kind {
                "add" => post_lhs += v,
                "sub" => post_lhs -= v,
                "set" => post_lhs = v,
                _ => {}
            }
        }
        if *field == rhs {
            match *kind {
                "add" => post_rhs += v,
                "sub" => post_rhs -= v,
                "set" => post_rhs = v,
                _ => {}
            }
        }
    }

    let mut post_state = Vec::new();
    if lhs != "__const" {
        post_state.push((lhs.to_string(), post_lhs));
    }
    if rhs != "__const" {
        post_state.push((rhs.to_string(), post_rhs));
    }

    let holds = match op_sym {
        "≤" | "<=" => post_lhs <= post_rhs,
        "≥" | ">=" => post_lhs >= post_rhs,
        "<" => post_lhs < post_rhs,
        ">" => post_lhs > post_rhs,
        _ => false,
    };

    let post_check = format!("{} {} {}", post_lhs, op_sym, post_rhs);

    Some(Counterexample {
        property: prop_name.to_string(),
        handler: op.name.clone(),
        pre_state,
        pre_check,
        effects,
        post_state,
        post_check,
        invariant_holds: holds,
    })
}

/// Build structured fix suggestions for a property preservation conflict.
fn build_fix_suggestions(
    expr: &str,
    prop_name: &str,
    op: &ParsedHandler,
    prop_fields: &[&str],
    modified_fields: &[&str],
) -> Vec<FixOption> {
    let relation = parse_property_relation(expr, prop_fields);
    let unmodified: Vec<&&str> = prop_fields
        .iter()
        .filter(|f| !modified_fields.contains(f))
        .collect();

    let mut fixes = Vec::new();

    // Fix A: add a guard that ensures the invariant holds after the effect
    if let Some((lhs, op_sym, rhs)) = relation {
        for (field, kind, _value) in &op.effects {
            if !modified_fields.contains(&field.as_str()) {
                continue;
            }
            if kind == "sub" {
                if field.as_str() == rhs && (op_sym == "≤" || op_sym == "<=") {
                    fixes.push(FixOption {
                        label: "Add guard".to_string(),
                        rationale: format!(
                            "{} subtracts from {} (RHS of ≤). A strict inequality guard ensures the invariant survives.",
                            op.name, rhs
                        ),
                        snippet: format!(
                            "handler {}\n  requires state.{} < state.{}",
                            op.name, lhs, rhs
                        ),
                    });
                } else if field.as_str() == lhs && (op_sym == "≥" || op_sym == ">=") {
                    fixes.push(FixOption {
                        label: "Add guard".to_string(),
                        rationale: format!(
                            "{} subtracts from {} (LHS of ≥). A strict inequality guard ensures the invariant survives.",
                            op.name, lhs
                        ),
                        snippet: format!(
                            "handler {}\n  requires state.{} > state.{}",
                            op.name, lhs, rhs
                        ),
                    });
                }
            }
        }
    }

    // Fix B: add the handler to preserved_by
    fixes.push(FixOption {
        label: "Add to preserved_by".to_string(),
        rationale: format!(
            "Include '{}' in the property's preserved_by list. Requires a guard (option above) to make the proof go through.",
            op.name
        ),
        snippet: format!(
            "property {} {{\n  preserved_by [..., {}]\n}}",
            prop_name, op.name
        ),
    });

    // Fix C: add a compensating effect
    if let Some(unmod) = unmodified.first() {
        fixes.push(FixOption {
            label: "Add compensating effect".to_string(),
            rationale: format!(
                "Adjust '{}' alongside the modified field(s) to maintain the invariant.",
                unmod
            ),
            snippet: format!(
                "handler {}\n  effect {{ {} = <adjusted_value> }}",
                op.name, unmod
            ),
        });
    }

    fixes
}

/// Check spec completeness — heuristic rules for under-specification.
/// Returns structured warnings with fix suggestions for agent consumption.
pub fn check_completeness(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();

    // Find a likely signer field name from state (first Pubkey field)
    let signer_hint = spec
        .state_fields
        .iter()
        .find(|(_, t)| t == "Pubkey")
        .map(|(n, _)| n.as_str())
        .unwrap_or("authority");

    for op in &spec.handlers {
        // Rule 1: handler without who:
        if op.who.is_none() {
            warnings.push(CompletenessWarning {
                rule: "no_access_control".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!("handler '{}' has no `auth` — anyone can call it", op.name),
                subject: Some(op.name.clone()),
                fix: format!(
                    "Add `auth {}` to restrict who can execute this handler",
                    signer_hint
                ),
                example: Some(format!("  handler {}\n    auth {}", op.name, signer_hint)),
                counterexample: None,
                fix_options: vec![],
            });
        }

        // Rule 2: handler not covered by any property
        let covered = spec
            .properties
            .iter()
            .any(|p| p.preserved_by.contains(&op.name));
        if !covered && !spec.properties.is_empty() {
            let prop_names: Vec<&str> = spec.properties.iter().map(|p| p.name.as_str()).collect();
            warnings.push(CompletenessWarning {
                rule: "uncovered_operation".to_string(),
                severity: Severity::Info,
                priority: 3,
                message: format!(
                    "handler '{}' is not in any property's `preserved_by`",
                    op.name
                ),
                subject: Some(op.name.clone()),
                fix: format!(
                    "Add '{}' to an existing property's `preserved_by` list, or confirm it doesn't need property coverage",
                    op.name
                ),
                example: Some(format!(
                    "  property {} \"...\"\n    preserved_by: ..., {}",
                    prop_names.first().unwrap_or(&"my_property"),
                    op.name
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }

        // Rule 3: add effect without explicit overflow bound (type-aware).
        // Fires per-field: for each add effect, check whether any existing guard/requires
        // mentions both the field name and a bound (<=). Sub effects get auto-guarded
        // for underflow by codegen, so we only warn about add overflow here.
        {
            // Collect all guard text for substring matching
            let all_guards: String = {
                let mut g = op.guard_str.clone().unwrap_or_default();
                for req in &op.requires {
                    g.push(' ');
                    g.push_str(&req.lean_expr);
                }
                g
            };

            for (field, kind, val) in &op.effects {
                if kind != "add" {
                    continue;
                }
                // Check if any guard already bounds this field's addition.
                // Use contains_word on the val side to avoid "1" matching "10".
                let patterns = [
                    format!("state.{} + {}", field, val),
                    format!("{} + state.{}", val, field),
                    format!("s.{} + {}", field, val),
                    format!("{} + s.{}", val, field),
                ];
                let field_bounded = patterns.iter().any(|pat| contains_word(&all_guards, pat));
                if field_bounded {
                    continue;
                }

                let field_type = find_field_type(spec, op, field);
                let type_max = match field_type.as_deref() {
                    Some("U8") => "U8_MAX (255)",
                    Some("U16") => "U16_MAX (65535)",
                    Some("U32") => "U32_MAX",
                    Some("U128") => "U128_MAX",
                    _ => "U64_MAX",
                };
                let type_label = field_type.as_deref().unwrap_or("U64");
                warnings.push(CompletenessWarning {
                    rule: "unguarded_arithmetic".to_string(),
                    severity: Severity::Info,
                    priority: 2,
                    message: format!(
                        "handler '{}' adds to {} field '{}' without an explicit bound — codegen auto-inserts a {} guard, but an explicit `requires` with a tighter domain bound produces stronger proofs",
                        op.name, type_label, field, type_label
                    ),
                    subject: Some(op.name.clone()),
                    fix: format!(
                        "Add `requires state.{} + {} <= MY_BOUND` for a tighter bound than {} max",
                        field, val, type_label
                    ),
                    example: Some(format!(
                        "  handler {}\n    requires state.{} + {} <= {}",
                        op.name, field, val, type_max
                    )),
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }

        // Rule 6: handler has no when/then lifecycle
        if op.pre_status.is_none() && op.post_status.is_none() {
            warnings.push(CompletenessWarning {
                rule: "no_lifecycle".to_string(),
                severity: Severity::Info,
                priority: 2,
                message: format!(
                    "handler '{}' has no `when`/`then` — no state machine enforcement",
                    op.name
                ),
                subject: Some(op.name.clone()),
                fix: "Add `when` and `then` clauses to enforce handler ordering".to_string(),
                example: Some(format!(
                    "  handler {}\n    when Active\n    then Active",
                    op.name
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 4: state fields never modified (excluding Pubkey)
    for (fname, ftype) in &spec.state_fields {
        if ftype == "Pubkey" {
            continue;
        }
        let modified = spec
            .handlers
            .iter()
            .any(|op| op.effects.iter().any(|(f, _, _)| f == fname));
        if !modified {
            let mutating_ops: Vec<&str> = spec
                .handlers
                .iter()
                .filter(|op| op.has_effect())
                .map(|op| op.name.as_str())
                .collect();
            let op_hint = mutating_ops.first().copied().unwrap_or("some_handler");
            warnings.push(CompletenessWarning {
                rule: "unused_field".to_string(),
                severity: Severity::Info,
                priority: 4,
                message: format!("state field '{}' is never modified by any effect", fname),
                subject: Some(fname.clone()),
                fix: format!(
                    "Add an `effect: {} set <value>` or `effect: {} add <value>` to an operation, or remove the field if it's not needed",
                    fname, fname
                ),
                example: Some(format!(
                    "  operation {}\n    effect: {} set new_value",
                    op_hint, fname
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 5: property references nonexistent handler
    let op_names: Vec<&str> = spec.handlers.iter().map(|o| o.name.as_str()).collect();
    for prop in &spec.properties {
        for op_name in &prop.preserved_by {
            if !op_names.contains(&op_name.as_str()) {
                warnings.push(CompletenessWarning {
                    rule: "dangling_preserved_by".to_string(),
                    severity: Severity::Warning,
                    priority: 1,
                    message: format!(
                        "property '{}' references nonexistent handler '{}'",
                        prop.name, op_name
                    ),
                    subject: Some(format!("{}.preserved_by.{}", prop.name, op_name)),
                    fix: format!(
                        "Check the spelling of '{}' — available handlers: {}",
                        op_name,
                        op_names.join(", ")
                    ),
                    example: None,
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }

    // Rule 7: takes params (U64) with no guard — suggest input validation
    for op in &spec.handlers {
        if op.has_guard() {
            continue;
        }
        // Skip if rule 3 (unguarded_arithmetic) already fired for this op
        let already_flagged = warnings
            .iter()
            .any(|w| w.rule == "unguarded_arithmetic" && w.subject.as_deref() == Some(&op.name));
        if already_flagged {
            continue;
        }
        let u64_params: Vec<&str> = op
            .takes_params
            .iter()
            .filter(|(_, t)| t == "U64")
            .map(|(n, _)| n.as_str())
            .collect();
        if !u64_params.is_empty() {
            let guard_parts: Vec<String> =
                u64_params.iter().map(|p| format!("{} > 0", p)).collect();
            let guard_expr = guard_parts.join(" and ");
            warnings.push(CompletenessWarning {
                rule: "missing_guard_from_takes".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!(
                    "handler '{}' takes U64 params but has no guard — no input validation",
                    op.name
                ),
                subject: Some(op.name.clone()),
                fix: "Add input validation for takes parameters".to_string(),
                example: Some(format!("  handler {}\n    guard {}", op.name, guard_expr)),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 8: takes params + lifecycle transition but no effect
    for op in &spec.handlers {
        if op.has_effect() {
            continue;
        }
        let has_lifecycle = op.pre_status.is_some() || op.post_status.is_some();
        let is_init_like = op.name.contains("init") || op.name.contains("create");
        if !op.takes_params.is_empty() && (has_lifecycle || is_init_like) {
            let effect_lines = suggested_effect_lines(spec, op, is_init_like);
            warnings.push(CompletenessWarning {
                rule: "missing_effect".to_string(),
                severity: Severity::Warning,
                priority: 2,
                message: format!(
                    "handler '{}' takes params and transitions state but has no effect",
                    op.name
                ),
                subject: Some(op.name.clone()),
                fix: "Add an effect block to describe state changes".to_string(),
                example: Some(format!(
                    "  handler {}\n  effect {{\n{}\n  }}",
                    op.name,
                    effect_lines.join("\n")
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 9: handlers with effects but zero properties
    let has_effects = spec.handlers.iter().any(|op| op.has_effect());
    if has_effects && spec.properties.is_empty() && spec.invariants.is_empty() {
        // Suggest conservation if paired add/sub exist on same field
        let mut modified_fields: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for op in &spec.handlers {
            for (field, kind, _) in &op.effects {
                modified_fields
                    .entry(field.as_str())
                    .or_default()
                    .push(kind.as_str());
            }
        }
        let conservation_candidates: Vec<&str> = modified_fields
            .iter()
            .filter(|(_, kinds)| kinds.contains(&"add") && kinds.contains(&"sub"))
            .map(|(f, _)| *f)
            .collect();

        let op_list: Vec<&str> = spec
            .handlers
            .iter()
            .filter(|op| op.has_effect())
            .map(|op| op.name.as_str())
            .collect();
        let preserved_by = if op_list.len() <= 4 {
            format!("[{}]", op_list.join(", "))
        } else {
            "all".to_string()
        };

        let example = if !conservation_candidates.is_empty() {
            let field = conservation_candidates[0];
            format!(
                "  property conservation {{\n    expr state.{} >= 0\n    preserved_by {}\n  }}",
                field, preserved_by
            )
        } else {
            format!(
                "  property my_invariant {{\n    expr <your invariant expression>\n    preserved_by {}\n  }}",
                preserved_by
            )
        };

        warnings.push(CompletenessWarning {
            rule: "no_properties".to_string(),
            severity: Severity::Warning,
            priority: 3,
            message: "spec has effects but no properties — verification has nothing to prove"
                .to_string(),
            subject: None,
            fix: "Add at least one property to define what the verification should prove"
                .to_string(),
            example: Some(example),
            counterexample: None,
            fix_options: vec![],
        });
    }

    // Rule 10: handler has token program in accounts but no transfers
    for handler in &spec.handlers {
        if !handler.has_token_program() {
            continue;
        }
        if !handler.has_calls() {
            let writable_tokens: Vec<&str> = handler
                .accounts
                .iter()
                .filter(|a| {
                    a.is_writable && a.account_type.as_deref() == Some("token") && !a.is_program
                })
                .map(|a| a.name.as_str())
                .collect();
            let signer_name = handler
                .signer_account()
                .map(|a| a.name.as_str())
                .unwrap_or("authority");
            let accounts_str = if writable_tokens.len() >= 2 {
                format!(
                    "from {} to {} authority {}",
                    writable_tokens[0], writable_tokens[1], signer_name
                )
            } else if writable_tokens.len() == 1 {
                format!(
                    "from {} to dest authority {}",
                    writable_tokens[0], signer_name
                )
            } else {
                format!("from source to dest authority {}", signer_name)
            };
            warnings.push(CompletenessWarning {
                rule: "missing_cpi_for_token_context".to_string(),
                severity: Severity::Warning,
                priority: 2,
                message: format!(
                    "handler '{}' has token_program in accounts but no `transfers` block",
                    handler.name
                ),
                subject: Some(handler.name.clone()),
                fix: "Add a `transfers` block to specify token movements".to_string(),
                example: Some(format!(
                    "  handler {}\n    transfers {{\n      {} amount <expr>\n    }}",
                    handler.name, accounts_str
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 11: no errors block but handlers have guards
    let any_guards = spec.handlers.iter().any(|op| op.has_guard());
    if any_guards && spec.error_codes.is_empty() {
        warnings.push(CompletenessWarning {
            rule: "no_errors_block".to_string(),
            severity: Severity::Info,
            priority: 4,
            message: "spec has guards but no `errors` block — codegen can't generate error types"
                .to_string(),
            subject: None,
            fix: "Add an errors block listing all failure modes".to_string(),
            example: Some("  errors [InvalidAmount, Unauthorized, AlreadyClosed]".to_string()),
            counterexample: None,
            fix_options: vec![],
        });
    }

    // Rule 12: lifecycle states unreachable by any operation transition
    if spec.lifecycle_states.len() > 1 {
        let reachable = reachable_lifecycle_states(spec);
        for state in &spec.lifecycle_states {
            if !reachable.contains(state) {
                warnings.push(CompletenessWarning {
                    rule: "lifecycle_unreachable_state".to_string(),
                    severity: Severity::Info,
                    priority: 2,
                    message: format!(
                        "lifecycle state '{}' cannot be reached from any initial state via operation transitions",
                        state
                    ),
                    subject: Some(state.clone()),
                    fix: format!(
                        "Add a `when: {}` or `then: {}` clause to an operation, or remove '{}' from the lifecycle",
                        state, state, state
                    ),
                    example: None,
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }

    // Rule 13: write_without_read — state field written in effects but never read in guards/properties
    {
        let mut written_fields = std::collections::HashSet::new();
        for op in &spec.handlers {
            for (field, _, _) in &op.effects {
                written_fields.insert(field.as_str());
            }
        }
        let mut read_fields = std::collections::HashSet::new();
        for op in &spec.handlers {
            if let Some(ref guard) = op.guard_str {
                for field in &written_fields {
                    if guard.contains(&format!("s.{}", field))
                        || guard.contains(&format!("state.{}", field))
                        || contains_word(guard, field)
                    {
                        read_fields.insert(*field);
                    }
                }
            }
        }
        for prop in &spec.properties {
            if let Some(ref expr) = prop.expression {
                for field in &written_fields {
                    if expr.contains(&format!("s.{}", field))
                        || expr.contains(&format!("state.{}", field))
                        || contains_word(expr, field)
                    {
                        read_fields.insert(*field);
                    }
                }
            }
        }
        for field in &written_fields {
            if !read_fields.contains(field) {
                warnings.push(CompletenessWarning {
                    rule: "write_without_read".to_string(),
                    severity: Severity::Info,
                    priority: 3,
                    message: format!(
                        "state field '{}' is written in effects but never referenced in any guard or property",
                        field
                    ),
                    subject: Some(field.to_string()),
                    fix: format!(
                        "Add '{}' to a property expression or guard, or verify that writing it without reading is intentional",
                        field
                    ),
                    example: Some(format!(
                        "  property my_invariant {{\n    expr state.{} >= 0\n    preserved_by all\n  }}",
                        field
                    )),
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }

    // Rule 14: dead_guard — a guard conjunct subsumed by another on the same operation
    {
        static CMP_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^(?:s\.|state\.)?(\w+)\s*(>=|<=|>|<|=)\s*(\d+)$").unwrap()
        });
        let cmp_re = &*CMP_RE;
        for op in &spec.handlers {
            if let Some(ref guard) = op.guard_str {
                // Split on ∧ and "and" to get individual conjuncts
                let conjuncts: Vec<&str> = guard
                    .split('\u{2227}')
                    .flat_map(|s| s.split(" and "))
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .collect();

                // Parse each conjunct into (field, op, value) triples
                let parsed: Vec<(usize, &str, &str, i64)> = conjuncts
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| {
                        cmp_re.captures(c).and_then(|caps| {
                            let field = caps.get(1)?.as_str();
                            let cmp = caps.get(2)?.as_str();
                            let val: i64 = caps.get(3)?.as_str().parse().ok()?;
                            Some((i, field, cmp, val))
                        })
                    })
                    .collect();

                // Check if any conjunct is subsumed by another
                for &(i, field_a, cmp_a, val_a) in &parsed {
                    for &(j, field_b, cmp_b, val_b) in &parsed {
                        if i == j || field_a != field_b {
                            continue;
                        }
                        // Check if conjunct j implies conjunct i (making i redundant)
                        let subsumed = match (cmp_a, cmp_b) {
                            (">=", ">=") => val_b >= val_a, // x >= 5 implies x >= 3
                            (">", ">") => val_b >= val_a,   // x > 5 implies x > 3
                            (">=", ">") => val_b >= val_a,  // x > 5 implies x >= 5
                            ("<=", "<=") => val_b <= val_a, // x <= 3 implies x <= 5
                            ("<", "<") => val_b <= val_a,
                            ("<=", "<") => val_b <= val_a,
                            _ => false,
                        };
                        if subsumed && i != j {
                            warnings.push(CompletenessWarning {
                                rule: "dead_guard".to_string(),
                                severity: Severity::Info,
                                priority: 4,
                                message: format!(
                                    "guard conjunct '{}' on operation '{}' is subsumed by '{}'",
                                    conjuncts[i], op.name, conjuncts[j]
                                ),
                                subject: Some(op.name.clone()),
                                fix: format!("Remove the redundant conjunct '{}'", conjuncts[i]),
                                example: None,
                                counterexample: None,
                                fix_options: vec![],
                            });
                            break; // Only report once per subsumed conjunct
                        }
                    }
                }
            }
        }
    }

    // Rule 15: circular_lifecycle_no_terminal — lifecycle where every state has outgoing transitions
    if spec.lifecycle_states.len() > 1 {
        let mut outgoing: std::collections::HashMap<&str, std::collections::HashSet<&str>> =
            std::collections::HashMap::new();
        for op in &spec.handlers {
            if let (Some(ref pre), Some(ref post)) = (&op.pre_status, &op.post_status) {
                if pre != post {
                    outgoing
                        .entry(pre.as_str())
                        .or_default()
                        .insert(post.as_str());
                }
            }
        }
        // A terminal state has no outgoing transitions to a different state
        let terminal_exists = spec
            .lifecycle_states
            .iter()
            .any(|s| !outgoing.contains_key(s.as_str()) || outgoing[s.as_str()].is_empty());
        if !terminal_exists {
            warnings.push(CompletenessWarning {
                rule: "circular_lifecycle_no_terminal".to_string(),
                severity: Severity::Info,
                priority: 3,
                message: "lifecycle has no terminal state — every state has outgoing transitions"
                    .to_string(),
                subject: None,
                fix: "Consider whether the cycle is intentional. If not, designate a terminal state by removing its outgoing transitions.".to_string(),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 16: excluded_op_modifies_property — handler NOT in preserved_by modifies fields
    // referenced by the property. The inductive theorem will need a manual proof (not sorry).
    for prop in &spec.properties {
        if let Some(ref expr) = prop.expression {
            // Extract field names from the property expression.
            // The expression is in Lean form (s.field_name) from the parser.
            let prop_fields: Vec<&str> = {
                let mut fields = Vec::new();
                // Check both "s." (Lean form) and "state." (DSL form) patterns
                for prefix in &["s.", "state."] {
                    for (i, _) in expr.match_indices(prefix) {
                        let rest = &expr[i + prefix.len()..];
                        let end = rest
                            .find(|c: char| !c.is_alphanumeric() && c != '_')
                            .unwrap_or(rest.len());
                        if end > 0 {
                            let field = &rest[..end];
                            if !fields.contains(&field) {
                                fields.push(field);
                            }
                        }
                    }
                }
                fields
            };

            let uses_all = prop.preserved_by.iter().any(|p| p == "all");
            if uses_all {
                continue; // all ops are in preserved_by, no exclusion
            }

            for op in &spec.handlers {
                if prop.preserved_by.contains(&op.name) {
                    continue; // this op IS covered
                }
                // Check if this excluded op modifies any field in the property expression
                let modified_prop_fields: Vec<&str> = op
                    .effects
                    .iter()
                    .filter(|(f, _, _)| prop_fields.contains(&f.as_str()))
                    .map(|(f, _, _)| f.as_str())
                    .collect();

                if !modified_prop_fields.is_empty() {
                    // Skip if ALL effects on property fields are monotonically safe.
                    // e.g., sub on LHS of ≤ can only decrease the LHS → invariant still holds.
                    if let Some((lhs, op_sym, _rhs)) = parse_property_relation(expr, &prop_fields) {
                        let all_safe = op
                            .effects
                            .iter()
                            .filter(|(f, _, _)| modified_prop_fields.contains(&f.as_str()))
                            .all(|(f, kind, _)| {
                                let on_lhs = f.as_str() == lhs;
                                match (kind.as_str(), op_sym, on_lhs) {
                                    ("sub", "≤", true) | ("sub", "<=", true) => true, // decreasing LHS of ≤
                                    ("add", "≥", true) | ("add", ">=", true) => true, // increasing LHS of ≥
                                    ("sub", "≥", false) | ("sub", ">=", false) => true, // decreasing RHS of ≥
                                    ("add", "≤", false) | ("add", "<=", false) => true, // increasing RHS of ≤
                                    _ => false,
                                }
                            });
                        if all_safe {
                            continue; // monotonically preserves the invariant
                        }
                    }

                    // Build structured counterexample and fix options for agent consumption.
                    let counterexample = build_counterexample(
                        expr,
                        &prop.name,
                        &prop_fields,
                        op,
                        &modified_prop_fields,
                    );

                    let fix_options = build_fix_suggestions(
                        expr,
                        &prop.name,
                        op,
                        &prop_fields,
                        &modified_prop_fields,
                    );

                    // Compose the human-readable fix string from the first fix option
                    let fix = fix_options.first().map_or_else(
                        || format!(
                            "Add '{}' to property '{}' `preserved_by` with a guard, or restructure the property",
                            op.name, prop.name
                        ),
                        |f| f.snippet.clone(),
                    );

                    warnings.push(CompletenessWarning {
                        rule: "excluded_op_modifies_property".to_string(),
                        severity: Severity::Warning,
                        priority: 2,
                        message: format!(
                            "handler '{}' modifies field(s) [{}] used in property '{}' but is excluded from `preserved_by` — the inductive theorem arm will emit `sorry`",
                            op.name,
                            modified_prop_fields.join(", "),
                            prop.name
                        ),
                        subject: Some(op.name.clone()),
                        fix,
                        example: None,
                        counterexample,
                        fix_options,
                    });
                }
            }
        }
    }

    // Validate new-DSL constructs: Map[N] T fields, subscripted effect LHS.
    warnings.extend(check_map_and_subscript(spec));

    // CPI tier lint: call sites whose target is Tier 0 (no ensures declared)
    // get flagged so users see the gap between "my Rust compiles" and "my
    // program is verified." See docs/design/spec-composition.md §2.
    warnings.extend(check_shape_only_cpi(spec));

    // Sort by priority (ascending), then by rule name for stability
    warnings.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.rule.cmp(&b.rule)));

    warnings
}

/// Emit `[shape_only_cpi]` info-level warnings for `call Interface.handler(...)`
/// sites whose target declares no `ensures`. The call still generates a real
/// Rust CPI builder; the lint simply makes the proof-side gap explicit so
/// nobody mistakes a compiling CPI for a verified one.
fn check_shape_only_cpi(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();

    for handler in &spec.handlers {
        for call in &handler.calls {
            let iface = spec
                .interfaces
                .iter()
                .find(|i| i.name == call.target_interface);
            let target_handler =
                iface.and_then(|i| i.handlers.iter().find(|h| h.name == call.target_handler));

            let (reason, fix) = match (iface, target_handler) {
                (None, _) => (
                    format!(
                        "interface `{}` is not declared in this spec — the call compiles but has no contract",
                        call.target_interface
                    ),
                    format!(
                        "Declare `interface {} {{ ... }}` at the top level, or `qedgen interface --idl <path>` to scaffold one.",
                        call.target_interface
                    ),
                ),
                (Some(_), None) => (
                    format!(
                        "interface `{}` has no handler named `{}` — check for a typo or add the handler",
                        call.target_interface, call.target_handler
                    ),
                    format!(
                        "Add `handler {}` inside `interface {} {{ ... }}`, or update the call site to match a real handler.",
                        call.target_handler, call.target_interface
                    ),
                ),
                (Some(_), Some(h)) if h.ensures.is_empty() => (
                    format!(
                        "`{}.{}` declares shape only (no `ensures`) — the call has no post-state assumptions for proofs",
                        call.target_interface, call.target_handler
                    ),
                    format!(
                        "Upgrade to Tier 1 by declaring `ensures` on `{}` inside `interface {}`, or import a qedspec for full verification.",
                        call.target_handler, call.target_interface
                    ),
                ),
                // Tier 1/2 target — nothing to lint.
                _ => continue,
            };

            warnings.push(CompletenessWarning {
                rule: "shape_only_cpi".to_string(),
                severity: Severity::Info,
                priority: 3,
                message: format!(
                    "handler '{}' calls `{}.{}` — {}",
                    handler.name, call.target_interface, call.target_handler, reason
                ),
                subject: Some(handler.name.clone()),
                fix,
                example: Some(format!(
                    "  interface {} {{\n    handler {} (...) {{\n      ensures /* what the callee guarantees */\n    }}\n  }}",
                    call.target_interface, call.target_handler
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    warnings
}

/// Parsed form of a field type string. Captures the distinction between a
/// plain type (e.g. `U128`, `Account`) and a bounded map (`Map[N] T`).
///
/// Only `Map { .. }` is inspected by the current consumer; `Simple` carries
/// the trimmed type string for future linting passes (e.g., primitive-type
/// checks, alias resolution) and intentionally remains exhaustive.
#[derive(Debug)]
enum FieldTypeShape<'a> {
    Simple(#[allow(dead_code)] &'a str),
    Map { bound: &'a str, inner: &'a str },
}

/// Parse a field-type source string into a structured view.
/// Returns `Simple` for `U128`, `Account`, `Vec U64` and `Map { ... }` for
/// `Map[CONST] T` (bound and inner trimmed).
fn classify_field_type(s: &str) -> FieldTypeShape<'_> {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("Map") {
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('[') {
            if let Some(close) = rest.find(']') {
                let bound = rest[..close].trim();
                let inner = rest[close + 1..].trim();
                return FieldTypeShape::Map { bound, inner };
            }
        }
    }
    FieldTypeShape::Simple(trimmed)
}

/// Validate `Map[N] T` field declarations and subscript usage.
///   - `N` must be a declared `const`
///   - `T` must be either a declared record or a well-known primitive
///   - Effect LHS of form `field[i].x` must reference a Map-typed state field
fn check_map_and_subscript(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    use std::collections::{HashMap, HashSet};

    let mut warnings = Vec::new();

    let const_names: HashSet<&str> = spec.constants.iter().map(|(n, _)| n.as_str()).collect();
    let record_names: HashSet<&str> = spec.records.iter().map(|r| r.name.as_str()).collect();

    // Collect Map-typed fields across all account types, keyed by field name.
    let mut map_fields: HashMap<&str, (&str, &str, &str)> = HashMap::new(); // field → (owner, bound, inner)

    for acct in &spec.account_types {
        for (fname, ftype) in &acct.fields {
            if let FieldTypeShape::Map { bound, inner } = classify_field_type(ftype) {
                // Rule: bound must be a declared const
                if !const_names.contains(bound) {
                    warnings.push(CompletenessWarning {
                        rule: "map_bound_not_const".to_string(),
                        severity: Severity::Error,
                        priority: 0,
                        message: format!(
                            "field '{}.{}' uses Map[{}] but '{}' is not declared as `const`",
                            acct.name, fname, bound, bound
                        ),
                        subject: Some(fname.clone()),
                        fix: format!("Add `const {} = <size>` at the top of the spec", bound),
                        example: Some(format!("  const {} = 1024", bound)),
                        counterexample: None,
                        fix_options: vec![],
                    });
                }

                // Rule: inner must be a record or a known primitive
                let is_known = record_names.contains(inner)
                    || matches!(
                        inner,
                        "Bool"
                            | "U8"
                            | "U16"
                            | "U32"
                            | "U64"
                            | "U128"
                            | "I8"
                            | "I16"
                            | "I32"
                            | "I64"
                            | "I128"
                            | "Pubkey"
                    );
                if !is_known {
                    warnings.push(CompletenessWarning {
                        rule: "map_value_unknown".to_string(),
                        severity: Severity::Error,
                        priority: 0,
                        message: format!(
                            "field '{}.{}' uses Map[{}] {} but '{}' is neither a declared record nor a primitive",
                            acct.name, fname, bound, inner, inner
                        ),
                        subject: Some(fname.clone()),
                        fix: format!("Declare `type {} = {{ ... }}`", inner),
                        example: Some(format!(
                            "  type {} = {{\n    active : Bool,\n    capital : U128,\n  }}",
                            inner
                        )),
                        counterexample: None,
                        fix_options: vec![],
                    });
                }

                map_fields.insert(fname.as_str(), (acct.name.as_str(), bound, inner));
            }
        }
    }

    // Effect LHS validation: any `name[i]...` must refer to a Map-typed field.
    for op in &spec.handlers {
        for (field, _, _) in &op.effects {
            if let Some(bracket) = field.find('[') {
                let root = &field[..bracket];
                if !map_fields.contains_key(root) {
                    warnings.push(CompletenessWarning {
                        rule: "subscript_not_map".to_string(),
                        severity: Severity::Error,
                        priority: 0,
                        message: format!(
                            "handler '{}' has effect `{}` but '{}' is not a Map-typed state field",
                            op.name, field, root
                        ),
                        subject: Some(op.name.clone()),
                        fix: format!(
                            "Declare `{} : Map[MAX_...] SomeRecord` in the state type, or remove the subscript",
                            root
                        ),
                        example: None,
                        counterexample: None,
                        fix_options: vec![],
                    });
                }
            }
        }
    }

    warnings
}

/// Run standalone lint — returns structured JSON for agent consumption.
pub fn lint(spec_path: &std::path::Path) -> Result<Vec<CompletenessWarning>> {
    let spec = parse_spec_file(spec_path)?;
    Ok(check_completeness(&spec))
}

// ============================================================================
// Coverage matrix (qedgen coverage)
// ============================================================================

/// A single cell in the operation × property coverage matrix.
#[derive(Debug, serde::Serialize)]
pub struct CoverageCell {
    pub operation: String,
    pub property: String,
    pub covered: bool,
}

/// The full coverage matrix: which operations are covered by which properties.
#[derive(Debug, serde::Serialize)]
pub struct CoverageMatrix {
    pub operations: Vec<String>,
    pub properties: Vec<String>,
    pub cells: Vec<CoverageCell>,
    pub gaps: Vec<String>,
    pub coverage_pct: f64,
}

/// Build a coverage matrix from a parsed spec.
pub fn coverage_matrix(spec: &ParsedSpec) -> CoverageMatrix {
    let op_names: Vec<String> = spec.handlers.iter().map(|o| o.name.clone()).collect();
    let prop_names: Vec<String> = spec
        .properties
        .iter()
        .filter(|p| p.expression.is_some())
        .map(|p| p.name.clone())
        .collect();

    let mut cells = Vec::new();
    let mut covered_ops = std::collections::HashSet::new();

    for op in &op_names {
        for prop in &spec.properties {
            if prop.expression.is_none() {
                continue;
            }
            let covered = prop.preserved_by.contains(op);
            if covered {
                covered_ops.insert(op.clone());
            }
            cells.push(CoverageCell {
                operation: op.clone(),
                property: prop.name.clone(),
                covered,
            });
        }
    }

    let gaps: Vec<String> = op_names
        .iter()
        .filter(|op| !covered_ops.contains(*op))
        .cloned()
        .collect();

    let coverage_pct = if op_names.is_empty() {
        100.0
    } else {
        (covered_ops.len() as f64 / op_names.len() as f64) * 100.0
    };

    CoverageMatrix {
        operations: op_names,
        properties: prop_names,
        cells,
        gaps,
        coverage_pct,
    }
}

/// Print a formatted coverage table to stderr.
pub fn print_coverage_table(matrix: &CoverageMatrix) {
    if matrix.properties.is_empty() {
        eprintln!("No properties defined — nothing to show.");
        return;
    }

    // Header row: operation name column + property columns
    let op_col_width = matrix
        .operations
        .iter()
        .map(|o| o.len())
        .max()
        .unwrap_or(9)
        .max(9);
    let prop_col_width = matrix
        .properties
        .iter()
        .map(|p| p.len())
        .max()
        .unwrap_or(4)
        .max(4);

    // Print header
    eprint!("{:<width$}", "operation", width = op_col_width + 2);
    for prop in &matrix.properties {
        eprint!(" {:^width$}", prop, width = prop_col_width);
    }
    eprintln!();

    // Separator
    eprint!("{}", "-".repeat(op_col_width + 2));
    for _ in &matrix.properties {
        eprint!("-{}", "-".repeat(prop_col_width));
    }
    eprintln!();

    // Data rows
    for op in &matrix.operations {
        eprint!("{:<width$}", op, width = op_col_width + 2);
        for prop in &matrix.properties {
            let covered = matrix
                .cells
                .iter()
                .any(|c| &c.operation == op && &c.property == prop && c.covered);
            let mark = if covered { "Y" } else { "-" };
            eprint!(" {:^width$}", mark, width = prop_col_width);
        }
        eprintln!();
    }

    eprintln!();
    eprintln!(
        "Coverage: {:.0}% ({}/{} operations covered by at least one property)",
        matrix.coverage_pct,
        matrix.operations.len() - matrix.gaps.len(),
        matrix.operations.len()
    );

    if !matrix.gaps.is_empty() {
        eprintln!("Gaps: {}", matrix.gaps.join(", "));
    }
}

/// Check code drift — compare generated files against current spec.
pub fn check_code_drift(
    spec: &ParsedSpec,
    fp: &crate::fingerprint::SpecFingerprint,
    code_dir: &std::path::Path,
) -> Result<Vec<DriftResult>> {
    let mut results = Vec::new();

    // Expected files from spec
    let mut expected_files: Vec<String> = vec![
        "src/lib.rs".to_string(),
        "src/state.rs".to_string(),
        "src/instructions/mod.rs".to_string(),
        "Cargo.toml".to_string(),
    ];
    if !spec.events.is_empty() {
        expected_files.push("src/events.rs".to_string());
    }
    if !spec.error_codes.is_empty() {
        expected_files.push("src/errors.rs".to_string());
    }
    for handler in &spec.handlers {
        expected_files.push(format!("src/instructions/{}.rs", handler.name));
    }

    for file in &expected_files {
        let path = code_dir.join(file);
        if !path.exists() {
            results.push(DriftResult {
                file: file.clone(),
                status: DriftStatus::Missing,
                detail: Some("expected by spec but not found".to_string()),
            });
            continue;
        }

        let content = std::fs::read_to_string(&path)?;
        let embedded = crate::fingerprint::extract_spec_hash(&content);
        let expected = fp.file_hashes.get(file.as_str());

        match (embedded, expected) {
            (None, _) => {
                results.push(DriftResult {
                    file: file.clone(),
                    status: DriftStatus::NoHash,
                    detail: Some(
                        "no embedded spec-hash (generated before fingerprinting)".to_string(),
                    ),
                });
            }
            (Some(ref emb), Some(exp)) if emb == exp => {
                results.push(DriftResult {
                    file: file.clone(),
                    status: DriftStatus::InSync,
                    detail: None,
                });
            }
            (Some(_), Some(_)) => {
                results.push(DriftResult {
                    file: file.clone(),
                    status: DriftStatus::SpecChanged,
                    detail: Some("spec changed since last generation".to_string()),
                });
            }
            (Some(_), None) => {
                // Hash in file but no expected hash — shouldn't happen, treat as in-sync
                results.push(DriftResult {
                    file: file.clone(),
                    status: DriftStatus::InSync,
                    detail: None,
                });
            }
        }
    }

    // Check for orphaned instruction files
    let instr_dir = code_dir.join("src/instructions");
    if instr_dir.exists() {
        let expected_ops: Vec<String> = spec
            .handlers
            .iter()
            .map(|o| format!("{}.rs", o.name))
            .collect();
        if let Ok(entries) = std::fs::read_dir(&instr_dir) {
            for entry in entries.flatten() {
                let fname = entry.file_name().to_string_lossy().to_string();
                if fname == "mod.rs" {
                    continue;
                }
                if fname.ends_with(".rs") && !expected_ops.contains(&fname) {
                    results.push(DriftResult {
                        file: format!("src/instructions/{}", fname),
                        status: DriftStatus::Orphaned,
                        detail: Some("file not expected by current spec".to_string()),
                    });
                }
            }
        }
    }

    Ok(results)
}

/// Check Kani drift — compare harness file against current spec.
pub fn check_kani_drift(
    spec: &ParsedSpec,
    fp: &crate::fingerprint::SpecFingerprint,
    kani_path: &std::path::Path,
) -> Result<Vec<KaniDriftResult>> {
    let mut results = Vec::new();

    if !kani_path.exists() {
        results.push(KaniDriftResult {
            harness_name: "(file)".to_string(),
            status: KaniDriftStatus::Missing,
        });
        return Ok(results);
    }

    let content = std::fs::read_to_string(kani_path)?;

    // File-level hash check
    let embedded = crate::fingerprint::extract_spec_hash(&content);
    let expected = fp.file_hashes.get("tests/kani.rs");
    let file_stale = match (embedded, expected) {
        (Some(ref emb), Some(exp)) => emb != exp,
        (None, _) => true,
        _ => false,
    };

    // Build expected harness names (same logic as kani::generate)
    let mut expected_harnesses = Vec::new();
    for op in &spec.handlers {
        if op.who.is_some() {
            expected_harnesses.push(format!("verify_{}_access_control", op.name));
        }
        if op.has_guard() {
            expected_harnesses.push(format!("verify_{}_rejects_invalid", op.name));
        }
        if let (Some(pre_s), Some(post_s)) = (&op.pre_status, &op.post_status) {
            let pre = pre_s.to_lowercase();
            let post = post_s.to_lowercase();
            expected_harnesses.push(format!("verify_{}_transition_{}_to_{}", op.name, pre, post));
        }
        if op.has_effect() {
            expected_harnesses.push(format!("verify_{}_effects", op.name));
        }
    }
    for prop in &spec.properties {
        for op_name in &prop.preserved_by {
            expected_harnesses.push(format!("verify_{}_preserves_{}", op_name, prop.name));
        }
    }

    // Parse file for fn verify_* names
    static FN_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"fn\s+(verify_\w+)\s*\(").unwrap());
    let fn_re = &*FN_RE;
    let found_harnesses: Vec<String> = fn_re
        .captures_iter(&content)
        .map(|c| c[1].to_string())
        .collect();

    for expected in &expected_harnesses {
        if found_harnesses.contains(expected) {
            if file_stale {
                results.push(KaniDriftResult {
                    harness_name: expected.clone(),
                    status: KaniDriftStatus::FileStale,
                });
            } else {
                results.push(KaniDriftResult {
                    harness_name: expected.clone(),
                    status: KaniDriftStatus::InSync,
                });
            }
        } else {
            results.push(KaniDriftResult {
                harness_name: expected.clone(),
                status: KaniDriftStatus::Missing,
            });
        }
    }

    for found in &found_harnesses {
        if !expected_harnesses.contains(found) {
            results.push(KaniDriftResult {
                harness_name: found.clone(),
                status: KaniDriftStatus::Orphaned,
            });
        }
    }

    Ok(results)
}

/// Run unified drift detection across all layers.
pub fn check_unified(
    spec_path: &std::path::Path,
    proofs_dir: &std::path::Path,
    code_dir: Option<&std::path::Path>,
    kani_path: Option<&std::path::Path>,
) -> Result<UnifiedReport> {
    let spec = parse_spec_file(spec_path)?;
    let fp = crate::fingerprint::compute_fingerprint(&spec);

    // 1. Spec completeness
    let completeness = check_completeness(&spec);

    // 2. Code drift
    let code_drift = if let Some(dir) = code_dir {
        Some(check_code_drift(&spec, &fp, dir)?)
    } else {
        None
    };

    // 3. Kani drift
    let kani_drift = if let Some(path) = kani_path {
        Some(check_kani_drift(&spec, &fp, path)?)
    } else {
        None
    };

    // 4. Lean coverage (existing)
    let lean_coverage = check(spec_path, proofs_dir)?;

    Ok(UnifiedReport {
        completeness,
        code_drift,
        kani_drift,
        lean_coverage,
    })
}

/// Print the unified drift report.
pub fn print_unified_report(spec_name: &str, report: &UnifiedReport) {
    // Spec completeness
    let warns = report
        .completeness
        .iter()
        .filter(|w| w.severity == Severity::Warning)
        .count();
    let infos = report
        .completeness
        .iter()
        .filter(|w| w.severity == Severity::Info)
        .count();

    eprintln!("──── Spec Completeness ──────────────────────────────────");
    if report.completeness.is_empty() {
        eprintln!("  (no issues)");
    } else {
        for w in &report.completeness {
            let icon = match w.severity {
                Severity::Error => "E",
                Severity::Warning => "!",
                Severity::Info => "i",
            };
            eprintln!("  {} [{}] {}", icon, w.rule, w.message);
            eprintln!("    Fix: {}", w.fix);
        }
    }
    eprintln!("  {} warning(s), {} info\n", warns, infos);

    // Code drift
    if let Some(ref drift) = report.code_drift {
        eprintln!("──── Code Drift ─────────────────────────────────────────");
        let issues = drift
            .iter()
            .filter(|d| d.status != DriftStatus::InSync)
            .count();
        let synced = drift
            .iter()
            .filter(|d| d.status == DriftStatus::InSync)
            .count();
        for d in drift {
            let (icon, tag) = match d.status {
                DriftStatus::InSync => ("✓", ""),
                DriftStatus::NoHash => ("?", " NO HASH"),
                DriftStatus::SpecChanged => ("✗", " SPEC CHANGED"),
                DriftStatus::Missing => ("✗", " MISSING"),
                DriftStatus::Orphaned => ("?", " ORPHANED"),
            };
            let detail = d
                .detail
                .as_ref()
                .map(|s| format!(" — {}", s))
                .unwrap_or_default();
            eprintln!("  {} {:<40} {}{}", icon, d.file, tag, detail);
        }
        eprintln!("  {} file(s) need attention, {} in sync\n", issues, synced);
    }

    // Kani drift
    if let Some(ref drift) = report.kani_drift {
        eprintln!("──── Kani Drift ─────────────────────────────────────────");
        let issues = drift
            .iter()
            .filter(|d| d.status != KaniDriftStatus::InSync)
            .count();
        let synced = drift
            .iter()
            .filter(|d| d.status == KaniDriftStatus::InSync)
            .count();
        for d in drift {
            let (icon, tag) = match d.status {
                KaniDriftStatus::InSync => ("✓", ""),
                KaniDriftStatus::Missing => ("✗", " MISSING"),
                KaniDriftStatus::Orphaned => ("?", " ORPHANED"),
                KaniDriftStatus::FileStale => ("✗", " FILE STALE"),
            };
            eprintln!("  {} {:<40} {}", icon, d.harness_name, tag);
        }
        eprintln!(
            "  {} harness(es) need attention, {} in sync\n",
            issues, synced
        );
    }

    // Lean coverage
    let proven = report
        .lean_coverage
        .iter()
        .filter(|r| r.status == Status::Proven)
        .count();
    let total = report.lean_coverage.len();

    eprintln!("──── Lean Coverage ──────────────────────────────────────");
    if report.lean_coverage.is_empty() {
        eprintln!("  (no properties declared)");
    } else {
        for r in &report.lean_coverage {
            let (icon, tag) = match r.status {
                Status::Proven => ("✓", ""),
                Status::Sorry => ("✗", " [sorry]"),
                Status::Missing => ("✗", " [missing]"),
            };
            let intent = r
                .intent
                .as_ref()
                .map(|s| format!(" — {}", s))
                .unwrap_or_default();
            eprintln!("  {} {:<40} {}{}", icon, r.name, tag, intent);
        }
    }
    eprintln!("  {}/{} proven\n", proven, total);

    // Summary
    let total_issues = report.issue_count();
    eprintln!(
        "──── {} {} — {} issue(s) ────",
        spec_name,
        if total_issues == 0 { "CLEAN" } else { "DRIFT" },
        total_issues
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_spec() -> ParsedSpec {
        ParsedSpec::default()
    }

    fn make_handler(name: &str) -> ParsedHandler {
        ParsedHandler {
            name: name.to_string(),
            doc: None,
            who: Some("authority".to_string()),
            on_account: None,
            pre_status: Some("Active".to_string()),
            post_status: Some("Active".to_string()),
            takes_params: vec![],
            guard_str: None,
            guard_str_rust: None,
            aborts_if: vec![],
            requires: vec![],
            ensures: vec![],
            modifies: None,
            let_bindings: vec![],
            aborts_total: false,
            effects: vec![],
            accounts: vec![],
            transfers: vec![],
            emits: vec![],
            invariants: vec![],
            properties: vec![],
            calls: vec![],
        }
    }

    #[test]
    fn test_missing_guard_from_takes_fires() {
        let mut h = make_handler("deposit");
        h.takes_params = vec![("amount".to_string(), "U64".to_string())];
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "missing_guard_from_takes"),
            "expected missing_guard_from_takes, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_missing_guard_from_takes_skips_when_guard_exists() {
        let mut h = make_handler("deposit");
        h.takes_params = vec![("amount".to_string(), "U64".to_string())];
        h.guard_str = Some("amount > 0".to_string());
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "missing_guard_from_takes"),
            "should not fire when guard exists"
        );
    }

    #[test]
    fn test_missing_effect_fires() {
        let mut h = make_handler("deposit");
        h.takes_params = vec![("amount".to_string(), "U64".to_string())];
        h.guard_str = Some("amount > 0".to_string());
        // has lifecycle (pre/post set via make_handler) but no effect
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings.iter().any(|w| w.rule == "missing_effect"),
            "expected missing_effect, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_missing_effect_skips_when_effect_exists() {
        let mut h = make_handler("deposit");
        h.takes_params = vec![("amount".to_string(), "U64".to_string())];
        h.guard_str = Some("amount > 0".to_string());
        h.effects = vec![(
            "balance".to_string(),
            "add".to_string(),
            "amount".to_string(),
        )];
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "missing_effect"),
            "should not fire when effect exists"
        );
    }

    #[test]
    fn test_missing_effect_uses_on_account_fields() {
        let mut h = make_handler("borrow");
        h.on_account = Some("Loan".to_string());
        h.takes_params = vec![("loan_amount".to_string(), "U64".to_string())];
        h.guard_str = Some("loan_amount > 0".to_string());
        h.pre_status = Some("Empty".to_string());
        h.post_status = Some("Active".to_string());

        let spec = ParsedSpec {
            handlers: vec![h],
            account_types: vec![
                ParsedAccountType {
                    name: "Pool".to_string(),
                    fields: vec![("total_deposits".to_string(), "U64".to_string())],
                    lifecycle: vec!["Active".to_string()],
                    pda_ref: None,
                },
                ParsedAccountType {
                    name: "Loan".to_string(),
                    fields: vec![("loan_amount".to_string(), "U64".to_string())],
                    lifecycle: vec!["Empty".to_string(), "Active".to_string()],
                    pda_ref: None,
                },
            ],
            state_fields: vec![("total_deposits".to_string(), "U64".to_string())],
            lifecycle_states: vec!["Empty".to_string(), "Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        let warning = warnings
            .iter()
            .find(|w| w.rule == "missing_effect")
            .expect("expected missing_effect warning");
        let example = warning
            .example
            .as_deref()
            .expect("missing_effect should include example");
        assert!(
            example.contains("loan_amount += loan_amount"),
            "expected account-aware suggestion, got: {}",
            example
        );
        assert!(
            !example.contains("total_deposits"),
            "should not use fields from a different account type: {}",
            example
        );
    }

    #[test]
    fn test_no_properties_fires() {
        let mut h = make_handler("deposit");
        h.effects = vec![(
            "balance".to_string(),
            "add".to_string(),
            "amount".to_string(),
        )];
        h.guard_str = Some("amount > 0".to_string());
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings.iter().any(|w| w.rule == "no_properties"),
            "expected no_properties, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_no_properties_skips_with_property() {
        let mut h = make_handler("deposit");
        h.effects = vec![(
            "balance".to_string(),
            "add".to_string(),
            "amount".to_string(),
        )];
        h.guard_str = Some("amount > 0".to_string());
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            properties: vec![ParsedProperty {
                name: "conservation".to_string(),
                expression: Some("state.balance >= 0".to_string()),
                preserved_by: vec!["deposit".to_string()],
            }],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "no_properties"),
            "should not fire when properties exist"
        );
    }

    #[test]
    fn test_missing_cpi_for_token_context() {
        let mut h = make_handler("transfer");
        // Has token program in accounts but no transfers block
        h.accounts = vec![
            ParsedHandlerAccount {
                name: "authority".to_string(),
                is_signer: true,
                is_writable: false,
                is_program: false,
                pda_seeds: None,
                account_type: None,
                authority: None,
            },
            ParsedHandlerAccount {
                name: "source".to_string(),
                is_signer: false,
                is_writable: true,
                is_program: false,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
            },
            ParsedHandlerAccount {
                name: "dest".to_string(),
                is_signer: false,
                is_writable: true,
                is_program: false,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
            },
            ParsedHandlerAccount {
                name: "token_program".to_string(),
                is_signer: false,
                is_writable: false,
                is_program: true,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
            },
        ];
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "missing_cpi_for_token_context"),
            "expected missing_cpi_for_token_context, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_lifecycle_unreachable_state() {
        let mut h = make_handler("initialize");
        h.pre_status = Some("Uninitialized".to_string());
        h.post_status = Some("Active".to_string());
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec![
                "Uninitialized".to_string(),
                "Active".to_string(),
                "Closed".to_string(),
            ],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "lifecycle_unreachable_state"
                    && w.subject.as_deref() == Some("Closed")),
            "expected lifecycle_unreachable_state for Closed, got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.subject))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_lifecycle_disconnected_subgraph_is_unreachable() {
        let mut init = make_handler("initialize");
        init.pre_status = Some("Uninitialized".to_string());
        init.post_status = Some("Active".to_string());

        let mut close = make_handler("close");
        close.pre_status = Some("Frozen".to_string());
        close.post_status = Some("Closed".to_string());

        let spec = ParsedSpec {
            handlers: vec![init, close],
            lifecycle_states: vec![
                "Uninitialized".to_string(),
                "Active".to_string(),
                "Frozen".to_string(),
                "Closed".to_string(),
            ],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings.iter().any(|w| {
                w.rule == "lifecycle_unreachable_state" && w.subject.as_deref() == Some("Frozen")
            }),
            "expected disconnected state Frozen to be unreachable, got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.subject))
                .collect::<Vec<_>>()
        );
        assert!(
            warnings.iter().any(|w| {
                w.rule == "lifecycle_unreachable_state" && w.subject.as_deref() == Some("Closed")
            }),
            "expected downstream state Closed to be unreachable, got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.subject))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_global_initial_state_seeded_when_account_lifecycle_differs() {
        // Account lifecycle starts at "Active", but the global initial state
        // is "Uninitialized". Without always seeding the global initial state,
        // "Uninitialized" would be flagged as unreachable even though it is
        // the entry point of the lifecycle.
        let mut init = make_handler("initialize");
        init.pre_status = Some("Uninitialized".to_string());
        init.post_status = Some("Active".to_string());

        let spec = ParsedSpec {
            handlers: vec![init],
            account_types: vec![ParsedAccountType {
                name: "Pool".to_string(),
                fields: vec![],
                lifecycle: vec!["Active".to_string(), "Frozen".to_string()],
                pda_ref: None,
            }],
            lifecycle_states: vec![
                "Uninitialized".to_string(),
                "Active".to_string(),
                "Frozen".to_string(),
            ],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| {
                w.rule == "lifecycle_unreachable_state"
                    && w.subject.as_deref() == Some("Uninitialized")
            }),
            "Uninitialized is the global initial state and should NOT be flagged as unreachable, got: {:?}",
            warnings
                .iter()
                .filter(|w| w.rule == "lifecycle_unreachable_state")
                .map(|w| &w.subject)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_no_errors_block_fires() {
        let mut h = make_handler("deposit");
        h.guard_str = Some("amount > 0".to_string());
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings.iter().any(|w| w.rule == "no_errors_block"),
            "expected no_errors_block, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_priority_ordering() {
        // Build a spec that triggers multiple rules at different priorities
        let mut h = make_handler("deposit");
        h.who = None; // priority 1: no_access_control
        h.takes_params = vec![("amount".to_string(), "U64".to_string())];
        h.effects = vec![(
            "balance".to_string(),
            "add".to_string(),
            "amount".to_string(),
        )];
        // no guard → priority 1: unguarded_arithmetic + missing_guard_from_takes
        // no properties → priority 3: no_properties
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![
                ("authority".to_string(), "Pubkey".to_string()),
                ("balance".to_string(), "U64".to_string()),
            ],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        // Verify sorted ascending by priority
        for window in warnings.windows(2) {
            assert!(
                window[0].priority <= window[1].priority,
                "warnings not sorted by priority: {} ({}) should come before {} ({})",
                window[0].rule,
                window[0].priority,
                window[1].rule,
                window[1].priority
            );
        }
    }

    #[test]
    fn test_complete_spec_clean() {
        let spec_content = include_str!("../../../examples/rust/escrow/escrow.qedspec");
        let spec =
            crate::chumsky_adapter::parse_str(spec_content).expect("escrow.qedspec should parse");
        let warnings = check_completeness(&spec);
        // A well-formed spec should have zero warnings
        let warning_rules: Vec<&str> = warnings
            .iter()
            .filter(|w| w.severity == Severity::Warning)
            .map(|w| w.rule.as_str())
            .collect();
        assert!(
            warning_rules.is_empty(),
            "escrow.qedspec should be clean but got warnings: {:?}",
            warning_rules
        );
    }

    // ========================================================================
    // v2.0 tests: coverage matrix, write_without_read, circular_lifecycle
    // ========================================================================

    #[test]
    fn test_coverage_matrix_full_coverage() {
        let spec_content = include_str!("../../../examples/rust/multisig/multisig.qedspec");
        let spec =
            crate::chumsky_adapter::parse_str(spec_content).expect("multisig.qedspec should parse");
        let matrix = coverage_matrix(&spec);
        assert_eq!(matrix.coverage_pct, 100.0);
        assert!(matrix.gaps.is_empty());
        assert_eq!(matrix.operations.len(), 7);
        assert_eq!(matrix.properties.len(), 2);
    }

    #[test]
    fn test_coverage_matrix_detects_gaps() {
        let mut h_covered = make_handler("deposit");
        h_covered.effects = vec![("balance".into(), "add".into(), "amount".into())];
        let mut h_uncovered = make_handler("withdraw");
        h_uncovered.effects = vec![("balance".into(), "sub".into(), "amount".into())];

        let spec = ParsedSpec {
            handlers: vec![h_covered, h_uncovered],
            state_fields: vec![("balance".into(), "U64".into())],
            properties: vec![ParsedProperty {
                name: "conservation".to_string(),
                expression: Some("state.balance >= 0".to_string()),
                preserved_by: vec!["deposit".to_string()], // only covers deposit
            }],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let matrix = coverage_matrix(&spec);
        assert_eq!(matrix.gaps, vec!["withdraw"]);
        assert!(matrix.coverage_pct < 100.0);
    }

    #[test]
    fn test_write_without_read_lint() {
        let mut h = make_handler("deposit");
        h.guard_str = Some("amount > 0".to_string());
        h.effects = vec![
            ("balance".into(), "add".into(), "amount".into()),
            ("counter".into(), "add".into(), "1".into()),
        ];
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![
                ("authority".into(), "Pubkey".into()),
                ("balance".into(), "U64".into()),
                ("counter".into(), "U64".into()),
            ],
            properties: vec![ParsedProperty {
                name: "conservation".to_string(),
                expression: Some("s.balance >= 0".to_string()),
                preserved_by: vec!["deposit".to_string()],
            }],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        // "counter" is written but never read in any guard or property
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "write_without_read" && w.subject.as_deref() == Some("counter")),
            "expected write_without_read for 'counter', got: {:?}",
            warnings
                .iter()
                .filter(|w| w.rule == "write_without_read")
                .map(|w| &w.subject)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_circular_lifecycle_no_terminal() {
        let mut h1 = make_handler("advance");
        h1.pre_status = Some("A".to_string());
        h1.post_status = Some("B".to_string());
        let mut h2 = make_handler("retreat");
        h2.pre_status = Some("B".to_string());
        h2.post_status = Some("A".to_string());
        let spec = ParsedSpec {
            handlers: vec![h1, h2],
            lifecycle_states: vec!["A".to_string(), "B".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "circular_lifecycle_no_terminal"),
            "expected circular_lifecycle_no_terminal, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    // ---- contains_word unit tests ----

    #[test]
    fn test_contains_word_basic() {
        assert!(contains_word("balance > 0", "balance"));
        assert!(contains_word("check balance here", "balance"));
        assert!(!contains_word("imbalance > 0", "balance"));
        assert!(!contains_word("rebalance_flag", "balance"));
        assert!(!contains_word("my_balance_v2", "balance"));
    }

    #[test]
    fn test_contains_word_short_field() {
        // Field "id" must not match inside "valid", "provide", "identity"
        assert!(!contains_word("valid > 0", "id"));
        assert!(!contains_word("provide_service", "id"));
        assert!(!contains_word("identity = true", "id"));
        // But should match when standalone
        assert!(contains_word("id > 0", "id"));
        assert!(contains_word("state.id > 0", "id"));
        assert!(contains_word("check id here", "id"));
    }

    #[test]
    fn test_contains_word_at_boundaries() {
        assert!(contains_word("id", "id"));
        assert!(contains_word("id ", "id"));
        assert!(contains_word(" id", "id"));
        assert!(contains_word("(id)", "id"));
        assert!(contains_word("id+1", "id"));
        assert!(!contains_word("kid", "id"));
        assert!(!contains_word("ids", "id"));
    }

    // ---- write_without_read word-boundary tests ----

    #[test]
    fn test_write_without_read_no_substring_match() {
        // Field "id" written in effects, guard only has "valid" — should NOT count as read
        let mut h = make_handler("update");
        h.effects = vec![("id".to_string(), "set".to_string(), "1".to_string())];
        h.guard_str = Some("valid > 0".to_string());
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![
                ("id".to_string(), "U64".to_string()),
                ("valid".to_string(), "U64".to_string()),
            ],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "write_without_read"
                    && w.subject.as_deref() == Some("id")),
            "field 'id' should be flagged as write_without_read when guard only contains 'valid', got: {:?}",
            warnings.iter().filter(|w| w.rule == "write_without_read").collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_write_without_read_bare_word_match() {
        // Field "balance" written in effects, guard has "balance > 0" — should count as read
        let mut h = make_handler("deposit");
        h.effects = vec![(
            "balance".to_string(),
            "add".to_string(),
            "amount".to_string(),
        )];
        h.guard_str = Some("balance > 0".to_string());
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "write_without_read"
                    && w.subject.as_deref() == Some("balance")),
            "field 'balance' should NOT be flagged when guard contains bare word 'balance', got: {:?}",
            warnings.iter().filter(|w| w.rule == "write_without_read").collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_write_without_read_prefixed_match() {
        // Field "id" written, guard has "state.id > 0" — should count as read
        let mut h = make_handler("update");
        h.effects = vec![("id".to_string(), "set".to_string(), "1".to_string())];
        h.guard_str = Some("state.id > 0".to_string());
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("id".to_string(), "U64".to_string())],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "write_without_read" && w.subject.as_deref() == Some("id")),
            "field 'id' should NOT be flagged when guard contains 'state.id', got: {:?}",
            warnings
                .iter()
                .filter(|w| w.rule == "write_without_read")
                .collect::<Vec<_>>()
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Multi-file spec loader
    // ──────────────────────────────────────────────────────────────────────

    const SPEC_ROOT: &str = r#"
spec Demo

type State
  | Active of { count : U64 }
"#;

    const SPEC_INC: &str = r#"
spec Demo

/// Increments count
handler inc (x : U64) : State.Active -> State.Active {
  effect { count += x }
}
"#;

    const SPEC_DEC: &str = r#"
spec Demo

handler dec (x : U64) : State.Active -> State.Active {
  effect { count -= x }
}
"#;

    #[test]
    fn multi_file_spec_merges_handlers_across_fragments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("demo.qedspec"), SPEC_ROOT).unwrap();
        std::fs::create_dir_all(dir.path().join("handlers")).unwrap();
        std::fs::write(dir.path().join("handlers/inc.qedspec"), SPEC_INC).unwrap();
        std::fs::write(dir.path().join("handlers/dec.qedspec"), SPEC_DEC).unwrap();

        let parsed = parse_spec_file(dir.path()).unwrap();
        assert_eq!(parsed.program_name, "Demo");
        let names: Vec<_> = parsed.handlers.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"inc"), "got handlers: {:?}", names);
        assert!(names.contains(&"dec"), "got handlers: {:?}", names);
    }

    #[test]
    fn multi_file_spec_rejects_name_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.qedspec"), SPEC_ROOT).unwrap();
        std::fs::write(
            dir.path().join("b.qedspec"),
            "spec Other\n\nhandler noop : State.Active -> State.Active { effect {} }\n",
        )
        .unwrap();

        let err = parse_spec_file(dir.path()).unwrap_err().to_string();
        assert!(
            err.contains("spec name mismatch"),
            "expected name-mismatch error, got: {err}"
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Interface adapter round-trip (v2.5 slice 1)
    // ──────────────────────────────────────────────────────────────────────

    // ──────────────────────────────────────────────────────────────────────
    // [shape_only_cpi] lint (v2.5 slice 4)
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn shape_only_cpi_fires_on_tier0_interface() {
        // Interface declared with no ensures — classic Tier-0. Should lint.
        let src = r#"spec Demo

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let ws = check_completeness(&parsed);
        let hits: Vec<_> = ws.iter().filter(|w| w.rule == "shape_only_cpi").collect();
        assert_eq!(
            hits.len(),
            1,
            "expected one shape_only_cpi warning, got {:?}",
            ws
        );
        assert!(hits[0].message.contains("shape only"));
    }

    #[test]
    fn shape_only_cpi_fires_on_undeclared_interface() {
        let src = r#"spec Demo

handler pay : State.A -> State.A {
  call Jupiter.swap(pool = amm, amount_in = 100, min_out = 90)
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let ws = check_completeness(&parsed);
        let hits: Vec<_> = ws.iter().filter(|w| w.rule == "shape_only_cpi").collect();
        assert_eq!(
            hits.len(),
            1,
            "expected one shape_only_cpi warning, got {:?}",
            ws
        );
        assert!(hits[0].message.contains("not declared"));
    }

    #[test]
    fn shape_only_cpi_silent_on_tier1_interface() {
        // Interface declares at least one ensures — no lint should fire.
        let src = r#"spec Demo

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let ws = check_completeness(&parsed);
        let hits: Vec<_> = ws.iter().filter(|w| w.rule == "shape_only_cpi").collect();
        assert!(
            hits.is_empty(),
            "Tier 1 interfaces should not lint, got: {:?}",
            hits
        );
    }

    #[test]
    fn call_clause_populates_handler_calls() {
        let src = r#"spec Demo

handler exchange : State.A -> State.B {
  call Token.transfer(from = taker_ta, to = initializer_ta, amount = taker_amount)
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let handler = &parsed.handlers[0];
        assert_eq!(handler.calls.len(), 1);
        let c = &handler.calls[0];
        assert_eq!(c.target_interface, "Token");
        assert_eq!(c.target_handler, "transfer");
        assert_eq!(c.args.len(), 3);
        assert_eq!(c.args[0].name, "from");
        assert_eq!(c.args[2].name, "amount");
        // Args carry both renderings so backends can pick the form they want.
        assert!(!c.args[0].rust_expr.is_empty());
        assert!(!c.args[0].lean_expr.is_empty());
    }

    // ──────────────────────────────────────────────────────────────────────
    // pragma sbpf { ... } adaptation
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn pragma_sbpf_unpacks_inner_items() {
        let src = r#"spec Transfer

pragma sbpf {
  pubkey TOKEN_PROGRAM [6, 221, 246, 225]

  instruction transfer {
    discriminant 3
    entry 0
  }
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        assert_eq!(parsed.pragmas, vec!["sbpf".to_string()]);
        assert_eq!(parsed.pubkeys.len(), 1);
        assert_eq!(parsed.pubkeys[0].name, "TOKEN_PROGRAM");
        assert_eq!(parsed.instructions.len(), 1);
        assert_eq!(parsed.instructions[0].name, "transfer");
    }

    #[test]
    fn pragma_body_adapts_into_standard_parsed_spec_fields() {
        // Items wrapped in `pragma sbpf { ... }` must land in the same
        // ParsedSpec fields downstream consumers already read — pubkeys,
        // instructions, etc. The pragma is a grammatical namespace, not
        // a new parallel tree.
        let src = r#"spec T

pragma sbpf {
  pubkey TOKEN_PROGRAM [1, 2, 3, 4]

  instruction foo {
    discriminant 1
    entry 0
  }
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        assert_eq!(parsed.pragmas, vec!["sbpf".to_string()]);
        assert!(parsed.has_pragma("sbpf"));
        assert_eq!(parsed.pubkeys.len(), 1);
        assert_eq!(parsed.pubkeys[0].name, "TOKEN_PROGRAM");
        assert_eq!(parsed.instructions.len(), 1);
        assert_eq!(parsed.instructions[0].name, "foo");
    }

    #[test]
    fn top_level_sbpf_items_now_rejected() {
        // Platform-specifics (pubkey, instruction, assembly) used to parse
        // at the top level; v2.5 moves them behind `pragma sbpf { ... }`.
        // The grammar enforces the discipline so a spec can't quietly mix
        // them into the core surface.
        let src = r#"spec T

pubkey TOKEN_PROGRAM [1, 2, 3, 4]
"#;
        assert!(
            crate::chumsky_adapter::parse_str(src).is_err(),
            "top-level `pubkey` should no longer parse"
        );
    }

    #[test]
    fn interface_block_populates_parsed_spec() {
        let src = r#"spec Escrow

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"

  upstream {
    package      "spl-token"
    version      "4.0.3"
    binary_hash  "sha256:abc"
    verified_with ["proptest", "kani"]
    verified_at  "2026-04-18"
  }

  handler transfer (amount : U64) {
    accounts {
      from      : writable, type token
      to        : writable, type token
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        assert_eq!(parsed.interfaces.len(), 1);
        let i = &parsed.interfaces[0];
        assert_eq!(i.name, "Token");
        assert_eq!(
            i.program_id.as_deref(),
            Some("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA")
        );

        let u = i.upstream.as_ref().expect("upstream present");
        assert_eq!(u.binary_hash.as_deref(), Some("sha256:abc"));
        // Lean absent by design — no overclaiming.
        assert!(!u.verified_with.contains(&"lean".to_string()));

        assert_eq!(i.handlers.len(), 1);
        let h = &i.handlers[0];
        assert_eq!(h.name, "transfer");
        assert_eq!(h.params, vec![("amount".to_string(), "U64".to_string())]);
        assert_eq!(h.accounts.len(), 3);
        assert_eq!(h.requires.len(), 1);
        assert_eq!(h.ensures.len(), 1);
    }

    #[test]
    fn multi_file_spec_source_matches_single_file_concat() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("1.qedspec"), SPEC_ROOT).unwrap();
        std::fs::write(dir.path().join("2.qedspec"), SPEC_INC).unwrap();

        // read_spec_source must emit fragments in sorted-path order so
        // spec_hash_for_handler finds handler bodies regardless of which
        // fragment they live in.
        let src = read_spec_source(dir.path()).unwrap();
        assert!(
            src.contains("type State"),
            "root fragment missing in merged source"
        );
        assert!(
            src.contains("handler inc"),
            "handler fragment missing in merged source"
        );
    }
}
