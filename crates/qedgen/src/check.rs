use anyhow::{Context, Result};
use regex::Regex;
use std::path::Path;

#[derive(Debug)]
pub struct PropertyStatus {
    pub name: String,
    pub status: Status,
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

/// Check spec coverage: which properties have proofs, which have sorry, which are missing.
pub fn check(spec_path: &Path, proofs_dir: &Path) -> Result<Vec<PropertyStatus>> {
    let spec_content = std::fs::read_to_string(spec_path)
        .with_context(|| format!("reading {}", spec_path.display()))?;

    // Extract theorem names declared by qedspec macro or manual theorems
    let properties = extract_spec_properties(&spec_content);

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
        .map(|name| {
            let status = check_property_status(&name, &proof_content);
            PropertyStatus { name, status }
        })
        .collect();

    Ok(results)
}

/// Extract property names from a spec file.
/// Looks for:
/// 1. qedspec-generated theorems (from operation/invariant declarations)
/// 2. Manual `theorem <name>` declarations
fn extract_spec_properties(content: &str) -> Vec<String> {
    let mut properties = Vec::new();

    // Extract from qedspec blocks: operation names generate .access_control and .state_machine
    let op_re = Regex::new(r"(?m)^\s*operation\s+(\w+)").unwrap();
    for cap in op_re.captures_iter(content) {
        let op_name = &cap[1];
        properties.push(format!("{}.access_control", op_name));
        properties.push(format!("{}.state_machine", op_name));
    }

    // Extract invariant names from qedspec blocks
    let inv_re = Regex::new(r#"(?m)^\s*invariant\s+(\w+)\s+"#).unwrap();
    for cap in inv_re.captures_iter(content) {
        properties.push(cap[1].to_string());
    }

    // Extract manual theorem declarations (outside qedspec blocks)
    let thm_re = Regex::new(r"(?m)^theorem\s+(\w+)").unwrap();
    for cap in thm_re.captures_iter(content) {
        let name = cap[1].to_string();
        if !properties.contains(&name) {
            properties.push(name);
        }
    }

    properties
}

/// Check whether a property is proven, sorry, or missing in the proof content.
fn check_property_status(property_name: &str, proof_content: &str) -> Status {
    // Look for `theorem <name>` or `<namespace>.<name>` pattern
    // The property could be namespaced (e.g., Escrow.cancel.access_control)
    // or just the leaf name (cancel.access_control)
    let leaf = property_name;

    // Find the theorem declaration and extract its body up to the next top-level declaration
    let escaped = regex::escape(leaf);
    let theorem_pattern = format!(r"theorem\s+\S*{}", escaped);
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

/// Print a formatted coverage report.
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

    eprintln!("{} spec coverage:", spec_name);
    for r in results {
        let icon = match r.status {
            Status::Proven => "  ✓",
            Status::Sorry => "  ✗",
            Status::Missing => "  ✗",
        };
        eprintln!("  {} {:<40} {}", icon, r.name, r.status);
    }
    eprintln!();
    eprintln!(
        "{}/{} proven, {} sorry, {} missing",
        proven, total, sorry, missing
    );

    if proven == total {
        eprintln!("All properties verified.");
    }
}
