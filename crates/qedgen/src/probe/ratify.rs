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
use crate::probe::elicit::{self, HypothesisOutcome, LoweringResult};
use crate::probe::hypothesize::InvariantHypothesis;
use crate::prompts::{read_interview_file, Choice, Ratification};

/// Output destinations; `None`s fall back to convention-driven defaults
/// relative to the audit dir.
pub struct RatifyOpts {
    pub audit_dir: PathBuf,
    pub spec_out: Option<PathBuf>,
    pub scoping_out: Option<PathBuf>,
    pub findings_dir: Option<PathBuf>,
    /// Structured answer set (`answers.json`); defaults to
    /// `<audit_dir>/answers.json` when present. When resolved, the legacy
    /// user-edited `interview.md` is not consulted.
    pub answers: Option<PathBuf>,
    /// Also generate the spec-model proptest harness from the ratified
    /// spec (`model-proptest.rs` in the audit dir). Generation proves the
    /// spec lowers; *running* the harness is what earns `model-tested`.
    pub proptest: bool,
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
    /// Spec elicitation: run identity carried from the probe (Phase 0).
    pub run_id: Option<String>,
    /// Confirmed hypotheses lowered to executable clauses (incl. claims
    /// the spec already modeled).
    pub hypotheses_lowered: usize,
    /// Confirmed hypotheses with no placeholder-free lowering — kept in
    /// the dossier, reported honestly, never inserted as comments.
    pub hypotheses_not_executable: usize,
    /// Error-severity completeness lints on the final spec (parse success
    /// is a hard gate; error lints are surfaced, not fatal — the skeleton
    /// may carry pre-existing ones).
    pub check_errors: usize,
    pub check_warnings: usize,
    /// Path of the generated spec-model proptest harness (`--proptest`).
    pub model_proptest_path: Option<PathBuf>,
}

pub fn run(opts: &RatifyOpts) -> Result<RatifyReport> {
    // ── Inputs ─────────────────────────────────────────────────────
    let interview_path = opts.audit_dir.join("interview.md");
    let clusters_path = opts.audit_dir.join("clusters.json");
    let skeleton_path = opts.audit_dir.join("skeleton.qedspec");
    let answers_path = opts
        .answers
        .clone()
        .or_else(|| {
            let conventional = opts.audit_dir.join("answers.json");
            conventional.exists().then_some(conventional)
        })
        .filter(|p| p.exists());
    let structured = answers_path.is_some();

    // skeleton.qedspec is the merge base and always required.
    // clusters.json is optional for a hypotheses-only working set (#248:
    // the bootstrap lane materializes hypotheses without proto-clause
    // clusters) — ratify then proceeds on the hypothesis answers alone.
    let hypotheses_path = opts.audit_dir.join("hypotheses.json");
    let hypotheses_only = !clusters_path.exists() && hypotheses_path.exists();
    let mut required: Vec<&Path> = vec![&skeleton_path];
    if !hypotheses_only {
        required.push(&clusters_path);
    }
    if !structured {
        // Legacy path only — the structured answer set replaces the
        // user-edited interview file (PRD D4: in-harness, no file).
        required.push(&interview_path);
    }
    for p in required {
        if !p.exists() {
            bail!(
                "audit working-set file missing: {} — was the audit dir written by `qedgen probe --emit-spec-candidates --audit-dir <path>`?",
                p.display()
            );
        }
    }

    // Hypotheses (present for working sets written after spec elicitation
    // landed; absent dirs stay supported).
    let hypotheses_doc = hypotheses_path
        .exists()
        .then(|| elicit::read_hypotheses(&hypotheses_path))
        .transpose()?;
    let hypothesis_by_id: BTreeMap<&str, &InvariantHypothesis> = hypotheses_doc
        .as_ref()
        .map(|doc| doc.hypotheses.iter().map(|h| (h.id.as_str(), h)).collect())
        .unwrap_or_default();

    // Answers: structured set → split into cluster ratifications +
    // hypothesis answers; legacy → parse interview.md, no hypotheses.
    let mut hypothesis_answers: Vec<(String, Choice, String)> = Vec::new();
    let mut answers_run_id: Option<String> = None;
    let ratifications: Vec<Ratification> = if let Some(path) = &answers_path {
        let set = elicit::read_answers(path)?;
        answers_run_id = set.run_id.clone();
        let mut cluster_answers = Vec::new();
        for a in &set.answers {
            let choice = a.choice();
            if a.id.starts_with("h-") {
                if let Some(c) = choice {
                    hypothesis_answers.push((a.id.clone(), c, a.note.clone()));
                } else {
                    eprintln!(
                        "warning: answers.json decision `{}` on {} is not accept/narrow/reject/bug — deferring",
                        a.decision, a.id
                    );
                }
            } else {
                cluster_answers.push(Ratification {
                    cluster_id: a.id.clone(),
                    choice,
                    notes: a.note.clone(),
                });
            }
        }
        cluster_answers
    } else {
        read_interview_file(&interview_path)?
    };

    let clusters: Vec<Cluster> = if hypotheses_only {
        Vec::new()
    } else {
        serde_json::from_str(&std::fs::read_to_string(&clusters_path)?)
            .with_context(|| format!("parsing clusters.json at {}", clusters_path.display()))?
    };
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
    let mut merged = merge_into_skeleton(
        &skeleton,
        &accepted_program,
        &accepted_by_handler,
        &narrowed_by_handler,
    );

    // ── Lower confirmed hypotheses (PRD §6.3) ──────────────────────
    // User confirmation, not detector confidence, activates a clause.
    // Each lowering is committed only if the candidate text still parses
    // and introduces no new Error-severity completeness lints; otherwise
    // the hypothesis stays `confirmed, not executable`.
    let baseline_errors = elicit::validate_spec_text(&merged).ok();
    let mut hypothesis_outcomes: Vec<HypothesisOutcome> = Vec::new();
    let mut hyp_rejected: Vec<(&InvariantHypothesis, String)> = Vec::new();
    let mut hyp_bugs: Vec<(&InvariantHypothesis, String)> = Vec::new();
    // State-ADT rewrites must land before transition lowerings so
    // lifecycle edges resolve against the real variants (stable sort —
    // answer order is otherwise preserved).
    hypothesis_answers.sort_by_key(|(id, _, _)| {
        let is_state_rewrite = hypothesis_by_id
            .get(id.as_str())
            .map(|h| {
                matches!(
                    h.lowering,
                    Some(crate::probe::hypothesize::HypothesisLowering::StateAdtRewrite { .. })
                )
            })
            .unwrap_or(false);
        !is_state_rewrite
    });
    for (id, choice, note) in &hypothesis_answers {
        let Some(hyp) = hypothesis_by_id.get(id.as_str()).copied() else {
            eprintln!(
                "warning: answers.json references hypothesis {} not present in hypotheses.json — skipping",
                id
            );
            deferred_count += 1;
            continue;
        };
        match choice {
            Choice::Accept | Choice::Narrow
                if hyp.class == crate::probe::hypothesize::HypothesisClass::UnwiredGuard =>
            {
                // §6.1's sixth class: accepting an unwired guard means
                // "the check is intended AND missing" — a
                // missing-enforcement finding, not a spec clause.
                let note = if note.trim().is_empty() {
                    "Confirmed during elicitation: the guard this variant names is \
                     intended but enforced nowhere."
                        .to_string()
                } else {
                    note.clone()
                };
                hyp_bugs.push((hyp, note));
                hypothesis_outcomes.push(HypothesisOutcome {
                    id: hyp.id.clone(),
                    handler: hyp.handler.clone(),
                    decision: "accept".to_string(),
                    outcome: "confirmed_unenforced".to_string(),
                    detail: None,
                });
            }
            Choice::Accept | Choice::Narrow => {
                let (outcome, detail) = match &hyp.lowering {
                    None => (
                        "confirmed_not_executable".to_string(),
                        Some("no mechanical lowering for this claim yet".to_string()),
                    ),
                    Some(_) if baseline_errors.is_none() => (
                        "confirmed_not_executable".to_string(),
                        Some(
                            "skeleton spec does not parse; nothing can be lowered against it"
                                .to_string(),
                        ),
                    ),
                    Some(lowering) => match elicit::apply_lowering(&merged, hyp, lowering) {
                        LoweringResult::Applied(candidate) => {
                            match elicit::validate_spec_text(&candidate) {
                                Ok(errors) if errors <= baseline_errors.unwrap_or(0) => {
                                    merged = candidate;
                                    ("lowered".to_string(), None)
                                }
                                Ok(errors) => (
                                    "confirmed_not_executable".to_string(),
                                    Some(format!(
                                        "lowering introduced {} new error lint(s); reverted",
                                        errors - baseline_errors.unwrap_or(0)
                                    )),
                                ),
                                Err(e) => (
                                    "confirmed_not_executable".to_string(),
                                    Some(format!("lowered text failed to parse: {e}")),
                                ),
                            }
                        }
                        LoweringResult::AlreadyModeled => ("already_modeled".to_string(), None),
                        LoweringResult::NotExecutable(reason) => {
                            ("confirmed_not_executable".to_string(), Some(reason))
                        }
                    },
                };
                if outcome == "confirmed_not_executable" {
                    eprintln!(
                        "hypothesis {} confirmed, not executable: {}",
                        hyp.id,
                        detail.as_deref().unwrap_or("no lowering")
                    );
                }
                hypothesis_outcomes.push(HypothesisOutcome {
                    id: hyp.id.clone(),
                    handler: hyp.handler.clone(),
                    decision: "accept".to_string(),
                    outcome,
                    detail,
                });
            }
            Choice::Reject => {
                hyp_rejected.push((hyp, note.clone()));
                hypothesis_outcomes.push(HypothesisOutcome {
                    id: hyp.id.clone(),
                    handler: hyp.handler.clone(),
                    decision: "reject".to_string(),
                    outcome: "rejected".to_string(),
                    detail: None,
                });
            }
            Choice::Bug => {
                hyp_bugs.push((hyp, note.clone()));
                hypothesis_outcomes.push(HypothesisOutcome {
                    id: hyp.id.clone(),
                    handler: hyp.handler.clone(),
                    decision: "bug".to_string(),
                    outcome: "bug".to_string(),
                    detail: None,
                });
            }
        }
    }

    // ── Mandatory check gate (PRD §6.4: `ratify --check` semantics are
    // always on) — the final spec must parse; completeness lints are
    // surfaced beside the result, never hidden.
    let spec_path = opts
        .spec_out
        .clone()
        .unwrap_or_else(|| default_spec_path(&opts.audit_dir));
    if let Some(parent) = spec_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&spec_path, &merged)?;
    let (check_errors, check_warnings) = match crate::chumsky_adapter::parse_str(&merged) {
        Ok(parsed) => {
            let warnings = crate::check::check_completeness(&parsed);
            let errors = warnings
                .iter()
                .filter(|w| matches!(w.severity, crate::check::Severity::Error))
                .count();
            (errors, warnings.len() - errors)
        }
        Err(e) => {
            bail!(
                "ratified spec at {} does not parse: {} — the working set's skeleton is \
                 broken; fix it (or re-run the probe) and ratify again",
                spec_path.display(),
                e
            );
        }
    };

    // ── Emit scoping notes ─────────────────────────────────────────
    let scoping_path = opts
        .scoping_out
        .clone()
        .unwrap_or_else(|| default_scoping_path(&opts.audit_dir));
    if !rejected.is_empty() || !hyp_rejected.is_empty() {
        if let Some(parent) = scoping_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut scoping_block = String::new();
        if !rejected.is_empty() {
            scoping_block.push_str(&render_scoping_block(&rejected));
        }
        if !hyp_rejected.is_empty() {
            scoping_block.push_str(&render_hypothesis_scoping_block(&hyp_rejected));
        }
        append_or_create(&scoping_path, &scoping_block)?;
    }

    // ── Emit bug findings ──────────────────────────────────────────
    let findings_dir = opts
        .findings_dir
        .clone()
        .unwrap_or_else(|| default_findings_dir(&opts.audit_dir));
    let mut findings_paths = Vec::new();
    if !bugs.is_empty() || !hyp_bugs.is_empty() {
        std::fs::create_dir_all(&findings_dir)?;
        for (cluster, ratification) in &bugs {
            let path = findings_dir.join(format!("scaffold-to-spec-{}.md", cluster.id));
            let md = render_bug_finding(cluster, ratification);
            std::fs::write(&path, md)?;
            findings_paths.push(path);
        }
        for (hyp, note) in &hyp_bugs {
            let path = findings_dir.join(format!("elicitation-{}.md", hyp.id));
            let md = render_hypothesis_bug_finding(hyp, note);
            std::fs::write(&path, md)?;
            findings_paths.push(path);
        }
    }

    persist_domain_ratifications(&opts.audit_dir, &ratifications)?;
    persist_domain_interview_answers(&opts.audit_dir)?;
    let spec_handoff_path = write_spec_handoff(&opts.audit_dir, &spec_path)?;
    let domain_sequences_path = write_domain_sequences(&opts.audit_dir)?;

    // ── Elicitation outcome (Phase-0 conversion instrumentation) ───
    let run_id = hypotheses_doc
        .as_ref()
        .and_then(|d| d.run_id.clone())
        .or(answers_run_id)
        .or_else(|| manifest_run_id(&opts.audit_dir));
    let hypotheses_lowered = hypothesis_outcomes
        .iter()
        .filter(|o| o.outcome == "lowered" || o.outcome == "already_modeled")
        .count();
    let hypotheses_not_executable = hypothesis_outcomes
        .iter()
        .filter(|o| o.outcome == "confirmed_not_executable")
        .count();
    if !hypothesis_outcomes.is_empty() || run_id.is_some() {
        write_elicitation_outcome(
            &opts.audit_dir,
            run_id.as_deref(),
            hypotheses_doc.as_ref().and_then(|d| d.generated_at_unix),
            &hypothesis_outcomes,
            check_errors,
            check_warnings,
        )?;
    }

    // ── Optional model harness generation (`--proptest`) ───────────
    // Generation is `checking`-level evidence that the spec lowers;
    // running the harness (qedgen verify --proptest) earns `model-tested`.
    let model_proptest_path = if opts.proptest {
        match generate_model_proptest(&merged, &opts.audit_dir) {
            Ok(path) => {
                eprintln!(
                    "model proptest harness generated at {} — running it (e.g. via \
                     `qedgen verify --proptest --proptest-path <path>` inside a scaffolded \
                     project) is what earns the `model-tested` label",
                    path.display()
                );
                Some(path)
            }
            Err(e) => {
                eprintln!("warning: model proptest generation failed: {e}");
                None
            }
        }
    } else {
        None
    };

    Ok(RatifyReport {
        accepted: accepted_program.len()
            + accepted_by_handler.values().map(|v| v.len()).sum::<usize>(),
        narrowed: narrowed_by_handler.values().map(|v| v.len()).sum::<usize>(),
        rejected: rejected.len() + hyp_rejected.len(),
        flagged_as_bug: bugs.len() + hyp_bugs.len(),
        deferred: deferred_count,
        spec_path,
        scoping_path,
        findings_paths,
        spec_handoff_path,
        domain_sequences_path,
        run_id,
        hypotheses_lowered,
        hypotheses_not_executable,
        check_errors,
        check_warnings,
        model_proptest_path,
    })
}

/// Read the `run_id` the probe threaded into `run-manifest.json`.
fn manifest_run_id(audit_dir: &Path) -> Option<String> {
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(audit_dir.join("run-manifest.json")).ok()?)
            .ok()?;
    manifest
        .get("run_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// `elicitation-outcome.json` — the ratify half of the Phase-0 funnel:
/// joined with `hypotheses.json` on `run_id` it yields conversion and
/// time-to-first-check.
fn write_elicitation_outcome(
    audit_dir: &Path,
    run_id: Option<&str>,
    generated_at_unix: Option<u64>,
    outcomes: &[HypothesisOutcome],
    check_errors: usize,
    check_warnings: usize,
) -> Result<()> {
    let ratified_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let time_to_ratify = generated_at_unix.map(|g| ratified_at_unix.saturating_sub(g));
    let doc = serde_json::json!({
        "schema_version": 1,
        "run_id": run_id,
        "ratified_at_unix": ratified_at_unix,
        "time_to_ratify_seconds": time_to_ratify,
        "outcomes": outcomes,
        "check": { "errors": check_errors, "warnings": check_warnings },
    });
    std::fs::write(
        audit_dir.join("elicitation-outcome.json"),
        format!("{}\n", serde_json::to_string_pretty(&doc)?),
    )?;
    Ok(())
}

/// Generate the spec-model proptest harness from the ratified spec text.
fn generate_model_proptest(spec_text: &str, audit_dir: &Path) -> Result<PathBuf> {
    let parsed = crate::chumsky_adapter::parse_str(spec_text)?;
    let mir = crate::mir::lower(&parsed);
    let path = audit_dir.join("model-proptest.rs");
    crate::proptest_gen_mir::generate(&mir, &parsed, &path)?;
    Ok(path)
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
                "verification_lanes": item.pointer("/metadata/verification_lanes").cloned().unwrap_or_else(|| serde_json::json!([])),
                "authoring": domain_authoring_guidance(kind, item)
            }));

            for reason in domain_language_gaps(kind, item) {
                language_gaps.push(serde_json::json!({
                    "candidate_id": item.get("id").cloned().unwrap_or(serde_json::Value::Null),
                    "reason": reason,
                    "disposition": "document_or_extend_language",
                    "current_language_support": current_language_support(reason)
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

fn domain_authoring_guidance(kind: &str, item: &serde_json::Value) -> serde_json::Value {
    let rounding = item
        .get("rounding")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let (constructs, template, notes): (Vec<&str>, String, Vec<&str>) = match kind {
        "asset_flow" => (
            vec!["call", "transfers", "property"],
            "handler <name> { call Token.transfer(<source>, <destination>, <amount>) }\nproperty <asset>_conserved : <post_total> == old(<pre_total>) preserved_by [<handlers>]".to_string(),
            vec!["Bind the dossier source, destination, asset, and nominal amount; do not infer token authority."],
        ),
        "quantity" => {
            let expression = match rounding {
                "floor" => "mul_div_floor(<amount>, <rate>, <denominator>)",
                "ceil" => "mul_div_ceil(<amount>, <rate>, <denominator>)",
                "exact" => "<checked arithmetic expression>",
                "nearest" => "<ratify tie policy: mul_div_round_half_up(<amount>, <rate>, <denominator>) or documentary alternative>",
                _ => "<rounding policy must be ratified>",
            };
            (
                vec!["dimension", "const", "requires", "mul_div_floor", "mul_div_ceil", "mul_div_round_half_up"],
                format!("dimension <Unit> = U64\nconst <SCALE> = <ratified scale>\nlet <quantity> = {expression}"),
                vec!["Declare each ratified nominal unit with `dimension`; literals remain polymorphic, while cross-unit arithmetic and comparisons fail typechecking.", "Floor, ceiling, and nearest-with-ties-up are executable; explicitly ratify the tie policy before using a nearest mode."],
            )
        }
        "paired_operation" => (
            vec!["property", "old", "sum", "preserved_by"],
            "property <round_trip_claim> : <post relation> == old(<pre relation>) preserved_by [<forward>, <reverse>]".to_string(),
            vec!["Use `sum i : Fin[N], ...` for finite aggregates; keep the generated domain sequence as the replay target."],
        ),
        "lifecycle_edge" => (
            vec!["handler transition", "invariant", "establishes"],
            "handler <name> : State.<from> -> State.<to> {\n  establishes <postcondition>\n}".to_string(),
            vec!["Use `invariant` for preservation and `establishes` when the transition creates the truth of the predicate."],
        ),
        "authority_capability" => (
            vec!["auth", "requires", "accounts"],
            "handler <name> {\n  auth <signer_or_account.field>\n  requires <stored_authority> == <signer>.pubkey else Unauthorized\n}".to_string(),
            vec!["Choose dotted `auth` only when one signer is unambiguous; otherwise write the explicit key equality."],
        ),
        "economic_equation" => (
            vec!["dimension", "property", "invariant", "sum", "mul_div_floor", "mul_div_ceil", "mul_div_round_half_up"],
            "dimension <Unit> = U64\nproperty <equation_name> : <ratified equation> preserved_by [<handlers>]".to_string(),
            vec!["Declare nominal units before authoring the equation; finite sums, floor/ceiling arithmetic, and dimensional compatibility are executable."],
        ),
        "external_assumption" => (
            vec!["environment", "external", "constraint", "old"],
            "environment <scenario> {\n  external <object>.<field> : <Type>\n  constraint <object>.<field> <relation> old(<object>.<field>)\n}".to_string(),
            vec!["Use typed external fields for clock, oracle, CPI-return, owner, and executable assumptions; each becomes a distinct pre/post symbolic value rather than a program-state mutation."],
        ),
        _ => (vec!["manual"], "<author executable clause>".to_string(), vec![]),
    };
    serde_json::json!({
        "constructs": constructs,
        "template": template,
        "notes": notes
    })
}

fn domain_language_gaps(kind: &str, item: &serde_json::Value) -> Vec<&'static str> {
    let mut gaps = Vec::new();
    match kind {
        "quantity" => {
            match item
                .get("rounding")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
            {
                "exact" | "floor" | "ceil" => {}
                "nearest" => gaps.push("rounding_mode_nearest"),
                _ => gaps.push("rounding_policy_unresolved"),
            }
        }
        "economic_equation" => {}
        "external_assumption" => {}
        _ => {}
    }
    gaps
}

fn current_language_support(reason: &str) -> &'static str {
    match reason {
        "dimensional_unit_types" => {
            "Declare `dimension Name = U64` (or another integer base); the checker enforces nominal compatibility and codegen erases the name to its integer ABI."
        }
        "rounding_mode_nearest" => {
            "`mul_div_round_half_up` is executable for nearest-with-ties-up; a generic `nearest` dossier value still requires explicit tie-policy ratification, and ties-to-even has no builtin."
        }
        "rounding_policy_unresolved" => {
            "The language supports exact, floor, and ceiling arithmetic after the intended policy is ratified."
        }
        "external_environment_model" => {
            "`environment` supports distinct typed scalar namespaces with `external object.field : Type`; composite imported contracts and direct coupling into handler bodies still require manual modeling."
        }
        _ => "No executable lowering is currently available.",
    }
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

/// Rejected-hypothesis log — same contract as the cluster scoping block:
/// the evidence was real, the user judged the claim non-fit; re-evaluate
/// on future audits.
fn render_hypothesis_scoping_block(rejected: &[(&InvariantHypothesis, String)]) -> String {
    let mut s = String::new();
    let now_iso = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Iso8601::DEFAULT)
        .unwrap_or_else(|_| "unknown".to_string());
    s.push_str(&format!(
        "\n## Rejected invariant hypotheses — {}\n\n",
        now_iso
    ));
    for (hyp, note) in rejected {
        s.push_str(&format!(
            "### hypothesis `{}` — {} (handler `{}`)\n\n",
            hyp.id,
            hyp.class.as_str(),
            hyp.handler
        ));
        s.push_str(&format!("**Claim:** {}\n\n", hyp.claim));
        for e in &hyp.evidence {
            s.push_str(&format!(
                "**Evidence:** {}{}\n\n",
                e.detail,
                e.source
                    .as_deref()
                    .map(|p| format!(" ({})", p))
                    .unwrap_or_default()
            ));
        }
        if note.trim().is_empty() {
            s.push_str("_No rationale captured._\n\n");
        } else {
            s.push_str("**User rationale:**\n\n");
            for line in note.lines() {
                s.push_str(&format!("> {}\n", line));
            }
            s.push('\n');
        }
    }
    s
}

/// A hypothesis answered `BUG`: the user confirms the invariant is
/// *intended* but reports the code does not enforce it — elicitation
/// doubling as a bug-catcher (PRD §6.2).
fn render_hypothesis_bug_finding(hyp: &InvariantHypothesis, note: &str) -> String {
    let mut s = String::new();
    s.push_str(&format!("# Elicitation bug flag: `{}`\n\n", hyp.id));
    s.push_str(&format!("**Class:** `{}`\n", hyp.class.as_str()));
    s.push_str(&format!("**Handler:** `{}`\n", hyp.handler));
    s.push_str(&format!("**Confidence:** {:?}\n\n", hyp.confidence));
    s.push_str(&format!("**Claim:** {}\n\n", hyp.claim));
    s.push_str("## Evidence\n\n");
    for e in &hyp.evidence {
        s.push_str(&format!(
            "- {}{}\n",
            e.detail,
            e.source
                .as_deref()
                .map(|p| format!(" ({})", p))
                .unwrap_or_default()
        ));
    }
    s.push('\n');
    s.push_str(
        "The user confirmed this invariant is **intended** but flagged that the \
         code does NOT enforce it — a missing-enforcement bug, not a spec gap. \
         Reproduce with a source-bound backend before claiming exploitability.\n",
    );
    if !note.trim().is_empty() {
        s.push_str("\n## User rationale\n\n");
        for line in note.lines() {
            s.push_str(&format!("> {}\n", line));
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

/// Project root for default output paths (#249). Priority:
/// 1. the working set's recorded `target.program_root`
///    (`run-manifest.json`) — authoritative, immune to audit-dir nesting;
/// 2. the `.qed` ancestor convention — `.qed/audit/<ts>/` (or any
///    nesting depth) resolves to the directory *containing* `.qed`.
///    The old two-`.parent()` arithmetic landed on `<root>/.qed` itself
///    and produced `<root>/.qed/.qed.qedspec`;
/// 3. cwd.
fn project_root_of(audit_dir: &Path) -> Option<PathBuf> {
    if let Some(root) = manifest_program_root(audit_dir) {
        return Some(root);
    }
    audit_dir
        .ancestors()
        .find(|a| a.file_name().is_some_and(|n| n == ".qed"))
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

/// Read the `target.program_root` the probe recorded in
/// `run-manifest.json`.
fn manifest_program_root(audit_dir: &Path) -> Option<PathBuf> {
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(audit_dir.join("run-manifest.json")).ok()?)
            .ok()?;
    let root = manifest.get("target")?.get("program_root")?.as_str()?;
    if root.is_empty() {
        return None;
    }
    let path = PathBuf::from(root);
    // #289: manifests written by pre-canonicalization binaries can carry
    // a relative root (e.g. `"."`), whose `file_name()` is `None` and
    // degrades every derived name to the `"program"` placeholder.
    // Best-effort canonicalize against the ratify cwd; keep the raw
    // path when resolution fails.
    if path.is_relative() {
        if let Ok(canonical) = path.canonicalize() {
            return Some(canonical);
        }
    }
    Some(path)
}

fn default_spec_path(audit_dir: &Path) -> PathBuf {
    let project_root = project_root_of(audit_dir);
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
    project_root_of(audit_dir)
        .map(|root| root.join(".qed/plan/scoping.md"))
        .unwrap_or_else(|| PathBuf::from(".qed/plan/scoping.md"))
}

fn default_findings_dir(audit_dir: &Path) -> PathBuf {
    project_root_of(audit_dir)
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
            answers: None,
            proptest: false,
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
            answers: None,
            proptest: false,
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
            answers: None,
            proptest: false,
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
            "rounding_policy_unresolved"
        );
        assert!(handoff["layers"]["domain"][0]["authoring"]["template"]
            .as_str()
            .unwrap()
            .contains("rounding policy must be ratified"));
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
            answers: None,
            proptest: false,
        })?;
        assert_eq!(
            std::fs::read_to_string(rerun.spec_handoff_path.expect("rerun handoff"))?,
            first_handoff
        );
        Ok(())
    }

    #[test]
    fn quantity_handoff_distinguishes_supported_rounding_from_true_gaps() {
        let floor = serde_json::json!({ "unit": "lamports", "rounding": "floor" });
        assert!(domain_language_gaps("quantity", &floor).is_empty());
        let dimensionless = serde_json::json!({ "unit": "dimensionless", "rounding": "ceil" });
        assert!(domain_language_gaps("quantity", &dimensionless).is_empty());
        let nearest = serde_json::json!({ "unit": "count", "rounding": "nearest" });
        assert_eq!(
            domain_language_gaps("quantity", &nearest),
            vec!["rounding_mode_nearest"]
        );
        let guidance = domain_authoring_guidance("quantity", &floor);
        assert!(guidance["template"]
            .as_str()
            .unwrap()
            .contains("dimension <Unit> = U64"));

        let external = serde_json::json!({ "kind": "clock", "relation": "monotonic" });
        assert!(domain_language_gaps("external_assumption", &external).is_empty());
        let external_guidance = domain_authoring_guidance("external_assumption", &external);
        assert!(external_guidance["template"]
            .as_str()
            .unwrap()
            .contains("external <object>.<field> : <Type>"));
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
            answers: None,
            proptest: false,
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
            answers: None,
            proptest: false,
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
            answers: None,
            proptest: false,
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
                    answers: None,
                    proptest: false,
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
            answers: None,
            proptest: false,
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

    fn write_hypotheses(
        dir: &Path,
        run_id: &str,
        hypotheses: &[crate::probe::hypothesize::InvariantHypothesis],
    ) -> Result<()> {
        std::fs::write(
            dir.join("hypotheses.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 1,
                "run_id": run_id,
                "generated_at_unix": 1_700_000_000u64,
                "hypotheses": hypotheses,
            }))?,
        )?;
        Ok(())
    }

    fn auth_hypothesis(
        handler: &str,
        signer: &str,
    ) -> crate::probe::hypothesize::InvariantHypothesis {
        use crate::probe::hypothesize::*;
        InvariantHypothesis {
            id: format!("h-abc12345-authorization-{handler}"),
            class: HypothesisClass::Authorization,
            handler: handler.to_string(),
            claim: format!("`{handler}` requires the stored authority"),
            evidence: vec![],
            payoff: String::new(),
            backend: String::new(),
            assurance: "checking".to_string(),
            confidence: crate::cluster::Confidence::High,
            lowering: Some(HypothesisLowering::AuthClause {
                signer_account: signer.to_string(),
            }),
        }
    }

    /// Structured `answers.json` (in-harness interview): an accepted auth
    /// hypothesis lowers to a real `auth` clause, the ratified spec
    /// parses + checks, and the run_id carries through to the outcome
    /// record. No `interview.md` exists — the legacy file must not be
    /// required on this path.
    #[test]
    fn structured_answers_lower_auth_hypothesis() -> Result<()> {
        let dir = tempdir()?;
        let audit = dir.path().join(".qed/audit/test");
        std::fs::create_dir_all(&audit)?;
        let skeleton = render_skeleton_from_handlers(&["set_fee".into()], "vault");
        std::fs::write(audit.join("skeleton.qedspec"), &skeleton)?;
        std::fs::write(audit.join("clusters.json"), "[]")?;
        let hyp = auth_hypothesis("set_fee", "admin");
        write_hypotheses(&audit, "run-vault-123", std::slice::from_ref(&hyp))?;
        std::fs::write(
            audit.join("answers.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "answers": [{ "id": hyp.id, "decision": "accept", "note": "yes, admin-gated" }]
            }))?,
        )?;

        let report = run(&RatifyOpts {
            audit_dir: audit.clone(),
            spec_out: Some(dir.path().join("vault.qedspec")),
            scoping_out: Some(dir.path().join("scoping.md")),
            findings_dir: Some(dir.path().join("findings")),
            answers: None,
            proptest: false,
        })?;
        assert_eq!(report.hypotheses_lowered, 1);
        assert_eq!(report.hypotheses_not_executable, 0);
        assert_eq!(report.run_id.as_deref(), Some("run-vault-123"));
        let spec = std::fs::read_to_string(&report.spec_path)?;
        assert!(spec.contains("auth admin"), "spec:\n{spec}");
        assert!(spec.contains(&format!("provenance: hypothesis {}", hyp.id)));
        crate::chumsky_adapter::parse_str(&spec).expect("ratified spec parses");
        let outcome: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
            audit.join("elicitation-outcome.json"),
        )?)?;
        assert_eq!(outcome["run_id"], "run-vault-123");
        assert_eq!(outcome["outcomes"][0]["outcome"], "lowered");
        assert!(outcome["time_to_ratify_seconds"].is_u64());
        Ok(())
    }

    /// #248: a hypotheses-only working set (skeleton + hypotheses.json,
    /// no clusters.json — the bootstrap lane's shape) must ratify on the
    /// hypothesis answers alone instead of hard-requiring clusters.json.
    #[test]
    fn hypotheses_only_working_set_ratifies_without_clusters() -> Result<()> {
        let dir = tempdir()?;
        let audit = dir.path().join(".qed/audit/test");
        std::fs::create_dir_all(&audit)?;
        let skeleton = render_skeleton_from_handlers(&["set_fee".into()], "vault");
        std::fs::write(audit.join("skeleton.qedspec"), &skeleton)?;
        // NOTE: no clusters.json.
        let hyp = auth_hypothesis("set_fee", "admin");
        write_hypotheses(&audit, "run-vault-124", std::slice::from_ref(&hyp))?;
        std::fs::write(
            audit.join("answers.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "answers": [{ "id": hyp.id, "decision": "accept", "note": "" }]
            }))?,
        )?;

        let report = run(&RatifyOpts {
            audit_dir: audit,
            spec_out: Some(dir.path().join("vault.qedspec")),
            scoping_out: Some(dir.path().join("scoping.md")),
            findings_dir: Some(dir.path().join("findings")),
            answers: None,
            proptest: false,
        })?;
        assert_eq!(report.hypotheses_lowered, 1);
        let spec = std::fs::read_to_string(&report.spec_path)?;
        assert!(spec.contains("auth admin"), "spec:\n{spec}");
        Ok(())
    }

    /// #249: the conventional `.qed/audit/<ts>` audit dir must resolve
    /// default outputs to `<root>/<name>.qedspec` and
    /// `<root>/.qed/plan/scoping.md` — the old two-`.parent()` arithmetic
    /// produced `<root>/.qed/.qed.qedspec`. Both relative and absolute.
    #[test]
    fn default_paths_resolve_qed_audit_convention() {
        for audit in [
            PathBuf::from("proj/.qed/audit/run-1"),
            PathBuf::from("/abs/proj/.qed/audit/run-1"),
        ] {
            let spec = default_spec_path(&audit);
            assert!(
                spec.ends_with("proj/proj.qedspec"),
                "audit {} -> spec {}",
                audit.display(),
                spec.display()
            );
            let scoping = default_scoping_path(&audit);
            assert!(
                scoping.ends_with("proj/.qed/plan/scoping.md"),
                "audit {} -> scoping {}",
                audit.display(),
                scoping.display()
            );
            let findings = default_findings_dir(&audit);
            assert!(
                findings.ends_with("proj/.qed/findings"),
                "audit {} -> findings {}",
                audit.display(),
                findings.display()
            );
        }
    }

    /// #249: `run-manifest.json`'s `target.program_root` is authoritative
    /// over path arithmetic — non-conventional audit-dir nesting still
    /// resolves to the recorded program root.
    #[test]
    fn default_paths_prefer_manifest_program_root() -> Result<()> {
        let dir = tempdir()?;
        let audit = dir.path().join("elsewhere/deep/audit-dir");
        std::fs::create_dir_all(&audit)?;
        let prog_root = dir.path().join("programs/vault");
        std::fs::write(
            audit.join("run-manifest.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "target": { "program_root": prog_root.display().to_string() }
            }))?,
        )?;
        assert_eq!(default_spec_path(&audit), prog_root.join("vault.qedspec"));
        assert_eq!(
            default_scoping_path(&audit),
            prog_root.join(".qed/plan/scoping.md")
        );
        assert_eq!(
            default_findings_dir(&audit),
            prog_root.join(".qed/findings")
        );
        Ok(())
    }

    /// #289: a relative `program_root` recorded by a pre-canonicalization
    /// binary (`--program .`) resolves against the ratify cwd — derived
    /// names use the real directory name, never the `program.qedspec`
    /// placeholder that `file_name() == None` produces.
    #[test]
    fn default_paths_canonicalize_relative_manifest_program_root() -> Result<()> {
        let dir = tempdir()?;
        let audit = dir.path().join("audit");
        std::fs::create_dir_all(&audit)?;
        std::fs::write(
            audit.join("run-manifest.json"),
            r#"{ "target": { "program_root": "." } }"#,
        )?;
        let cwd = std::env::current_dir()?.canonicalize()?;
        let name = cwd
            .file_name()
            .and_then(|n| n.to_str())
            .expect("test cwd has a name")
            .to_string();
        assert_eq!(
            default_spec_path(&audit),
            cwd.join(format!("{name}.qedspec"))
        );
        assert_eq!(
            default_scoping_path(&audit),
            cwd.join(".qed/plan/scoping.md")
        );
        Ok(())
    }

    /// A confirmed hypothesis whose handler is missing from the skeleton
    /// is reported `confirmed, not executable` — never inserted as a
    /// placeholder comment.
    #[test]
    fn structured_answers_not_executable_fallback() -> Result<()> {
        let dir = tempdir()?;
        let audit = dir.path().join(".qed/audit/test");
        std::fs::create_dir_all(&audit)?;
        let skeleton = render_skeleton_from_handlers(&["other_handler".into()], "vault");
        std::fs::write(audit.join("skeleton.qedspec"), &skeleton)?;
        std::fs::write(audit.join("clusters.json"), "[]")?;
        let hyp = auth_hypothesis("set_fee", "admin");
        write_hypotheses(&audit, "run-vault-124", std::slice::from_ref(&hyp))?;
        std::fs::write(
            audit.join("answers.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "answers": [{ "id": hyp.id, "decision": "accept" }]
            }))?,
        )?;

        let report = run(&RatifyOpts {
            audit_dir: audit.clone(),
            spec_out: Some(dir.path().join("vault.qedspec")),
            scoping_out: Some(dir.path().join("scoping.md")),
            findings_dir: Some(dir.path().join("findings")),
            answers: None,
            proptest: false,
        })?;
        assert_eq!(report.hypotheses_lowered, 0);
        assert_eq!(report.hypotheses_not_executable, 1);
        let spec = std::fs::read_to_string(&report.spec_path)?;
        assert!(!spec.contains("auth admin"));
        assert!(!spec.contains(&hyp.id), "no placeholder comment: {spec}");
        let outcome: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
            audit.join("elicitation-outcome.json"),
        )?)?;
        assert_eq!(
            outcome["outcomes"][0]["outcome"],
            "confirmed_not_executable"
        );
        Ok(())
    }

    /// A hypothesis answered `bug` routes to a finding file (elicitation
    /// as bug-catcher) and a rejected one to the scoping log.
    #[test]
    fn structured_answers_bug_and_reject_route() -> Result<()> {
        let dir = tempdir()?;
        let audit = dir.path().join(".qed/audit/test");
        std::fs::create_dir_all(&audit)?;
        let skeleton = render_skeleton_from_handlers(&["set_fee".into(), "close".into()], "vault");
        std::fs::write(audit.join("skeleton.qedspec"), &skeleton)?;
        std::fs::write(audit.join("clusters.json"), "[]")?;
        let bug_hyp = auth_hypothesis("set_fee", "admin");
        let rej_hyp = auth_hypothesis("close", "admin");
        write_hypotheses(&audit, "run-vault-125", &[bug_hyp.clone(), rej_hyp.clone()])?;
        std::fs::write(
            audit.join("answers.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "answers": [
                    { "id": bug_hyp.id, "decision": "bug", "note": "intended but unenforced" },
                    { "id": rej_hyp.id, "decision": "reject", "note": "close is permissionless by design" }
                ]
            }))?,
        )?;

        let scoping = dir.path().join("scoping.md");
        let report = run(&RatifyOpts {
            audit_dir: audit,
            spec_out: Some(dir.path().join("vault.qedspec")),
            scoping_out: Some(scoping.clone()),
            findings_dir: Some(dir.path().join("findings")),
            answers: None,
            proptest: false,
        })?;
        assert_eq!(report.flagged_as_bug, 1);
        assert_eq!(report.rejected, 1);
        let finding = std::fs::read_to_string(&report.findings_paths[0])?;
        assert!(finding.contains("Elicitation bug flag"));
        assert!(finding.contains("intended but unenforced"));
        let scoping_text = std::fs::read_to_string(&scoping)?;
        assert!(scoping_text.contains("Rejected invariant hypotheses"));
        assert!(scoping_text.contains("permissionless by design"));
        Ok(())
    }

    /// An accepted unwired-guard hypothesis is a missing-enforcement
    /// finding, never a spec clause: accept routes to the bug path with
    /// outcome `confirmed_unenforced` (elicitation as bug-catcher).
    #[test]
    fn structured_answers_unwired_guard_accept_routes_to_finding() -> Result<()> {
        use crate::probe::hypothesize::*;
        let dir = tempdir()?;
        let audit = dir.path().join(".qed/audit/test");
        std::fs::create_dir_all(&audit)?;
        let skeleton = render_skeleton_from_handlers(&["set_fee".into()], "vault");
        std::fs::write(audit.join("skeleton.qedspec"), &skeleton)?;
        std::fs::write(audit.join("clusters.json"), "[]")?;
        let hyp = InvariantHypothesis {
            id: "h-9999aaaa-unwired_guard-CapTooHigh".to_string(),
            class: HypothesisClass::UnwiredGuard,
            handler: "CapTooHigh".to_string(),
            claim: "Error variant `CapTooHigh` names a check the program never enforces"
                .to_string(),
            evidence: vec![],
            payoff: String::new(),
            backend: String::new(),
            assurance: "checking".to_string(),
            confidence: crate::cluster::Confidence::Medium,
            lowering: None,
        };
        write_hypotheses(&audit, "run-vault-127", std::slice::from_ref(&hyp))?;
        std::fs::write(
            audit.join("answers.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "answers": [{ "id": hyp.id, "decision": "accept" }]
            }))?,
        )?;

        let report = run(&RatifyOpts {
            audit_dir: audit.clone(),
            spec_out: Some(dir.path().join("vault.qedspec")),
            scoping_out: Some(dir.path().join("scoping.md")),
            findings_dir: Some(dir.path().join("findings")),
            answers: None,
            proptest: false,
        })?;
        // Routed as a finding, not a lowering.
        assert_eq!(report.flagged_as_bug, 1);
        assert_eq!(report.hypotheses_lowered, 0);
        let finding = std::fs::read_to_string(&report.findings_paths[0])?;
        assert!(finding.contains("Elicitation bug flag"));
        let outcome: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
            audit.join("elicitation-outcome.json"),
        )?)?;
        assert_eq!(outcome["outcomes"][0]["outcome"], "confirmed_unenforced");
        Ok(())
    }

    /// Lifecycle lowering annotates a bare handler with an init-shaped
    /// transition resolved against the skeleton's own State variants.
    #[test]
    fn structured_answers_lower_lifecycle_transition() -> Result<()> {
        use crate::probe::hypothesize::*;
        let dir = tempdir()?;
        let audit = dir.path().join(".qed/audit/test");
        std::fs::create_dir_all(&audit)?;
        // Bare handler (no transition annotation) — the interesting case.
        let skeleton = "spec Vault\n\ntype State\n  | Uninitialized\n  | Active\n\ntype Error\n  | InvalidArgument\n  | Unauthorized\n\nhandler initialize {\n  // accounts, requires, effect, transfers — filled by interview\n}\n";
        std::fs::write(audit.join("skeleton.qedspec"), skeleton)?;
        std::fs::write(audit.join("clusters.json"), "[]")?;
        let hyp = InvariantHypothesis {
            id: "h-def67890-lifecycle_init_once-initialize".to_string(),
            class: HypothesisClass::LifecycleInitOnce,
            handler: "initialize".to_string(),
            claim: "`initialize` runs exactly once".to_string(),
            evidence: vec![],
            payoff: String::new(),
            backend: String::new(),
            assurance: "checking".to_string(),
            confidence: crate::cluster::Confidence::High,
            lowering: Some(HypothesisLowering::LifecycleTransition),
        };
        write_hypotheses(&audit, "run-vault-126", std::slice::from_ref(&hyp))?;
        std::fs::write(
            audit.join("answers.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "answers": [{ "id": hyp.id, "decision": "accept" }]
            }))?,
        )?;

        let report = run(&RatifyOpts {
            audit_dir: audit,
            spec_out: Some(dir.path().join("vault.qedspec")),
            scoping_out: Some(dir.path().join("scoping.md")),
            findings_dir: Some(dir.path().join("findings")),
            answers: None,
            proptest: false,
        })?;
        assert_eq!(report.hypotheses_lowered, 1);
        let spec = std::fs::read_to_string(&report.spec_path)?;
        assert!(
            spec.contains("handler initialize : State.Uninitialized -> State.Active {"),
            "spec:\n{spec}"
        );
        crate::chumsky_adapter::parse_str(&spec).expect("ratified spec parses");
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
            answers: None,
            proptest: false,
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
