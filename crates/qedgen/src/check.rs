use anyhow::Result;
use regex::Regex;
use std::path::Path;

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

/// Parsed operation from a qedspec block with its clauses.
#[derive(Debug)]
pub struct ParsedOperation {
    pub name: String,
    pub doc: Option<String>,
    pub who: Option<String>,
    /// Which account type this operation targets (from `on` clause).
    /// None means the default (first/only) account.
    #[allow(dead_code)]
    pub on_account: Option<String>,
    pub has_when: bool,
    pub pre_status: Option<String>,
    pub post_status: Option<String>,
    pub has_calls: bool,
    pub program_id: Option<String>,
    #[allow(dead_code)]
    pub has_u64_fields: bool,
    #[allow(dead_code)]
    pub has_takes: bool,
    pub has_guard: bool,
    pub guard_str: Option<String>,
    pub has_effect: bool,
    #[allow(dead_code)]
    pub takes_params: Vec<(String, String)>,
    #[allow(dead_code)]
    pub effects: Vec<(String, String, String)>,
    #[allow(dead_code)]
    pub calls_accounts: Vec<(String, String)>,
    #[allow(dead_code)]
    pub calls_discriminator: Option<String>,
    #[allow(dead_code)]
    pub emits: Vec<String>,
}

/// Parsed property from a qedspec block.
#[derive(Debug)]
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

/// Full parsed spec context.
#[derive(Debug, Default)]
pub struct ParsedSpec {
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

/// Parse a spec file from disk. Only .qedspec format is supported.
pub fn parse_spec_file(path: &Path) -> Result<ParsedSpec> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext != "qedspec" {
        anyhow::bail!(
            "Unsupported spec format: .{}. Only .qedspec files are supported.\n\
             Convert Lean specs to .qedspec format (see examples/).",
            ext
        );
    }
    crate::parser::parse_file(path)
}

/// Generate the full list of expected properties with intent descriptions.
/// Returns (property_name, intent_description, optional_suggestion).
///
/// Post-refactoring: Lean only generates CPI correctness theorems (per-op)
/// and inductive property preservation theorems (one per property, not per-op).
/// Access control, state machine, and u64_bounds are covered by Kani/unit tests.
fn generate_properties(spec: &ParsedSpec) -> Vec<(String, String, Option<String>)> {
    let mut props = Vec::new();

    for op in &spec.operations {
        // CPI correctness (only when calls: specified) — unique to Lean, typically rfl
        if op.has_calls {
            let program = op.program_id.as_deref().unwrap_or("?");
            let intent = format!(
                "{} CPI targets {} with correct accounts and discriminator",
                op.name, program
            );
            let suggestion = Some(
                "The CPI builder is generated by the DSL — this should be provable by rfl/exact."
                    .to_string(),
            );
            props.push((format!("{}.cpi_correct", op.name), intent, suggestion));
        }
    }

    // Invariants
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

    // Inductive property preservation — one theorem per property (not per op)
    // Lean generates: theorem {prop}_inductive (s s' : State) (signer : Pubkey) (op : Operation)
    //   (h_inv : prop s) (h : applyOp s signer op = some s') : prop s'
    for prop in &spec.properties {
        let ops_list = prop.preserved_by.join(", ");
        let intent = format!(
            "{} is preserved by every operation ({}). Inductive proof over Operation type.",
            prop.name, ops_list
        );
        let suggestion = Some(format!(
            "Prove by `cases op` then unfold/omega per case. Each case reduces to showing \
             that {}Transition preserves {} — the Operation inductive gives you all cases.",
            prop.preserved_by.first().unwrap_or(&"<op>".to_string()),
            prop.name
        ));
        props.push((format!("{}_inductive", prop.name), intent, suggestion));
    }

    props
}

/// Check whether a property is proven, sorry, or missing in the proof content.
fn check_property_status(property_name: &str, proof_content: &str) -> Status {
    // The property name uses dots (e.g., "initialize.access_control").
    // Proofs may use either dots (DSL-generated sorry stubs) or underscores
    // (proof namespace, e.g., "initialize_access_control").
    // Also handle «»-quoted names (e.g., «initialize».access_control).
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

    let theorem_pattern = format!(
        r"theorem\s+\S*(?:{}|{}|{})",
        escaped_dot, escaped_under, escaped_quoted
    );
    let theorem_re = Regex::new(&theorem_pattern).unwrap();

    let Some(m) = theorem_re.find(proof_content) else {
        return Status::Missing;
    };

    // Extract theorem body: from the match to the next top-level keyword
    let rest = &proof_content[m.start()..];
    let body_end_re =
        Regex::new(r"\n(?:theorem|def|noncomputable def|namespace|end|section|#)").unwrap();
    let body = match body_end_re.find(&rest[1..]) {
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

/// Print a formatted coverage report with intent descriptions.
pub fn print_report(spec_name: &str, results: &[PropertyStatus]) {
    let proven = results
        .iter()
        .filter(|r| r.status == Status::Proven)
        .count();
    let sorry = results.iter().filter(|r| r.status == Status::Sorry).count();
    let missing = results
        .iter()
        .filter(|r| r.status == Status::Missing)
        .count();
    let total = results.len();

    eprintln!(
        "{} spec coverage ({}/{} proven):\n",
        spec_name, proven, total
    );
    for r in results {
        let icon = match r.status {
            Status::Proven => "✓",
            Status::Sorry => "✗",
            Status::Missing => "✗",
        };
        let intent_str = r
            .intent
            .as_deref()
            .map(|i| format!(" — {}", i))
            .unwrap_or_default();

        let status_tag = match r.status {
            Status::Proven => "".to_string(),
            Status::Sorry => " [SORRY]".to_string(),
            Status::Missing => " [MISSING]".to_string(),
        };

        eprintln!("  {} {}{}{}", icon, r.name, intent_str, status_tag);

        // Print suggestion for unproven properties
        if r.status != Status::Proven {
            if let Some(ref suggestion) = r.suggestion {
                eprintln!("    → {}", suggestion);
            }
        }
    }
    eprintln!();
    eprintln!(
        "Summary: {} proven, {} sorry, {} missing ({} total)",
        proven, sorry, missing, total
    );

    if proven == total {
        eprintln!("All properties verified.");
    }
}

// ============================================================================
// Unified drift detection (qedgen check --code --kani)
// ============================================================================

/// Severity of a completeness warning.
#[derive(Debug, PartialEq, Clone, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Info,
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

fn fields_for_operation<'a>(spec: &'a ParsedSpec, op: &ParsedOperation) -> &'a [(String, String)] {
    if let Some(account_name) = op.on_account.as_deref() {
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
    op: &ParsedOperation,
    is_init_like: bool,
) -> Vec<String> {
    op.takes_params
        .iter()
        .map(|(name, _)| name.as_str())
        .take(3)
        .map(|param| {
            let matching_field = fields_for_operation(spec, op)
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
        for op in &spec.operations {
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

    for op in &spec.operations {
        // Rule 1: operation without who:
        if op.who.is_none() {
            warnings.push(CompletenessWarning {
                rule: "no_access_control".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!("operation '{}' has no `who:` — anyone can call it", op.name),
                subject: Some(op.name.clone()),
                fix: format!(
                    "Add `who: {}` to restrict who can execute this operation",
                    signer_hint
                ),
                example: Some(format!("  operation {}\n    who: {}", op.name, signer_hint)),
            });
        }

        // Rule 2: operation not covered by any property
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
                    "operation '{}' is not in any property's `preserved_by`",
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
            });
        }

        // Rule 3: arithmetic effect without guard
        let arithmetic_effects: Vec<&(String, String, String)> = op
            .effects
            .iter()
            .filter(|(_, kind, _)| kind == "add" || kind == "sub")
            .collect();
        if !arithmetic_effects.is_empty() && !op.has_guard {
            let (field, kind, val) = arithmetic_effects[0];
            let guard_suggestion = if kind == "add" {
                format!("s.{} + {} ≤ U64_MAX", field, val)
            } else {
                format!("s.{} ≥ {}", field, val)
            };
            warnings.push(CompletenessWarning {
                rule: "unguarded_arithmetic".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!(
                    "operation '{}' has arithmetic effects but no `guard:` — potential overflow/underflow",
                    op.name
                ),
                subject: Some(op.name.clone()),
                fix: format!(
                    "Add a `guard:` clause to prevent {} overflow",
                    if kind == "add" { "addition" } else { "subtraction" }
                ),
                example: Some(format!(
                    "  operation {}\n    guard: \"{}\"",
                    op.name, guard_suggestion
                )),
            });
        }

        // Rule 6: operation has no when:/then: lifecycle
        if op.pre_status.is_none() && op.post_status.is_none() {
            warnings.push(CompletenessWarning {
                rule: "no_lifecycle".to_string(),
                severity: Severity::Info,
                priority: 2,
                message: format!(
                    "operation '{}' has no `when:`/`then:` — no state machine enforcement",
                    op.name
                ),
                subject: Some(op.name.clone()),
                fix: "Add `when:` and `then:` clauses to enforce operation ordering".to_string(),
                example: Some(format!(
                    "  operation {}\n    when: Active\n    then: Active",
                    op.name
                )),
            });
        }
    }

    // Rule 4: state fields never modified (excluding Pubkey)
    for (fname, ftype) in &spec.state_fields {
        if ftype == "Pubkey" {
            continue;
        }
        let modified = spec
            .operations
            .iter()
            .any(|op| op.effects.iter().any(|(f, _, _)| f == fname));
        if !modified {
            let mutating_ops: Vec<&str> = spec
                .operations
                .iter()
                .filter(|op| op.has_effect)
                .map(|op| op.name.as_str())
                .collect();
            let op_hint = mutating_ops.first().copied().unwrap_or("some_operation");
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
            });
        }
    }

    // Rule 5: property references nonexistent operation
    let op_names: Vec<&str> = spec.operations.iter().map(|o| o.name.as_str()).collect();
    for prop in &spec.properties {
        for op_name in &prop.preserved_by {
            if !op_names.contains(&op_name.as_str()) {
                warnings.push(CompletenessWarning {
                    rule: "dangling_preserved_by".to_string(),
                    severity: Severity::Warning,
                    priority: 1,
                    message: format!(
                        "property '{}' references nonexistent operation '{}'",
                        prop.name, op_name
                    ),
                    subject: Some(format!("{}.preserved_by.{}", prop.name, op_name)),
                    fix: format!(
                        "Check the spelling of '{}' — available operations: {}",
                        op_name,
                        op_names.join(", ")
                    ),
                    example: None,
                });
            }
        }
    }

    // Rule 7: takes params (U64) with no guard — suggest input validation
    for op in &spec.operations {
        if op.has_guard {
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
                    "operation '{}' takes U64 params but has no guard — no input validation",
                    op.name
                ),
                subject: Some(op.name.clone()),
                fix: "Add input validation for takes parameters".to_string(),
                example: Some(format!("  operation {}\n    guard {}", op.name, guard_expr)),
            });
        }
    }

    // Rule 8: takes params + lifecycle transition but no effect
    for op in &spec.operations {
        if op.has_effect {
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
                    "operation '{}' takes params and transitions state but has no effect",
                    op.name
                ),
                subject: Some(op.name.clone()),
                fix: "Add an effect block to describe state changes".to_string(),
                example: Some(format!(
                    "  operation {}\n  effect {{\n{}\n  }}",
                    op.name,
                    effect_lines.join("\n")
                )),
            });
        }
    }

    // Rule 9: operations with effects but zero properties
    let has_effects = spec.operations.iter().any(|op| op.has_effect);
    if has_effects && spec.properties.is_empty() && spec.invariants.is_empty() {
        // Suggest conservation if paired add/sub exist on same field
        let mut modified_fields: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for op in &spec.operations {
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
            .operations
            .iter()
            .filter(|op| op.has_effect)
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
        });
    }

    // Rule 10: context has Token program but operation has no calls
    for ctx in &spec.contexts {
        let has_token_program = ctx
            .accounts
            .iter()
            .any(|a| a.account_type == "Program" && a.inner_type.as_deref() == Some("Token"));
        if !has_token_program {
            continue;
        }
        let op = spec.operations.iter().find(|o| o.name == ctx.operation);
        if let Some(op) = op {
            if !op.has_calls {
                let writable_tokens: Vec<&str> = ctx
                    .accounts
                    .iter()
                    .filter(|a| {
                        a.is_mut
                            && a.inner_type.as_deref() == Some("Token")
                            && a.account_type == "Account"
                    })
                    .map(|a| a.name.as_str())
                    .collect();
                let signer_name = ctx
                    .accounts
                    .iter()
                    .find(|a| a.account_type == "Signer")
                    .map(|a| a.name.as_str())
                    .unwrap_or("authority");
                let accounts_str = if writable_tokens.len() >= 2 {
                    format!(
                        "{} writable, {} writable, {} signer",
                        writable_tokens[0], writable_tokens[1], signer_name
                    )
                } else if writable_tokens.len() == 1 {
                    format!("{} writable, {} signer", writable_tokens[0], signer_name)
                } else {
                    format!("source writable, dest writable, {} signer", signer_name)
                };
                warnings.push(CompletenessWarning {
                    rule: "missing_cpi_for_token_context".to_string(),
                    severity: Severity::Warning,
                    priority: 2,
                    message: format!(
                        "operation '{}' has Token program in context but no `calls` clause",
                        ctx.operation
                    ),
                    subject: Some(ctx.operation.clone()),
                    fix: "Add a `calls` clause to verify the CPI invocation".to_string(),
                    example: Some(format!(
                        "  operation {}\n    calls TOKEN_PROGRAM_ID DISC_TRANSFER({})",
                        ctx.operation, accounts_str
                    )),
                });
            }
        }
    }

    // Rule 11: no errors block but operations have guards
    let any_guards = spec.operations.iter().any(|op| op.has_guard);
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
                });
            }
        }
    }

    // Sort by priority (ascending), then by rule name for stability
    warnings.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.rule.cmp(&b.rule)));

    warnings
}

/// Run standalone lint — returns structured JSON for agent consumption.
pub fn lint(spec_path: &std::path::Path) -> Result<Vec<CompletenessWarning>> {
    let spec = parse_spec_file(spec_path)?;
    Ok(check_completeness(&spec))
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
    for op in &spec.operations {
        expected_files.push(format!("src/instructions/{}.rs", op.name));
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
            .operations
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
    for op in &spec.operations {
        if op.who.is_some() {
            expected_harnesses.push(format!("verify_{}_access_control", op.name));
        }
        if op.has_guard {
            expected_harnesses.push(format!("verify_{}_rejects_invalid", op.name));
        }
        if let (Some(pre_s), Some(post_s)) = (&op.pre_status, &op.post_status) {
            let pre = pre_s.to_lowercase();
            let post = post_s.to_lowercase();
            expected_harnesses.push(format!("verify_{}_transition_{}_to_{}", op.name, pre, post));
        }
        if op.has_effect {
            expected_harnesses.push(format!("verify_{}_effects", op.name));
        }
    }
    for prop in &spec.properties {
        for op_name in &prop.preserved_by {
            expected_harnesses.push(format!("verify_{}_preserves_{}", op_name, prop.name));
        }
    }

    // Parse file for fn verify_* names
    let fn_re = regex::Regex::new(r"fn\s+(verify_\w+)\s*\(").unwrap();
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

    fn make_op(name: &str) -> ParsedOperation {
        ParsedOperation {
            name: name.to_string(),
            doc: None,
            who: Some("authority".to_string()),
            on_account: None,
            has_when: false,
            pre_status: Some("Active".to_string()),
            post_status: Some("Active".to_string()),
            has_calls: false,
            program_id: None,
            has_u64_fields: false,
            has_takes: false,
            has_guard: false,
            guard_str: None,
            has_effect: false,
            takes_params: vec![],
            effects: vec![],
            calls_accounts: vec![],
            calls_discriminator: None,
            emits: vec![],
        }
    }

    #[test]
    fn test_missing_guard_from_takes_fires() {
        let mut op = make_op("deposit");
        op.takes_params = vec![("amount".to_string(), "U64".to_string())];
        op.has_takes = true;
        let spec = ParsedSpec {
            operations: vec![op],
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
        let mut op = make_op("deposit");
        op.takes_params = vec![("amount".to_string(), "U64".to_string())];
        op.has_takes = true;
        op.has_guard = true;
        op.guard_str = Some("amount > 0".to_string());
        let spec = ParsedSpec {
            operations: vec![op],
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
        let mut op = make_op("deposit");
        op.takes_params = vec![("amount".to_string(), "U64".to_string())];
        op.has_takes = true;
        op.has_guard = true;
        // has lifecycle (pre/post set via make_op) but no effect
        let spec = ParsedSpec {
            operations: vec![op],
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
        let mut op = make_op("deposit");
        op.takes_params = vec![("amount".to_string(), "U64".to_string())];
        op.has_takes = true;
        op.has_guard = true;
        op.has_effect = true;
        op.effects = vec![(
            "balance".to_string(),
            "add".to_string(),
            "amount".to_string(),
        )];
        let spec = ParsedSpec {
            operations: vec![op],
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
        let mut op = make_op("borrow");
        op.on_account = Some("Loan".to_string());
        op.takes_params = vec![("loan_amount".to_string(), "U64".to_string())];
        op.has_takes = true;
        op.has_guard = true;
        op.pre_status = Some("Empty".to_string());
        op.post_status = Some("Active".to_string());

        let spec = ParsedSpec {
            operations: vec![op],
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
        let mut op = make_op("deposit");
        op.has_effect = true;
        op.effects = vec![(
            "balance".to_string(),
            "add".to_string(),
            "amount".to_string(),
        )];
        op.has_guard = true;
        let spec = ParsedSpec {
            operations: vec![op],
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
        let mut op = make_op("deposit");
        op.has_effect = true;
        op.effects = vec![(
            "balance".to_string(),
            "add".to_string(),
            "amount".to_string(),
        )];
        op.has_guard = true;
        let spec = ParsedSpec {
            operations: vec![op],
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
        let mut op = make_op("transfer");
        op.has_calls = false;
        let spec = ParsedSpec {
            operations: vec![op],
            contexts: vec![ParsedContext {
                operation: "transfer".to_string(),
                accounts: vec![
                    ParsedAccountEntry {
                        name: "authority".to_string(),
                        account_type: "Signer".to_string(),
                        inner_type: None,
                        is_mut: false,
                        is_init: false,
                        is_init_if_needed: false,
                        payer: None,
                        seeds_ref: None,
                        has_bump: false,
                        close_target: None,
                        has_one: None,
                        token_mint: None,
                        token_authority: None,
                    },
                    ParsedAccountEntry {
                        name: "source".to_string(),
                        account_type: "Account".to_string(),
                        inner_type: Some("Token".to_string()),
                        is_mut: true,
                        is_init: false,
                        is_init_if_needed: false,
                        payer: None,
                        seeds_ref: None,
                        has_bump: false,
                        close_target: None,
                        has_one: None,
                        token_mint: None,
                        token_authority: None,
                    },
                    ParsedAccountEntry {
                        name: "dest".to_string(),
                        account_type: "Account".to_string(),
                        inner_type: Some("Token".to_string()),
                        is_mut: true,
                        is_init: false,
                        is_init_if_needed: false,
                        payer: None,
                        seeds_ref: None,
                        has_bump: false,
                        close_target: None,
                        has_one: None,
                        token_mint: None,
                        token_authority: None,
                    },
                    ParsedAccountEntry {
                        name: "token_program".to_string(),
                        account_type: "Program".to_string(),
                        inner_type: Some("Token".to_string()),
                        is_mut: false,
                        is_init: false,
                        is_init_if_needed: false,
                        payer: None,
                        seeds_ref: None,
                        has_bump: false,
                        close_target: None,
                        has_one: None,
                        token_mint: None,
                        token_authority: None,
                    },
                ],
            }],
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
        let mut op = make_op("initialize");
        op.pre_status = Some("Uninitialized".to_string());
        op.post_status = Some("Active".to_string());
        let spec = ParsedSpec {
            operations: vec![op],
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
        let mut init = make_op("initialize");
        init.pre_status = Some("Uninitialized".to_string());
        init.post_status = Some("Active".to_string());

        let mut close = make_op("close");
        close.pre_status = Some("Frozen".to_string());
        close.post_status = Some("Closed".to_string());

        let spec = ParsedSpec {
            operations: vec![init, close],
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
        let mut init = make_op("initialize");
        init.pre_status = Some("Uninitialized".to_string());
        init.post_status = Some("Active".to_string());

        let spec = ParsedSpec {
            operations: vec![init],
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
        let mut op = make_op("deposit");
        op.has_guard = true;
        op.guard_str = Some("amount > 0".to_string());
        let spec = ParsedSpec {
            operations: vec![op],
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
        let mut op = make_op("deposit");
        op.who = None; // priority 1: no_access_control
        op.takes_params = vec![("amount".to_string(), "U64".to_string())];
        op.has_takes = true;
        op.has_effect = true;
        op.effects = vec![(
            "balance".to_string(),
            "add".to_string(),
            "amount".to_string(),
        )];
        // no guard → priority 1: unguarded_arithmetic + missing_guard_from_takes
        // no properties → priority 3: no_properties
        let spec = ParsedSpec {
            operations: vec![op],
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
        let spec = crate::parser::parse(spec_content).expect("escrow.qedspec should parse");
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
}
