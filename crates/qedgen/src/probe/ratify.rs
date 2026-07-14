//! Scaffold-to-spec ratification — inverse of
//! `qedgen probe --emit-spec-candidates`. Reads the audit working set
//! (`interview.md`, `clusters.json`, `skeleton.qedspec`) and produces:
//!
//! - `<program>.qedspec` — skeleton with accepted clauses merged into
//!   handler bodies / top-level invariants.
//! - `.qed/plan/scoping.md` — rejected clusters with user rationale
//!   (captures non-fit decisions).
//! - `.qed/findings/scaffold-to-spec-<cluster_id>.md` — bug-flagged
//!   clusters (real missing-enforcement bug, not a spec clause).
//! - `domain-dossier.json` — accepted, rejected, bug, and deferred state
//!   persisted on the same stable structural candidate IDs.
//!
//! `clusters.json` (v3 envelope) is the source of truth for cluster
//! metadata; ratification only adds user choices on top. Audit directories
//! created before domain artifacts remain supported.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::cluster::{Cluster, ClusterScope};
use crate::prompts::{read_interview_file, Choice, Ratification};

/// Output destinations; `None`s fall back to convention-driven defaults
/// relative to the audit dir.
pub struct RatifyOpts {
    pub audit_dir: PathBuf,
    pub spec_out: Option<PathBuf>,
    pub scoping_out: Option<PathBuf>,
    pub findings_dir: Option<PathBuf>,
}

/// Result summary returned to the CLI for the digest line.
#[derive(Debug)]
pub struct RatifyReport {
    pub accepted: usize,
    pub narrowed: usize,
    pub rejected: usize,
    pub flagged_as_bug: usize,
    pub deferred: usize,
    pub spec_path: PathBuf,
    pub scoping_path: PathBuf,
    pub findings_paths: Vec<PathBuf>,
    /// Machine-readable three-layer handoff. Present when the audit working
    /// set contains a domain dossier.
    pub spec_handoff_path: Option<PathBuf>,
    /// Stateful coverage targets synthesized from ratified paired operations
    /// and lifecycle edges.
    pub domain_sequences_path: Option<PathBuf>,
}

pub fn run(opts: &RatifyOpts) -> Result<RatifyReport> {
    // ── Inputs ─────────────────────────────────────────────────────
    let interview_path = opts.audit_dir.join("interview.md");
    let clusters_path = opts.audit_dir.join("clusters.json");
    let skeleton_path = opts.audit_dir.join("skeleton.qedspec");
    for p in [&interview_path, &clusters_path, &skeleton_path] {
        if !p.exists() {
            bail!(
                "audit working-set file missing: {} — was the audit dir written by `qedgen probe --emit-spec-candidates --audit-dir <path>`?",
                p.display()
            );
        }
    }

    let ratifications = read_interview_file(&interview_path)?;
    let clusters: Vec<Cluster> = serde_json::from_str(&std::fs::read_to_string(&clusters_path)?)
        .with_context(|| format!("parsing clusters.json at {}", clusters_path.display()))?;
    let skeleton = std::fs::read_to_string(&skeleton_path)?;

    let cluster_by_id: BTreeMap<&str, &Cluster> =
        clusters.iter().map(|c| (c.id.as_str(), c)).collect();

    // ── Classify ratifications ─────────────────────────────────────
    let mut accepted_program: Vec<&Cluster> = Vec::new();
    let mut accepted_by_handler: BTreeMap<String, Vec<&Cluster>> = BTreeMap::new();
    let mut narrowed_by_handler: BTreeMap<String, Vec<&Cluster>> = BTreeMap::new();
    let mut rejected: Vec<(&Cluster, &Ratification)> = Vec::new();
    let mut bugs: Vec<(&Cluster, &Ratification)> = Vec::new();
    let mut deferred_count = 0usize;

    for r in &ratifications {
        let Some(choice) = r.choice else {
            deferred_count += 1;
            continue;
        };
        let Some(cluster) = cluster_by_id.get(r.cluster_id.as_str()) else {
            // Cluster id missing from clusters.json (user edit or ID
            // drift) — warn and treat as deferred.
            eprintln!(
                "warning: interview mentions cluster id {} not present in clusters.json — skipping",
                r.cluster_id
            );
            deferred_count += 1;
            continue;
        };
        match choice {
            Choice::Accept => match &cluster.scope {
                ClusterScope::Program => accepted_program.push(cluster),
                ClusterScope::Handler(h) => accepted_by_handler
                    .entry(h.clone())
                    .or_default()
                    .push(cluster),
            },
            Choice::Narrow => {
                // Program-scope narrow currently emits the same
                // suggested_syntax as accept (per-handler `writes_on_narrow`
                // templating awaits the AST round-trip). Handler-scope
                // narrow ≡ accept, but counted separately for the digest.
                match &cluster.scope {
                    ClusterScope::Program => {
                        accepted_program.push(cluster);
                    }
                    ClusterScope::Handler(h) => narrowed_by_handler
                        .entry(h.clone())
                        .or_default()
                        .push(cluster),
                }
            }
            Choice::Reject => rejected.push((cluster, r)),
            Choice::Bug => bugs.push((cluster, r)),
        }
    }

    // ── Emit spec ──────────────────────────────────────────────────
    let merged = merge_into_skeleton(
        &skeleton,
        &accepted_program,
        &accepted_by_handler,
        &narrowed_by_handler,
    );
    let spec_path = opts
        .spec_out
        .clone()
        .unwrap_or_else(|| default_spec_path(&opts.audit_dir));
    if let Some(parent) = spec_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&spec_path, merged)?;

    // ── Emit scoping notes ─────────────────────────────────────────
    let scoping_path = opts
        .scoping_out
        .clone()
        .unwrap_or_else(|| default_scoping_path(&opts.audit_dir));
    if !rejected.is_empty() {
        if let Some(parent) = scoping_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let scoping_block = render_scoping_block(&rejected);
        append_or_create(&scoping_path, &scoping_block)?;
    }

    // ── Emit bug findings ──────────────────────────────────────────
    let findings_dir = opts
        .findings_dir
        .clone()
        .unwrap_or_else(|| default_findings_dir(&opts.audit_dir));
    let mut findings_paths = Vec::new();
    if !bugs.is_empty() {
        std::fs::create_dir_all(&findings_dir)?;
        for (cluster, ratification) in &bugs {
            let path = findings_dir.join(format!("scaffold-to-spec-{}.md", cluster.id));
            let md = render_bug_finding(cluster, ratification);
            std::fs::write(&path, md)?;
            findings_paths.push(path);
        }
    }

    persist_domain_ratifications(&opts.audit_dir, &ratifications)?;
    persist_domain_interview_answers(&opts.audit_dir)?;
    let spec_handoff_path = write_spec_handoff(&opts.audit_dir, &spec_path)?;
    let domain_sequences_path = write_domain_sequences(&opts.audit_dir)?;

    Ok(RatifyReport {
        accepted: accepted_program.len()
            + accepted_by_handler.values().map(|v| v.len()).sum::<usize>(),
        narrowed: narrowed_by_handler.values().map(|v| v.len()).sum::<usize>(),
        rejected: rejected.len(),
        flagged_as_bug: bugs.len(),
        deferred: deferred_count,
        spec_path,
        scoping_path,
        findings_paths,
        spec_handoff_path,
        domain_sequences_path,
    })
}

fn write_domain_sequences(audit_dir: &Path) -> Result<Option<PathBuf>> {
    let dossier_path = audit_dir.join("domain-dossier.json");
    if !dossier_path.exists() {
        return Ok(None);
    }
    let dossier: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&dossier_path)
            .with_context(|| format!("reading {}", dossier_path.display()))?,
    )?;
    let sequences = super::domain_sequence::synthesize_domain_sequences(&dossier)?;
    let path = audit_dir.join("domain-sequences.json");
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&sequences)?),
    )?;
    let manifest_path = audit_dir.join("run-manifest.json");
    if manifest_path.exists() {
        let mut manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)?;
        manifest["artifacts"]["domain_sequences"] =
            serde_json::Value::String("domain-sequences.json".to_string());
        std::fs::write(
            &manifest_path,
            format!("{}\n", serde_json::to_string_pretty(&manifest)?),
        )?;
    }
    Ok(Some(path))
}

fn persist_domain_interview_answers(audit_dir: &Path) -> Result<()> {
    let interview_path = audit_dir.join("domain-interview.json");
    let dossier_path = audit_dir.join("domain-dossier.json");
    if !interview_path.exists() || !dossier_path.exists() {
        return Ok(());
    }
    let mut interview: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&interview_path)
            .with_context(|| format!("reading {}", interview_path.display()))?,
    )?;
    let answers: Vec<super::domain_interview::DomainAnswer> = serde_json::from_value(
        interview
            .get("answers")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .with_context(|| format!("parsing answers in {}", interview_path.display()))?;
    if answers.is_empty() {
        return Ok(());
    }
    let dossier: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&dossier_path)
            .with_context(|| format!("reading {}", dossier_path.display()))?,
    )?;
    let (updated, _) = super::domain_interview::apply_answers(&dossier, &answers)?;
    std::fs::write(
        &dossier_path,
        format!("{}\n", serde_json::to_string_pretty(&updated)?),
    )?;
    let applied = interview
        .get_mut("answers")
        .map(|answers| std::mem::replace(answers, serde_json::json!([])))
        .unwrap_or_else(|| serde_json::json!([]));
    let applied_items = applied.as_array().cloned().unwrap_or_default();
    let history = interview
        .as_object_mut()
        .ok_or_else(|| anyhow!("{} must contain a JSON object", interview_path.display()))?
        .entry("applied_answers")
        .or_insert_with(|| serde_json::json!([]));
    history
        .as_array_mut()
        .ok_or_else(|| {
            anyhow!(
                "applied_answers in {} must be an array",
                interview_path.display()
            )
        })?
        .extend(applied_items);
    std::fs::write(
        &interview_path,
        format!("{}\n", serde_json::to_string_pretty(&interview)?),
    )?;
    Ok(())
}

/// Materialize the audit-to-spec boundary without silently translating
/// domain prose into executable claims. Structural candidates accepted by the
/// existing interview are marked `emitted`; domain candidates remain
/// `needs_authoring` until a parser-valid clause is linked back to the same ID.
fn write_spec_handoff(audit_dir: &Path, spec_path: &Path) -> Result<Option<PathBuf>> {
    let dossier_path = audit_dir.join("domain-dossier.json");
    if !dossier_path.exists() {
        return Ok(None);
    }
    let dossier: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&dossier_path)
            .with_context(|| format!("reading {}", dossier_path.display()))?,
    )?;

    let structural = dossier
        .get("structural_candidates")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("ratification").and_then(serde_json::Value::as_str) == Some("user"))
        .map(|item| {
            serde_json::json!({
                "candidate_id": item.get("id").cloned().unwrap_or(serde_json::Value::Null),
                "disposition": "emitted",
                "spec_path": spec_path.display().to_string(),
                "verification_lanes": ["check"]
            })
        })
        .collect::<Vec<_>>();

    let mut domain = Vec::new();
    let mut language_gaps = Vec::new();
    for (collection, kind) in [
        ("asset_flows", "asset_flow"),
        ("quantities", "quantity"),
        ("paired_operations", "paired_operation"),
        ("lifecycle_edges", "lifecycle_edge"),
        ("authority_capabilities", "authority_capability"),
        ("economic_equations", "economic_equation"),
        ("external_assumptions", "external_assumption"),
    ] {
        for item in dossier
            .get(collection)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let ratification = item
                .pointer("/metadata/ratification")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("pending");
            if !matches!(ratification, "auto" | "user") {
                continue;
            }
            let id = item.get("id").cloned().unwrap_or(serde_json::Value::Null);
            domain.push(serde_json::json!({
                "candidate_id": id,
                "kind": kind,
                "disposition": "needs_authoring",
                "verification_lanes": item.pointer("/metadata/verification_lanes").cloned().unwrap_or_else(|| serde_json::json!([]))
            }));

            let gap = match kind {
                "quantity"
                    if item.get("unit").and_then(serde_json::Value::as_str) == Some("unknown")
                        || item.get("rounding").and_then(serde_json::Value::as_str)
                            == Some("unknown") =>
                {
                    Some("unit_or_rounding_semantics")
                }
                "economic_equation"
                    if item.get("unit_check").and_then(serde_json::Value::as_str)
                        != Some("compatible") =>
                {
                    Some("dimensional_consistency")
                }
                "external_assumption" => Some("external_environment_model"),
                _ => None,
            };
            if let Some(reason) = gap {
                language_gaps.push(serde_json::json!({
                    "candidate_id": item.get("id").cloned().unwrap_or(serde_json::Value::Null),
                    "reason": reason,
                    "disposition": "document_or_extend_language"
                }));
            }
        }
    }

    let handoff = serde_json::json!({
        "schema_version": 1,
        "schema_uri": "https://qedgen.dev/schemas/auditor/spec-handoff-v1.schema.json",
        "spec_path": spec_path.display().to_string(),
        "layers": {
            "structural": structural,
            "domain": domain,
            "regression": []
        },
        "language_gaps": language_gaps
    });
    let path = audit_dir.join("spec-handoff.json");
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&handoff)?),
    )?;

    let manifest_path = audit_dir.join("run-manifest.json");
    if manifest_path.exists() {
        let mut manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)?;
        manifest["artifacts"]["spec_handoff"] =
            serde_json::Value::String("spec-handoff.json".to_string());
        std::fs::write(
            &manifest_path,
            format!("{}\n", serde_json::to_string_pretty(&manifest)?),
        )?;
    }
    Ok(Some(path))
}

/// Keep the schema-v1 dossier as the canonical ratification record. Older
/// audit directories without domain artifacts remain supported.
fn persist_domain_ratifications(audit_dir: &Path, ratifications: &[Ratification]) -> Result<()> {
    let dossier_path = audit_dir.join("domain-dossier.json");
    if !dossier_path.exists() {
        return Ok(());
    }

    let mut dossier: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&dossier_path)
            .with_context(|| format!("reading {}", dossier_path.display()))?,
    )
    .with_context(|| format!("parsing {}", dossier_path.display()))?;
    let by_id: BTreeMap<&str, &Ratification> = ratifications
        .iter()
        .map(|ratification| (ratification.cluster_id.as_str(), ratification))
        .collect();

    let candidates = dossier
        .get_mut("structural_candidates")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| {
            anyhow!(
                "{} has no structural_candidates array",
                dossier_path.display()
            )
        })?;
    for candidate in candidates {
        let Some(id) = candidate.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(ratification) = by_id.get(id) else {
            continue;
        };
        let Some(choice) = ratification.choice else {
            continue;
        };
        let (state, default_rationale) = match choice {
            Choice::Accept | Choice::Narrow => ("user", None),
            Choice::Reject => (
                "rejected",
                Some("Rejected during scaffold-to-spec interview; no rationale captured."),
            ),
            Choice::Bug => (
                "bug",
                Some("Flagged as a missing-enforcement bug during scaffold-to-spec interview."),
            ),
        };
        candidate["ratification"] = serde_json::Value::String(state.to_string());
        candidate["rationale"] = if ratification.notes.trim().is_empty() {
            default_rationale
                .map(|text| serde_json::Value::String(text.to_string()))
                .unwrap_or(serde_json::Value::Null)
        } else {
            serde_json::Value::String(ratification.notes.trim().to_string())
        };
    }
    std::fs::write(
        &dossier_path,
        format!("{}\n", serde_json::to_string_pretty(&dossier)?),
    )?;

    let manifest_path = audit_dir.join("run-manifest.json");
    if manifest_path.exists() {
        let mut manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&manifest_path)
                .with_context(|| format!("reading {}", manifest_path.display()))?,
        )
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
        manifest["artifacts"]["ratified_intent"] =
            serde_json::Value::String("domain-dossier.json".to_string());
        std::fs::write(
            &manifest_path,
            format!("{}\n", serde_json::to_string_pretty(&manifest)?),
        )?;
    }
    Ok(())
}

// ── Spec emission ─────────────────────────────────────────────────────

/// Merge accepted clauses into the skeleton: program-scope clauses append
/// at the end; handler-scope clauses replace the matching handler's
/// `// filled by interview` placeholder line.
fn merge_into_skeleton(
    skeleton: &str,
    program_clauses: &[&Cluster],
    handler_clauses: &BTreeMap<String, Vec<&Cluster>>,
    narrowed_handler_clauses: &BTreeMap<String, Vec<&Cluster>>,
) -> String {
    use regex::Regex;
    let handler_open = Regex::new(r"^handler\s+(\w+)\s").expect("static regex");

    let mut out = String::new();
    let mut current_handler: Option<String> = None;
    let mut placeholder_seen_for_current = false;

    for line in skeleton.lines() {
        if let Some(caps) = handler_open.captures(line) {
            current_handler = Some(caps[1].to_string());
            placeholder_seen_for_current = false;
            out.push_str(line);
            out.push('\n');
            continue;
        }

        // Placeholder inside a handler = injection point. Two shapes:
        //   - Pinocchio skeleton: `// … — filled by interview`
        //   - Anchor (anchor_adapt::adapt): `// TODO: requires` (inject
        //     above it so clauses precede other body placeholders)
        let is_placeholder =
            line.contains("filled by interview") || line.trim() == "// TODO: requires";
        if is_placeholder && !placeholder_seen_for_current {
            placeholder_seen_for_current = true;
            if let Some(h) = &current_handler {
                let merged = collect_handler_clauses(h, handler_clauses, narrowed_handler_clauses);
                if !merged.is_empty() {
                    // Templates carry their own structure — stream
                    // suggested_syntax verbatim, no extra wrapping.
                    for c in &merged {
                        out.push_str(&format!("// provenance: domain-candidate {}\n", c.id));
                        out.push_str(&c.suggested_syntax);
                        if !c.suggested_syntax.ends_with('\n') {
                            out.push('\n');
                        }
                    }
                    // Injected clauses replace, not augment, the
                    // placeholder.
                    continue;
                }
            }
        }

        if current_handler.is_some() && line.trim() == "}" {
            current_handler = None;
        }

        out.push_str(line);
        out.push('\n');
    }

    if !program_clauses.is_empty() {
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str("// ── Ratified program-wide invariants ──────────────────────────\n");
        out.push_str("// Surfaced from the scaffold-to-spec interview. Each invariant\n");
        out.push_str("// below uses the description form (parser-valid). Refine bodies\n");
        out.push_str("// into expression form (`forall i : T, …`) as the spec matures.\n\n");
        for c in program_clauses {
            // Program-scope templates are parser-valid description-form
            // invariants — emit verbatim; the user refines into
            // expression form later.
            out.push_str(&format!("// provenance: domain-candidate {}\n", c.id));
            out.push_str(&c.suggested_syntax);
            if !c.suggested_syntax.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
    }

    out
}

fn collect_handler_clauses<'a>(
    handler: &str,
    accepted: &'a BTreeMap<String, Vec<&'a Cluster>>,
    narrowed: &'a BTreeMap<String, Vec<&'a Cluster>>,
) -> Vec<&'a Cluster> {
    let mut v = Vec::new();
    if let Some(cs) = accepted.get(handler) {
        v.extend(cs.iter().copied());
    }
    if let Some(cs) = narrowed.get(handler) {
        v.extend(cs.iter().copied());
    }
    v
}

// ── Scoping notes ─────────────────────────────────────────────────────

fn render_scoping_block(rejected: &[(&Cluster, &Ratification)]) -> String {
    let mut s = String::new();
    let now_iso = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Iso8601::DEFAULT)
        .unwrap_or_else(|_| "unknown".to_string());
    s.push_str(&format!(
        "\n## Rejected scaffold-to-spec clusters — {}\n\n",
        now_iso
    ));
    s.push_str(
        "Each entry records a proto-spec clause the user explicitly rejected during \
         the interview. The implicit precondition is real (the probe surfaced \
         it from real findings), but the user judged it does NOT apply / is an \
         over-claim / belongs elsewhere. Re-evaluate during future audits.\n\n",
    );
    for (cluster, r) in rejected {
        s.push_str(&format!(
            "### cluster `{}` — {} ({})\n\n",
            cluster.id,
            cluster.kind.as_str(),
            cluster.scope.as_key()
        ));
        s.push_str(&format!(
            "**Proto-clause:** {}\n\n",
            cluster.proto_clause_text
        ));
        s.push_str(&format!(
            "**Evidence:** {} finding(s)\n\n",
            cluster.evidence_count
        ));
        if r.notes.is_empty() {
            s.push_str("_No rationale captured._\n\n");
        } else {
            s.push_str("**User rationale:**\n\n");
            for line in r.notes.lines() {
                s.push_str(&format!("> {}\n", line));
            }
            s.push('\n');
        }
    }
    s
}

fn append_or_create(path: &Path, content: &str) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| anyhow!("opening {} for append: {}", path.display(), e))?;
    file.write_all(content.as_bytes())?;
    Ok(())
}

// ── Bug-flagged findings ──────────────────────────────────────────────

fn render_bug_finding(cluster: &Cluster, ratification: &Ratification) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "# Scaffold-to-spec bug flag: `{}`\n\n",
        cluster.id
    ));
    s.push_str(&format!("**Cluster kind:** `{}`\n", cluster.kind.as_str()));
    s.push_str(&format!("**Scope:** `{}`\n", cluster.scope.as_key()));
    s.push_str(&format!("**Confidence:** {:?}\n", cluster.confidence));
    s.push_str(&format!(
        "**Evidence:** {} finding(s)\n\n",
        cluster.evidence_count
    ));
    s.push_str(&format!(
        "**Proto-clause:** {}\n\n",
        cluster.proto_clause_text
    ));
    s.push_str(
        "The user flagged this cluster as a **bug** during the interview — \
         i.e., the implicit precondition is real, but the code does NOT \
         enforce it anywhere. This is a missing-check bug, not a spec gap.\n\n",
    );
    s.push_str("## Affected findings\n\n");
    for fid in &cluster.finding_ids {
        s.push_str(&format!("- `{}`\n", fid));
    }
    s.push('\n');
    if !ratification.notes.is_empty() {
        s.push_str("## User rationale\n\n");
        for line in ratification.notes.lines() {
            s.push_str(&format!("> {}\n", line));
        }
        s.push('\n');
    }
    s.push_str("## Suggested investigation\n\n");
    s.push_str(
        "Review the affected findings and verify the implicit precondition \
         is genuinely unchecked. If confirmed, file as a missing-enforcement \
         bug; the corresponding fix is to add the check upstream of every \
         finding's site.\n",
    );
    s
}

// ── Defaults ──────────────────────────────────────────────────────────

fn default_spec_path(audit_dir: &Path) -> PathBuf {
    // <project_root>/<project_name>.qedspec; `.qed/audit/<ts>/` → project
    // root is two levels up. Fall back to cwd if non-conforming.
    let project_root = audit_dir
        .parent()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf);
    let name = project_root
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("program")
        .to_string();
    let dir = project_root.unwrap_or_else(|| PathBuf::from("."));
    dir.join(format!("{}.qedspec", name))
}

fn default_scoping_path(audit_dir: &Path) -> PathBuf {
    audit_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join(".qed/plan/scoping.md"))
        .unwrap_or_else(|| PathBuf::from(".qed/plan/scoping.md"))
}

fn default_findings_dir(audit_dir: &Path) -> PathBuf {
    audit_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join(".qed/findings"))
        .unwrap_or_else(|| PathBuf::from(".qed/findings"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::{cluster_protos, ClusterKind, ProtoClause};
    use crate::pinocchio_to_spec::render_skeleton_from_handlers;
    use tempfile::tempdir;

    fn proto(kind: ClusterKind, handler: &str, fid: &str) -> ProtoClause {
        ProtoClause {
            kind,
            handler: handler.to_string(),
            finding_id: fid.to_string(),
            evidence_text: String::new(),
        }
    }

    fn write_audit_dir(
        dir: &Path,
        skeleton: &str,
        clusters: &[Cluster],
        ratifications_md: &str,
    ) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join("skeleton.qedspec"), skeleton)?;
        std::fs::write(
            dir.join("clusters.json"),
            serde_json::to_string_pretty(clusters)?,
        )?;
        std::fs::write(dir.join("interview.md"), ratifications_md)?;
        Ok(())
    }

    fn build_test_clusters() -> Vec<Cluster> {
        let protos: Vec<_> = (1..=4)
            .map(|i| {
                proto(
                    ClusterKind::AccountOwnerCheck,
                    &format!("h{}", i),
                    &format!("f{}", i),
                )
            })
            .collect();
        cluster_protos(protos)
    }

    #[test]
    fn ratify_accept_emits_program_invariant_into_spec() -> Result<()> {
        let dir = tempdir()?;
        let audit = dir.path().join(".qed/audit/test");
        let clusters = build_test_clusters();
        let cid = clusters[0].id.clone();
        let interview = format!("<!-- cluster: {} -->\n- [x] **accept** — emit\n", cid);
        let skeleton = render_skeleton_from_handlers(
            &["process_transfer".to_string(), "process_burn".to_string()],
            "ptoken",
        );
        write_audit_dir(&audit, &skeleton, &clusters, &interview)?;

        let opts = RatifyOpts {
            audit_dir: audit,
            spec_out: Some(dir.path().join("ptoken.qedspec")),
            scoping_out: Some(dir.path().join(".qed/plan/scoping.md")),
            findings_dir: Some(dir.path().join(".qed/findings")),
        };
        let report = run(&opts)?;
        assert_eq!(report.accepted, 1);
        let spec = std::fs::read_to_string(&report.spec_path)?;
        assert!(
            spec.contains("Ratified program-wide invariants"),
            "expected ratified-invariants block in {}",
            spec
        );
        assert!(spec.contains("owner_locked_writes"), "spec: {}", spec);
        Ok(())
    }

    #[test]
    fn ratify_reject_writes_to_scoping_with_notes() -> Result<()> {
        let dir = tempdir()?;
        let audit = dir.path().join(".qed/audit/test");
        let clusters = build_test_clusters();
        let cid = clusters[0].id.clone();
        let interview = format!(
            "<!-- cluster: {} -->\n- [x] **reject**\n\n_notes:_\n\nover-claim, only mints need this\n",
            cid
        );
        let skeleton = render_skeleton_from_handlers(&["process_burn".into()], "p");
        write_audit_dir(&audit, &skeleton, &clusters, &interview)?;
        std::fs::write(
            audit.join("domain-dossier.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "structural_candidates": [{
                    "id": cid,
                    "ratification": "pending",
                    "rationale": null
                }]
            }))?,
        )?;
        std::fs::write(
            audit.join("run-manifest.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "artifacts": { "ratified_intent": null }
            }))?,
        )?;

        let scoping_path = dir.path().join(".qed/plan/scoping.md");
        let opts = RatifyOpts {
            audit_dir: audit.clone(),
            spec_out: Some(dir.path().join("p.qedspec")),
            scoping_out: Some(scoping_path.clone()),
            findings_dir: Some(dir.path().join(".qed/findings")),
        };
        let report = run(&opts)?;
        assert_eq!(report.rejected, 1);
        let scoping = std::fs::read_to_string(&scoping_path)?;
        assert!(scoping.contains("Rejected scaffold-to-spec"));
        assert!(scoping.contains("over-claim, only mints need this"));
        let dossier: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(audit.join("domain-dossier.json"))?)?;
        assert_eq!(
            dossier["structural_candidates"][0]["ratification"],
            "rejected"
        );
        assert_eq!(
            dossier["structural_candidates"][0]["rationale"],
            "over-claim, only mints need this"
        );
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(audit.join("run-manifest.json"))?)?;
        assert_eq!(
            manifest["artifacts"]["ratified_intent"],
            "domain-dossier.json"
        );
        Ok(())
    }

    #[test]
    fn ratify_applies_domain_answers_and_writes_three_layer_handoff() -> Result<()> {
        let dir = tempdir()?;
        let audit = dir.path().join(".qed/audit/test");
        let clusters = build_test_clusters();
        let cid = clusters[0].id.clone();
        let interview = format!("<!-- cluster: {} -->\n- [x] **accept**\n", cid);
        let skeleton = render_skeleton_from_handlers(&["deposit".into()], "vault");
        write_audit_dir(&audit, &skeleton, &clusters, &interview)?;
        std::fs::write(
            audit.join("domain-dossier.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "structural_candidates": [{ "id": cid, "ratification": "pending", "rationale": null }],
                "handlers": [
                    { "name": "deposit", "accounts_type": "Deposit", "args": [{ "name": "amount", "qedspec_type": "U64" }] },
                    { "name": "withdraw", "accounts_type": "Withdraw", "args": [{ "name": "shares", "qedspec_type": "U64" }] }
                ],
                "asset_flows": [],
                "quantities": [{
                    "id": "qty_fee",
                    "unit": "unknown",
                    "rounding": "unknown",
                    "metadata": { "ratification": "pending", "verification_lanes": ["manual", "crucible"] }
                }],
                "paired_operations": [{
                    "id": "pair_deposit_withdraw",
                    "left_handlers": ["deposit"],
                    "right_handlers": ["withdraw"],
                    "metadata": { "ratification": "user" }
                }],
                "lifecycle_edges": [],
                "authority_capabilities": [],
                "economic_equations": [],
                "external_assumptions": []
            }))?,
        )?;
        std::fs::write(
            audit.join("domain-interview.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 1,
                "questions": [],
                "answers": [{ "candidate_id": "qty_fee", "decision": "confirm", "rationale": "fees are token-denominated" }]
            }))?,
        )?;

        let report = run(&RatifyOpts {
            audit_dir: audit.clone(),
            spec_out: Some(dir.path().join("vault.qedspec")),
            scoping_out: None,
            findings_dir: None,
        })?;
        let dossier: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(audit.join("domain-dossier.json"))?)?;
        assert_eq!(dossier["quantities"][0]["metadata"]["ratification"], "user");
        let handoff_path = report.spec_handoff_path.expect("handoff path");
        let first_handoff = std::fs::read_to_string(handoff_path)?;
        let handoff: serde_json::Value = serde_json::from_str(&first_handoff)?;
        assert_eq!(handoff["layers"]["structural"][0]["disposition"], "emitted");
        assert_eq!(handoff["layers"]["domain"][0]["candidate_id"], "qty_fee");
        assert_eq!(
            handoff["layers"]["domain"][0]["disposition"],
            "needs_authoring"
        );
        assert_eq!(
            handoff["language_gaps"][0]["reason"],
            "unit_or_rounding_semantics"
        );
        let spec = std::fs::read_to_string(&report.spec_path)?;
        assert!(spec.contains(&format!("provenance: domain-candidate {}", cid)));
        let sequences_path = report.domain_sequences_path.expect("domain sequences");
        let sequences: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(sequences_path)?)?;
        assert_eq!(sequences["plans"][0]["kind"], "paired_round_trip");
        assert_eq!(sequences["plans"][0]["forward"][0]["handler"], "deposit");
        assert_eq!(sequences["plans"][0]["reverse"][0]["handler"], "withdraw");
        assert!(!sequences["plans"][0]["unresolved_parameters"]
            .as_array()
            .unwrap()
            .is_empty());
        let rerun = run(&RatifyOpts {
            audit_dir: audit.clone(),
            spec_out: Some(dir.path().join("vault.qedspec")),
            scoping_out: None,
            findings_dir: None,
        })?;
        assert_eq!(
            std::fs::read_to_string(rerun.spec_handoff_path.expect("rerun handoff"))?,
            first_handoff
        );
        Ok(())
    }

    #[test]
    fn ratify_bug_writes_finding_file() -> Result<()> {
        let dir = tempdir()?;
        let audit = dir.path().join(".qed/audit/test");
        let clusters = build_test_clusters();
        let cid = clusters[0].id.clone();
        let interview = format!(
            "<!-- cluster: {} -->\n- [x] **bug**\n\n_notes:_\n\nnot enforced anywhere in batch path\n",
            cid
        );
        let skeleton = render_skeleton_from_handlers(&["process_batch".into()], "p");
        write_audit_dir(&audit, &skeleton, &clusters, &interview)?;

        let findings_dir = dir.path().join(".qed/findings");
        let opts = RatifyOpts {
            audit_dir: audit,
            spec_out: Some(dir.path().join("p.qedspec")),
            scoping_out: Some(dir.path().join(".qed/plan/scoping.md")),
            findings_dir: Some(findings_dir.clone()),
        };
        let report = run(&opts)?;
        assert_eq!(report.flagged_as_bug, 1);
        assert_eq!(report.findings_paths.len(), 1);
        let body = std::fs::read_to_string(&report.findings_paths[0])?;
        assert!(body.contains("Scaffold-to-spec bug flag"));
        assert!(body.contains("not enforced anywhere"));
        Ok(())
    }

    #[test]
    fn ratify_handler_scope_clause_injects_into_handler_body() -> Result<()> {
        let dir = tempdir()?;
        let audit = dir.path().join(".qed/audit/test");
        // Single handler-scope cluster (only one handler contributes — no
        // Program-scope promotion).
        let protos = vec![proto(
            ClusterKind::AccountInitCheck,
            "process_transfer",
            "f1",
        )];
        let clusters = cluster_protos(protos);
        let cid = clusters[0].id.clone();
        let interview = format!("<!-- cluster: {} -->\n- [x] **accept**\n", cid);
        let skeleton = render_skeleton_from_handlers(&["process_transfer".into()], "p");
        write_audit_dir(&audit, &skeleton, &clusters, &interview)?;

        let opts = RatifyOpts {
            audit_dir: audit,
            spec_out: Some(dir.path().join("p.qedspec")),
            scoping_out: Some(dir.path().join("scoping.md")),
            findings_dir: Some(dir.path().join("findings")),
        };
        let report = run(&opts)?;
        assert_eq!(report.accepted, 1);
        let spec = std::fs::read_to_string(&report.spec_path)?;
        // The placeholder is dropped; the injected clause appears in the
        // handler body.
        assert!(
            !spec.contains("// accounts, requires, effect, transfers — filled by interview"),
            "placeholder should be replaced. spec:\n{}",
            spec
        );
        // Handler-scope ratifications emit `// TODO ratified (...)`
        // markers (parser-irrelevant) carrying the kind + target form.
        assert!(
            spec.contains("TODO ratified (account_init_check"),
            "expected `// TODO ratified (account_init_check …)` marker. spec:\n{}",
            spec
        );
        assert!(
            spec.contains("Target form:"),
            "expected `// Target form: …` line pointing at the parseable shape. spec:\n{}",
            spec
        );
        Ok(())
    }

    #[test]
    fn ratify_unmatched_cluster_id_does_not_fail() -> Result<()> {
        let dir = tempdir()?;
        let audit = dir.path().join(".qed/audit/test");
        let clusters = build_test_clusters();
        // Interview references a cluster id that doesn't exist in
        // clusters.json — e.g., from a manual edit.
        let interview = "<!-- cluster: c-bogus-fake -->\n- [x] **accept**\n";
        let skeleton = render_skeleton_from_handlers(&[], "p");
        write_audit_dir(&audit, &skeleton, &clusters, interview)?;

        let opts = RatifyOpts {
            audit_dir: audit,
            spec_out: Some(dir.path().join("p.qedspec")),
            scoping_out: Some(dir.path().join("scoping.md")),
            findings_dir: Some(dir.path().join("findings")),
        };
        let report = run(&opts)?;
        assert_eq!(report.accepted, 0);
        // Unmatched cluster surfaces as deferred (so the user notices in
        // the digest line) but doesn't abort the run.
        assert_eq!(report.deferred, 1);
        Ok(())
    }

    /// Every spec ratify emits must parse cleanly: synthetic audit-dir
    /// per cluster kind × scope, run ratify, parse the result.
    #[test]
    fn every_ratified_spec_parses() -> Result<()> {
        use crate::cluster::{ClusterKind, ProtoClause};
        let all_kinds = [
            ClusterKind::AccountOwnerCheck,
            ClusterKind::AccountInitCheck,
            ClusterKind::AccountSignerCheck,
            ClusterKind::AccountTypeTagCheck,
            ClusterKind::AccountDistinct,
            ClusterKind::ArithmeticNoOverflow,
            ClusterKind::ArithmeticBoundPre,
            ClusterKind::PdaCanonicalDerivation,
            ClusterKind::PdaSeedUniqueness,
            ClusterKind::LifecycleOneShot,
            ClusterKind::LifecycleMonotonic,
            ClusterKind::CpiProgramPin,
            ClusterKind::CpiAccountDirection,
            ClusterKind::DispatchCallerEstablishesCalleeRequires,
        ];
        for kind in all_kinds {
            for scope_variant in 0..2 {
                let dir = tempdir()?;
                let audit = dir.path().join(".qed/audit/test");

                // Build a proto-clause set that exercises this kind at
                // either Program scope (≥3 handlers) or Handler scope
                // (single handler).
                let protos: Vec<_> = if scope_variant == 0 {
                    (1..=4)
                        .map(|i| ProtoClause {
                            kind,
                            handler: format!("h{}", i),
                            finding_id: format!("f{}", i),
                            evidence_text: String::new(),
                        })
                        .collect()
                } else {
                    vec![ProtoClause {
                        kind,
                        handler: "process_test".into(),
                        finding_id: "f1".into(),
                        evidence_text: String::new(),
                    }]
                };
                let clusters = cluster_protos(protos);
                let cid = clusters[0].id.clone();
                let interview = format!("<!-- cluster: {} -->\n- [x] **accept**\n", cid);
                let skeleton = render_skeleton_from_handlers(
                    &[
                        "process_test".to_string(),
                        "h1".into(),
                        "h2".into(),
                        "h3".into(),
                    ],
                    "test_prog",
                );
                write_audit_dir(&audit, &skeleton, &clusters, &interview)?;
                let out = dir.path().join("test.qedspec");
                let opts = RatifyOpts {
                    audit_dir: audit,
                    spec_out: Some(out.clone()),
                    scoping_out: Some(dir.path().join("scoping.md")),
                    findings_dir: Some(dir.path().join("findings")),
                };
                run(&opts)?;
                let content = std::fs::read_to_string(&out)?;
                crate::chumsky_adapter::parse_str(&content).unwrap_or_else(|e| {
                    panic!(
                        "kind={:?} scope_variant={} produced unparseable spec: {}\n\n=== emitted ===\n{}",
                        kind, scope_variant, e, content
                    );
                });
            }
        }
        Ok(())
    }

    /// Re-running ratify on the same audit dir must be byte-identical —
    /// otherwise user edits between runs silently regress on the next
    /// interview pass.
    #[test]
    fn ratify_is_byte_idempotent() -> Result<()> {
        use crate::cluster::{ClusterKind, ProtoClause};
        let dir = tempdir()?;
        let audit = dir.path().join(".qed/audit/test");
        let protos: Vec<_> = (1..=4)
            .map(|i| ProtoClause {
                kind: ClusterKind::AccountOwnerCheck,
                handler: format!("h{}", i),
                finding_id: format!("f{}", i),
                evidence_text: String::new(),
            })
            .collect();
        let clusters = cluster_protos(protos);
        let cid = clusters[0].id.clone();
        let interview = format!("<!-- cluster: {} -->\n- [x] **accept**\n", cid);
        let skeleton =
            render_skeleton_from_handlers(&["h1".into(), "h2".into(), "h3".into()], "test_prog");
        write_audit_dir(&audit, &skeleton, &clusters, &interview)?;

        let out1 = dir.path().join("run1.qedspec");
        let out2 = dir.path().join("run2.qedspec");
        let make_opts = |out: PathBuf| RatifyOpts {
            audit_dir: audit.clone(),
            spec_out: Some(out),
            scoping_out: Some(dir.path().join("scoping.md")),
            findings_dir: Some(dir.path().join("findings")),
        };
        run(&make_opts(out1.clone()))?;
        run(&make_opts(out2.clone()))?;
        let bytes1 = std::fs::read(&out1)?;
        let bytes2 = std::fs::read(&out2)?;
        assert_eq!(
            bytes1, bytes2,
            "re-running ratify on identical audit_dir must produce identical spec bytes"
        );
        Ok(())
    }

    #[test]
    fn ratify_missing_audit_dir_files_errors_clearly() {
        let dir = tempdir().unwrap();
        let audit = dir.path().join(".qed/audit/empty");
        std::fs::create_dir_all(&audit).unwrap();
        let opts = RatifyOpts {
            audit_dir: audit,
            spec_out: None,
            scoping_out: None,
            findings_dir: None,
        };
        let err = run(&opts).expect_err("missing files should error");
        let msg = format!("{}", err);
        assert!(
            msg.contains("audit working-set file missing"),
            "got: {}",
            msg
        );
    }
}
