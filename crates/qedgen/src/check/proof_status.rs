use super::*;
use anyhow::Result;
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

/// Check spec coverage: which properties have proofs, which have sorry, which are missing.
pub fn check(spec_path: &Path, proofs_dir: &Path) -> Result<Vec<PropertyStatus>> {
    let parsed = parse_spec_file(spec_path)?;

    let properties = generate_properties(&parsed);

    if properties.is_empty() {
        eprintln!("No properties found in {}", spec_path.display());
        return Ok(vec![]);
    }

    let mut proof_content = String::new();
    collect_lean_files(proofs_dir, &mut proof_content)?;

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

/// `qedgen check --explain`: render the verification-status report.
/// `--json` emits the data layer for the agent to render; without it the
/// CLI prints the inline Markdown human fallback. Written to `output`
/// when set, stdout otherwise. Moved out of the dispatch arm (T7c).
pub fn render_explain_report(
    spec_path: &Path,
    spec_name: &str,
    proofs_dir: &Path,
    json: bool,
    output: Option<&Path>,
) -> Result<()> {
    let results = check(spec_path, proofs_dir)?;
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

    let (rendered, what) = if json {
        let payload = serde_json::json!({
            "spec": spec_name,
            "summary": {
                "proven": proven,
                "sorry": sorry,
                "missing": missing,
                "total": total,
            },
            "properties": results,
        });
        (serde_json::to_string_pretty(&payload)?, "report (JSON)")
    } else {
        let mut md = format!("# {} Verification Report\n\n", spec_name);
        md.push_str(&format!(
            "**{}/{} properties verified** ({} sorry, {} missing)\n\n",
            proven, total, sorry, missing
        ));
        if proven == total {
            md.push_str("> All properties verified (sorry-free).\n\n");
        }
        md.push_str("## Properties\n\n");
        for r in &results {
            let (icon, label) = match r.status {
                Status::Proven => ("✓", "PROVEN"),
                Status::Sorry => ("✗", "SORRY"),
                Status::Missing => ("✗", "MISSING"),
            };
            md.push_str(&format!("### {} {} — {}\n\n", icon, r.name, label));
            if let Some(ref intent) = r.intent {
                md.push_str(&format!("**Intent:** {}\n\n", intent));
            }
            if r.status != Status::Proven {
                if let Some(ref suggestion) = r.suggestion {
                    md.push_str(&format!("**Suggestion:** {}\n\n", suggestion));
                }
            }
        }
        (md, "report")
    };

    if let Some(path) = output {
        std::fs::write(path, &rendered)?;
        eprintln!("Wrote verification {} to {}", what, path.display());
    } else {
        print!("{}", rendered);
    }
    Ok(())
}

/// Expected properties as (property_name, intent_description, optional_suggestion).
/// Works off unified `spec.handlers` across all target types.
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

    for inv in &spec.invariants {
        let name = &inv.name;
        let intent = match (&inv.lean_expr, inv.doc.is_empty()) {
            (Some(expr), _) => format!("Invariant: {}", expr),
            (None, false) => format!("Invariant: {}", inv.doc),
            (None, true) => format!("Invariant: {}", name),
        };
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
    // Property names use dots ("Initialize.rejects_wrong_data_len"); proofs
    // may use dots, underscores, «»-quoted names, or (hand-written) the bare
    // name without prefix.
    let leaf = property_name;
    let leaf_underscore = property_name.replace('.', "_");

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

    // Bare name without instruction prefix (hand-written proofs), plus
    // lowercase prefix forms: "Initialize.X" → "init_X" / "initialize_X".
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
