use anyhow::{Context, Result};
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

/// Parsed operation from a qedspec block with its clauses.
#[derive(Debug)]
pub struct ParsedOperation {
    pub name: String,
    pub doc: Option<String>,
    pub who: Option<String>,
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
}

/// Parsed property from a qedspec block.
#[derive(Debug)]
pub struct ParsedProperty {
    pub name: String,
    pub preserved_by: Vec<String>,
}

/// Full parsed spec context.
#[derive(Debug)]
pub struct ParsedSpec {
    pub operations: Vec<ParsedOperation>,
    pub invariants: Vec<(String, String)>, // (name, description)
    pub properties: Vec<ParsedProperty>,
    pub has_u64_fields: bool,
    pub u64_field_names: Vec<String>,
}

/// Check spec coverage: which properties have proofs, which have sorry, which are missing.
pub fn check(spec_path: &Path, proofs_dir: &Path) -> Result<Vec<PropertyStatus>> {
    let spec_content = std::fs::read_to_string(spec_path)
        .with_context(|| format!("reading {}", spec_path.display()))?;

    // Parse the spec for structure and intent
    let parsed = parse_spec(&spec_content);

    // Generate expected properties with intent annotations
    let properties = generate_properties(&parsed);

    if properties.is_empty() {
        eprintln!("No properties found in {}", spec_path.display());
        return Ok(vec![]);
    }

    // Collect all .lean files in the proofs directory (recursively)
    let mut proof_content = String::new();
    collect_lean_files(proofs_dir, &mut proof_content)?;

    // Also check the spec file itself (theorems might be proven inline)
    proof_content.push_str(&spec_content);

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

/// Parse a qedspec block to extract operations, invariants, and properties
/// with their clause details for intent generation.
pub fn parse_spec(content: &str) -> ParsedSpec {
    let mut operations = Vec::new();
    let mut invariants = Vec::new();
    let mut properties = Vec::new();
    let mut u64_field_names = Vec::new();

    // Detect U64 fields in state block
    let u64_re = Regex::new(r"(?m)^\s+(\w+)\s*:\s*U64").unwrap();
    for cap in u64_re.captures_iter(content) {
        u64_field_names.push(cap[1].to_string());
    }
    let has_u64_fields = !u64_field_names.is_empty();

    // Parse operations with their clauses
    // Split content into operation blocks by finding "operation <name>" lines
    let op_name_re = Regex::new(r"(?m)^\s*operation\s+(\w+)").unwrap();
    let block_boundary_re =
        Regex::new(r"(?m)^\s*(?:operation|invariant|property)\s").unwrap();

    let op_matches: Vec<_> = op_name_re
        .captures_iter(content)
        .map(|c| {
            let m = c.get(0).unwrap();
            (c[1].to_string(), m.start(), m.end())
        })
        .collect();

    for (idx, (name, start, end)) in op_matches.iter().enumerate() {
        // Block extends from this operation to the next operation/invariant/property (or EOF)
        let block_start = *start;
        let match_end = *end;
        let block_end = if idx + 1 < op_matches.len() {
            op_matches[idx + 1].1
        } else {
            // Search from match END to avoid matching this operation's own line
            block_boundary_re
                .find_at(content, match_end)
                .map(|m| m.start())
                .unwrap_or(content.len())
        };
        let block = &content[block_start..block_end];

        let doc = extract_clause_str(block, "doc");
        let who = extract_clause_ident(block, "who");
        let pre_status = extract_clause_ident(block, "when");
        let post_status = extract_clause_ident(block, "then");
        let has_when = pre_status.is_some();
        let has_calls = block.contains("calls:");
        let has_takes = block.contains("takes:");
        let has_guard = block.contains("guard:");
        let has_effect = block.contains("effect:");
        let guard_str = extract_clause_str(block, "guard");

        // Extract program ID from calls clause
        let program_re = Regex::new(r"calls:\s+(\w+)").unwrap();
        let program_id = program_re
            .captures(block)
            .map(|c| c[1].to_string());

        operations.push(ParsedOperation {
            name: name.clone(),
            doc,
            who,
            has_when,
            pre_status,
            post_status,
            has_calls,
            program_id,
            has_u64_fields: has_u64_fields,
            has_takes,
            has_guard,
            guard_str,
            has_effect,
        });
    }

    // Parse invariants
    let inv_re = Regex::new(r#"(?m)^\s*invariant\s+(\w+)\s+"([^"]+)""#).unwrap();
    for cap in inv_re.captures_iter(content) {
        invariants.push((cap[1].to_string(), cap[2].to_string()));
    }

    // Parse properties with preserved_by
    let prop_re =
        Regex::new(r#"(?ms)^\s*property\s+(\w+)\s+"[^"]*"\s*\n\s*preserved_by:\s*(.*?)$"#)
            .unwrap();
    for cap in prop_re.captures_iter(content) {
        let name = cap[1].to_string();
        let ops: Vec<String> = cap[2]
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        properties.push(ParsedProperty {
            name,
            preserved_by: ops,
        });
    }

    ParsedSpec {
        operations,
        invariants,
        properties,
        has_u64_fields,
        u64_field_names,
    }
}

/// Extract a string-valued clause: `clause: "value"`
fn extract_clause_str(block: &str, clause: &str) -> Option<String> {
    let re = Regex::new(&format!(r#"{}:\s+"([^"]+)""#, clause)).unwrap();
    re.captures(block).map(|c| c[1].to_string())
}

/// Extract an identifier-valued clause: `clause: ident`
fn extract_clause_ident(block: &str, clause: &str) -> Option<String> {
    let re = Regex::new(&format!(r"{}:\s+(\w+)", clause)).unwrap();
    re.captures(block).map(|c| c[1].to_string())
}

/// Generate the full list of expected properties with intent descriptions.
/// Returns (property_name, intent_description, optional_suggestion).
fn generate_properties(spec: &ParsedSpec) -> Vec<(String, String, Option<String>)> {
    let mut props = Vec::new();

    for op in &spec.operations {
        // Access control (only when who: specified)
        if let Some(ref signer) = op.who {
            let intent = if let Some(ref doc) = op.doc {
                format!("{} — signer must be {}", doc, signer)
            } else {
                format!("Only {} can call {}", signer, op.name)
            };
            let suggestion = Some(format!(
                "Prove that if {}Transition succeeds, the signer equals s.{}.",
                op.name, signer
            ));
            props.push((
                format!("{}.access_control", op.name),
                intent,
                suggestion,
            ));
        }

        // State machine (only when when:/then: specified)
        if op.has_when || op.post_status.is_some() {
            let pre = op.pre_status.as_deref().unwrap_or("?");
            let post = op.post_status.as_deref().unwrap_or("?");
            let intent = format!("{} transitions from {} to {}", op.name, pre, post);
            let suggestion = Some(format!(
                "Prove that if {}Transition maps s to s', then s.status = .{} and s'.status = .{}.",
                op.name, pre, post
            ));
            props.push((
                format!("{}.state_machine", op.name),
                intent,
                suggestion,
            ));
        }

        // CPI correctness (only when calls: specified)
        if op.has_calls {
            let program = op.program_id.as_deref().unwrap_or("?");
            let intent = format!(
                "{} CPI targets {} with correct accounts and discriminator",
                op.name, program
            );
            let suggestion = Some(format!(
                "The CPI builder is generated by the DSL — this should be provable by rfl/exact.",
            ));
            props.push((
                format!("{}.cpi_correct", op.name),
                intent,
                suggestion,
            ));
        }

        // U64 bounds (when spec has U64 fields)
        if spec.has_u64_fields {
            let fields = spec.u64_field_names.join(", ");
            let intent = format!(
                "All U64 fields ({}) remain in bounds after {}",
                fields, op.name
            );
            let mut hint = format!(
                "Prove that valid_u64 is preserved through {}Transition.",
                op.name
            );
            if op.has_effect {
                hint.push_str(" The operation has effects — check that guards prevent overflow/underflow.");
            }
            if op.has_guard {
                if let Some(ref g) = op.guard_str {
                    hint.push_str(&format!(" Guard: \"{}\".", g));
                }
            }
            let suggestion = Some(hint);
            props.push((
                format!("{}.u64_bounds", op.name),
                intent,
                suggestion,
            ));
        }
    }

    // Invariants
    for (name, desc) in &spec.invariants {
        let intent = format!("Invariant: {}", desc);
        let suggestion = Some(format!(
            "This invariant stub is generated as `True` by the DSL. \
             For a meaningful conservation proof, define the predicate and prove it \
             is preserved by all operations.",
        ));
        props.push((name.clone(), intent, suggestion));
    }

    // Properties with preservation scope
    for prop in &spec.properties {
        for op in &prop.preserved_by {
            let intent = format!("{} preserves {}", op, prop.name);
            let suggestion = Some(format!(
                "Prove that if {} holds before {}Transition, it still holds after.",
                prop.name, op
            ));
            props.push((
                format!("{}.preserves_{}", op, prop.name),
                intent,
                suggestion,
            ));
        }
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
    let quoted = leaf
        .splitn(2, '.')
        .collect::<Vec<_>>();
    let escaped_quoted = if quoted.len() == 2 {
        format!(r"«{}»\.{}", regex::escape(quoted[0]), regex::escape(quoted[1]))
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

    eprintln!("{} spec coverage ({}/{} proven):\n", spec_name, proven, total);
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
