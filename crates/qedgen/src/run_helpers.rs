//! Dispatch-support glue for `run::dispatch`: git-repo guards, the
//! Crucible-backend bridge, CI-template expansion, lint-warning
//! formatting, and the Anchor/native probe runners. Split out of
//! `main.rs` (v3.0 prep). `use crate::*` pulls in the crate-root
//! re-export hub (the `crate::<module>` aliases these helpers reach).

use crate::probe::{domain_extract, domain_interview};
use crate::*;
use anyhow::{ensure, Result};
use std::path::{Path, PathBuf};

/// Shared argument validation for the two Leanstral dispatch verbs
/// (`generate`, `fill-sorry`).
pub(crate) fn validate_dispatch_args(
    passes: usize,
    temperature: f64,
    max_tokens: usize,
) -> Result<()> {
    ensure!(passes > 0, "passes must be greater than 0");
    ensure!(
        (0.0..=2.0).contains(&temperature),
        "temperature must be between 0.0 and 2.0"
    );
    ensure!(max_tokens > 0, "max_tokens must be greater than 0");
    Ok(())
}

/// Shared poll-interval bounds for the two waiting Aristotle verbs
/// (`submit`, `status`).
pub(crate) fn validate_poll_interval(poll_interval: Option<u64>) -> Result<()> {
    if let Some(interval) = poll_interval {
        ensure!(interval >= 5, "poll_interval must be at least 5 seconds");
        ensure!(
            interval <= 3600,
            "poll_interval must be at most 3600 seconds"
        );
    }
    Ok(())
}

/// The five Rust-shaped codegen artifacts share one skip rationale for
/// sBPF specs (assembly is verified via Lean proofs only).
pub(crate) fn note_sbpf_skip(artifact: &str) {
    eprintln!(
        "note: skipping {artifact} codegen for sBPF spec — assembly \
         programs are verified via Lean proofs; runtime checks \
         belong in client-side tests."
    );
}

/// Resolve a codegen output path against the spec's directory (#279).
/// Relative paths — including every clap literal default like
/// `./programs` — join onto `spec_dir`; the `Components` re-collect
/// drops the interior `CurDir` so the resolved path never renders a
/// literal `/./` (#290). Absolute paths pass through untouched.
pub(crate) fn resolve_output_against_spec(spec_dir: &Path, p: PathBuf) -> PathBuf {
    if p.is_absolute() {
        p
    } else {
        spec_dir.join(p).components().collect()
    }
}

/// Canonicalize the `--program` root once at the probe entry (#289).
/// `--program .` is the natural invocation from inside a program root,
/// but every downstream name — the skeleton `spec` name, the
/// `target.program_root` recorded in the run manifest and dossier, and
/// ratify's default `<name>.qedspec` — derives from
/// `Path::file_name()`, which is `None` for `"."` and falls back to a
/// `"program"` placeholder. Nonexistent paths fall back to a lexical
/// cwd-join so downstream "not found" errors still fire on a concrete
/// path.
pub(crate) fn canonicalize_program_root(raw: &Path) -> PathBuf {
    raw.canonicalize().unwrap_or_else(|_| {
        std::env::current_dir()
            .map(|cwd| cwd.join(raw).components().collect())
            .unwrap_or_else(|_| raw.to_path_buf())
    })
}

/// Materialize the audit working set (`interview.md`, `clusters.json`,
/// `skeleton.qedspec`, domain dossier seed, and run manifest) under `dir` —
/// shared by the Pinocchio (run.rs),
/// Anchor, and Native probe paths, which previously each carried this
/// block inline. `lenient_skeleton`: the Anchor path falls back to a
/// minimal stub when the structural adapter fails (non-standard
/// layouts); the other paths propagate the error.
pub(crate) fn write_audit_working_set(
    dir: &Path,
    prog_root: &Path,
    clusters: &[cluster::Cluster],
    framework: program_model::ProgramFramework,
    dossier_runtime: &str,
    lenient_skeleton: bool,
) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let program_name = prog_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("program")
        .to_string();
    let now_iso = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Iso8601::DEFAULT)
        .unwrap_or_else(|_| "unknown".to_string());
    let md = prompts::render_interview(clusters, &program_name, &now_iso);
    std::fs::write(dir.join("interview.md"), md)?;
    // clusters.json — ratify looks up cluster_id → suggested_syntax here.
    let clusters_json = serde_json::to_string_pretty(clusters)?;
    std::fs::write(dir.join("clusters.json"), clusters_json)?;
    // skeleton.qedspec — handler stubs only.
    let anchor_overrides = std::collections::HashMap::new();
    let adapter_config = adapt::AdapterConfig::new(&program_name, &anchor_overrides);
    let adapter = adapt::adapter_for_framework(framework, adapter_config);
    let model = match adapter.extract(prog_root) {
        Ok(model) => Some(model),
        Err(e) if lenient_skeleton => {
            eprintln!(
                "warning: program adapter failed ({}); writing minimal skeleton",
                e
            );
            None
        }
        Err(e) => return Err(e),
    };
    let minimal_skeleton = || {
        format!(
            "spec {}\n\ntype State | Init | Active\ntype Error | InvalidArgument\n",
            program_name
        )
    };
    let skeleton = match model.as_ref() {
        Some(model) => match adapter.render_spec(model) {
            Ok(skeleton) => skeleton,
            Err(e) if lenient_skeleton => {
                eprintln!(
                    "warning: program adapter render failed ({}); writing minimal skeleton",
                    e
                );
                minimal_skeleton()
            }
            Err(e) => return Err(e),
        },
        None => minimal_skeleton(),
    };
    std::fs::write(dir.join("skeleton.qedspec"), skeleton)?;
    write_domain_artifact_seeds(
        dir,
        prog_root,
        &program_name,
        clusters,
        dossier_runtime,
        model.as_ref(),
    )?;
    eprintln!("Wrote audit working set to {}", dir.display());
    Ok(())
}

/// Emit the machine-readable handoff that connects structural probe clusters
/// to the read-driven domain pass. QEDGen owns only code-derived structural
/// candidates here; semantic domain arrays intentionally start empty and the
/// auditor fills them from source review before ratification.
fn write_domain_artifact_seeds(
    dir: &Path,
    prog_root: &Path,
    program_name: &str,
    clusters: &[cluster::Cluster],
    runtime: &str,
    model: Option<&program_model::ProgramModel>,
) -> Result<()> {
    let run_name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("run");
    let audit_id = stable_audit_id(&format!("{program_name}_{run_name}"));
    let structural_candidates: Vec<serde_json::Value> = clusters
        .iter()
        .map(|candidate| {
            let confidence = match candidate.confidence {
                cluster::Confidence::High => "high",
                cluster::Confidence::Medium => "medium",
                cluster::Confidence::Low => "low",
            };
            serde_json::json!({
                "id": candidate.id,
                "kind": candidate.kind.as_str(),
                "scope": candidate.scope.as_key(),
                "summary": candidate.proto_clause_text,
                "suggested_syntax": candidate.suggested_syntax,
                "probe_confidence": confidence,
                "ratification": "pending",
                "rationale": null,
                "finding_ids": candidate.finding_ids,
                "evidence_count": candidate.evidence_count,
            })
        })
        .collect();
    let handlers: Vec<serde_json::Value> = model
        .map(|model| {
            model
                .handlers
                .iter()
                .map(|handler| {
                    let args: Vec<serde_json::Value> = handler
                        .args
                        .iter()
                        .map(|arg| {
                            serde_json::json!({
                                "name": arg.name,
                                "qedspec_type": arg.qedspec_type,
                            })
                        })
                        .collect();
                    serde_json::json!({
                        "name": handler.name,
                        "source_path": handler.source_path.as_ref().map(|path| path.display().to_string()),
                        "accounts_type": handler.accounts_type,
                        "args": args,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let source_facts = domain_extract::extract_program(prog_root)?;
    let asset_flows: Vec<_> = source_facts
        .asset_flows
        .iter()
        .map(|flow| {
            let nominal_amount = flow
                .quantity_references
                .first()
                .cloned()
                .unwrap_or_else(|| "pending:user".to_string());
            serde_json::json!({
                "id": flow.id,
                "handler": flow.handler.as_deref().unwrap_or("unknown_handler"),
                "asset": "token_or_lamports_pending",
                "source": "pending:source_account",
                "destination": "pending:destination_account",
                "nominal_amount": nominal_amount,
                "delivered_amount": null,
                "fee_expression": null,
                "authority": null,
                "state_mutations": [],
                "metadata": source_hint_metadata(&flow.source_span, &flow.claim_limit),
            })
        })
        .collect();
    let quantities: Vec<_> = source_facts
        .quantities
        .iter()
        .map(|quantity| {
            let unit = serde_json::to_value(&quantity.unit_hint)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "unknown".to_string());
            serde_json::json!({
                "id": quantity.id,
                "symbol": quantity.identifier,
                "unit": unit,
                "scale": "pending:user",
                "rounding": "unknown",
                "valid_range": null,
                "conversions": [],
                "metadata": source_hint_metadata(&quantity.source_span, &quantity.claim_limit),
            })
        })
        .collect();
    let paired_operations: Vec<_> = source_facts
        .paired_operations
        .iter()
        .map(|pair| {
            let anchors: Vec<_> = pair.evidence.iter().map(source_anchor).collect();
            serde_json::json!({
                "id": pair.id,
                "left_operation": pair.left_operation,
                "right_operation": pair.right_operation,
                "relationship": pair.relationship,
                "left_handlers": pair.left_handlers,
                "right_handlers": pair.right_handlers,
                "metadata": {
                    "confidence": "derived",
                    "ratification": "pending",
                    "rationale": pair.claim_limit,
                    "source_anchors": anchors,
                    "verification_lanes": ["manual", "crucible"]
                }
            })
        })
        .collect();

    let dossier = serde_json::json!({
        "schema_version": 1,
        "schema_uri": "https://qedgen.dev/schemas/auditor/domain-dossier-v1.schema.json",
        "audit_id": audit_id,
        "target": {
            "program_root": prog_root.display().to_string(),
            "runtime": runtime,
            "mode": "spec-less",
            "spec": null,
            "source_commit": null,
        },
        "handlers": handlers,
        "structural_candidates": structural_candidates,
        "asset_flows": asset_flows,
        "quantities": quantities,
        "paired_operations": paired_operations,
        "lifecycle_edges": [],
        "authority_capabilities": [],
        "economic_equations": [],
        "external_assumptions": [],
    });
    let dossier_json = format!("{}\n", serde_json::to_string_pretty(&dossier)?);
    std::fs::write(dir.join("domain-dossier.json"), dossier_json)?;
    let domain_questions = domain_interview::generate_questions(&dossier);
    std::fs::write(
        dir.join("domain-interview.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 1,
                "questions": domain_questions,
                "answers": []
            }))?
        ),
    )?;
    let mut domain_interview_md = String::from(
        "# Domain intent interview\n\nAnswer only intent questions; source locations and code facts are already captured in `domain-dossier.json`. Record `confirm`, `reject`, or `bug` plus rationale in `domain-interview.json`.\n\n",
    );
    for question in &domain_questions {
        domain_interview_md.push_str(&format!(
            "## `{}`\n\n{}\n\n- Decision: pending\n- Rationale:\n\n",
            question.candidate_id, question.prompt
        ));
    }
    if domain_questions.is_empty() {
        domain_interview_md.push_str("No pending domain candidates were extracted.\n");
    }
    std::fs::write(dir.join("domain-interview.md"), domain_interview_md)?;

    let mut dossier_md = format!(
        "# Domain dossier — {}\n\nCanonical data: `domain-dossier.json`.\n\n## Structural candidates\n\n| ID | Kind | Scope | Confidence | Summary |\n|---|---|---|---|---|\n",
        program_name
    );
    for candidate in clusters {
        let confidence = match candidate.confidence {
            cluster::Confidence::High => "high",
            cluster::Confidence::Medium => "medium",
            cluster::Confidence::Low => "low",
        };
        let summary = candidate
            .proto_clause_text
            .replace('|', "\\|")
            .replace('\n', " ");
        dossier_md.push_str(&format!(
            "| `{}` | `{}` | `{}` | {} | {} |\n",
            candidate.id,
            candidate.kind.as_str(),
            candidate.scope.as_key(),
            confidence,
            summary
        ));
    }
    dossier_md.push_str(
        "\n## Asset flows\n\nPending source review.\n\n## Quantities and units\n\nPending source review.\n\n## Lifecycle\n\nPending source review.\n\n## Authority capabilities\n\nPending source review.\n\n## Economic equations\n\nPending user ratification.\n\n## External assumptions\n\nPending source review.\n",
    );
    std::fs::write(dir.join("domain-dossier.md"), dossier_md)?;

    let manifest = serde_json::json!({
        "schema_version": 1,
        "schema_uri": "https://qedgen.dev/schemas/auditor/audit-run-manifest-v1.schema.json",
        "audit_id": audit_id,
        "status": "running",
        "target": {
            "program_root": prog_root.display().to_string(),
            "runtime": runtime,
            "mode": "spec-less",
            "spec": null,
            "source_commit": null,
        },
        "tool_versions": {},
        "lanes": [
            { "name": "source-review", "status": "not-run", "reason": null, "resume_command": null, "started_at": null, "finished_at": null, "artifact_paths": [] },
            { "name": "ordinary-probe", "status": "passed", "reason": null, "resume_command": null, "started_at": null, "finished_at": null, "artifact_paths": ["clusters.json", "domain-dossier.json"] },
            { "name": "compile", "status": "not-run", "reason": null, "resume_command": null, "started_at": null, "finished_at": null, "artifact_paths": [] },
            { "name": "mollusk", "status": "not-run", "reason": null, "resume_command": null, "started_at": null, "finished_at": null, "artifact_paths": [] },
            { "name": "miri", "status": "not-run", "reason": null, "resume_command": null, "started_at": null, "finished_at": null, "artifact_paths": [] },
            { "name": "crucible-protocol", "status": "not-run", "reason": null, "resume_command": null, "started_at": null, "finished_at": null, "artifact_paths": [] },
            { "name": "crucible-skeleton", "status": "not-run", "reason": null, "resume_command": null, "started_at": null, "finished_at": null, "artifact_paths": [] },
            { "name": "crucible-domain", "status": "not-run", "reason": null, "resume_command": null, "started_at": null, "finished_at": null, "artifact_paths": [] }
        ],
        "artifacts": {
            "domain_dossier_json": "domain-dossier.json",
            "domain_dossier_markdown": "domain-dossier.md",
            "ratified_intent": null,
            "domain_interview": "domain-interview.json",
            "domain_sequences": null,
            "spec_handoff": null,
            "report": null,
            "suppressions": null,
        }
    });
    let manifest_json = format!("{}\n", serde_json::to_string_pretty(&manifest)?);
    std::fs::write(dir.join("run-manifest.json"), manifest_json)?;
    Ok(())
}

fn source_anchor(span: &domain_extract::SourceSpan) -> serde_json::Value {
    serde_json::json!({
        "path": span.path,
        "line_start": span.start_line,
        "line_end": span.end_line,
        "symbol": null,
        "excerpt": null,
    })
}

fn source_hint_metadata(span: &domain_extract::SourceSpan, claim_limit: &str) -> serde_json::Value {
    serde_json::json!({
        "confidence": "derived",
        "ratification": "pending",
        "rationale": claim_limit,
        "source_anchors": [source_anchor(span)],
        "verification_lanes": ["manual", "crucible"]
    })
}

pub(crate) fn dossier_runtime_label(runtime: &probe::Runtime) -> &'static str {
    match runtime {
        probe::Runtime::Anchor => "anchor",
        probe::Runtime::Native => "native-rust",
        probe::Runtime::Sbpf => "sbpf",
        probe::Runtime::Quasar => "quasar",
        probe::Runtime::QedgenCodegen => "qedgen-codegen",
        probe::Runtime::Pinocchio => "pinocchio",
        probe::Runtime::Unknown => "unknown",
    }
}

/// Preserve source-derived domain artifacts when runtime-specific probing is
/// unavailable. The dossier remains usable for manual review and intent
/// ratification; the manifest makes the missing executable lane explicit.
pub(crate) fn mark_ordinary_probe_blocked(
    audit_dir: &Path,
    reason: &str,
    resume_command: &str,
) -> Result<()> {
    let path = audit_dir.join("run-manifest.json");
    let mut manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    manifest["status"] = serde_json::Value::String("tooling-blocked".to_string());
    if let Some(lane) = manifest
        .get_mut("lanes")
        .and_then(serde_json::Value::as_array_mut)
        .and_then(|lanes| {
            lanes
                .iter_mut()
                .find(|lane| lane["name"] == "ordinary-probe")
        })
    {
        lane["status"] = serde_json::Value::String("blocked".to_string());
        lane["reason"] = serde_json::Value::String(reason.to_string());
        lane["resume_command"] = serde_json::Value::String(resume_command.to_string());
        lane["artifact_paths"] =
            serde_json::json!(["domain-dossier.json", "domain-interview.json"]);
    }
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&manifest)?),
    )?;
    Ok(())
}

/// Emit an attribute report to `--out` or stdout — shared by `stamp` and
/// the deprecated `adapt --spec` alias.
pub(crate) fn write_attribute_report(rendered: &str, out: Option<&Path>) -> Result<()> {
    if let Some(path) = out {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, rendered)?;
        eprintln!("Wrote {} ({} bytes)", path.display(), rendered.len());
    } else {
        print!("{}", rendered);
    }
    Ok(())
}

/// Persist the `stamp` gate's evidence record after a verify run.
/// Best-effort by contract: a failed write must not turn a green verify
/// red (the record is a convenience for `stamp`, not a verify output).
pub(crate) fn record_verify_evidence(
    spec: &Path,
    program: Option<&Path>,
    report: &crate::verify::VerifyReport,
    kani_impl_bound: bool,
    probe_repros_passed: Option<bool>,
    obligations_digest: Option<String>,
) {
    match crate::verify::evidence::build(
        spec,
        program,
        report,
        kani_impl_bound,
        probe_repros_passed,
        obligations_digest,
    )
    .and_then(|e| crate::verify::evidence::record(&e, spec))
    {
        Ok(path) => eprintln!("Recorded verification evidence at {}", path.display()),
        Err(e) => eprintln!("warning: failed to record verification evidence: {e}"),
    }
}

fn stable_audit_id(program_name: &str) -> String {
    let normalized: String = program_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let normalized = normalized.trim_matches('_');
    if normalized.is_empty() {
        "audit_program".to_string()
    } else {
        format!("audit_{normalized}")
    }
}
/// Redirect a `…/tests/kani_impl.rs` path to a sibling `…/src/kani_impl.rs`.
/// Pinocchio Kani harnesses must live in the lib (`src/`) because
/// `cargo kani` only discovers `#[kani::proof]` there, not in `tests/`
/// (M1 smoke-test finding, design doc §11a). Paths whose parent is not
/// `tests` pass through unchanged so an explicit `--kani-impl-output`
/// override is respected.
pub(crate) fn redirect_kani_impl_to_src(path: &std::path::Path) -> PathBuf {
    let file = path
        .file_name()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("kani_impl.rs"));
    match path.parent() {
        Some(parent) if parent.file_name().and_then(|s| s.to_str()) == Some("tests") => {
            // …/tests/kani_impl.rs → …/src/kani_impl.rs
            parent.parent().unwrap_or(parent).join("src").join(&file)
        }
        _ => path.to_path_buf(),
    }
}

/// Walk up from `start` looking for a `.git` directory. Returns true if one
/// is found before hitting the filesystem root. qedgen refuses to write
/// scaffolding unless the user has a git repo — the safety net for
/// regeneration is a clean working tree.
pub(crate) fn has_git_repo(start: &std::path::Path) -> bool {
    let mut cur = match start.canonicalize() {
        Ok(p) => p,
        Err(_) => start.to_path_buf(),
    };
    loop {
        if cur.join(".git").exists() {
            return true;
        }
        match cur.parent() {
            Some(p) => cur = p.to_path_buf(),
            None => return false,
        }
    }
}

pub(crate) fn require_git_repo() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    if !has_git_repo(&cwd) {
        eprintln!("qedgen requires a git repo — run `git init` first");
        std::process::exit(1);
    }
    Ok(())
}

/// v2.18 P3 alias: wrap a Crucible fuzz-probe run into a single
/// BackendReport so `qedgen verify --crucible <budget>` renders through
/// the v2.17 named-counterexample human surface alongside the other
/// backends. Each finding's action sequence becomes a counterexample;
/// the harness path lives in BackendReport.detail for context.
pub(crate) fn crucible_backend_report(
    spec: &Path,
    harness_dir: Option<PathBuf>,
    budget_secs: u64,
    no_smoke: bool,
    stateful: bool,
) -> verify::BackendReport {
    use std::time::Instant;
    let start = Instant::now();

    let project_root = spec
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let parsed = match check::parse_spec_file(spec) {
        Ok(p) => p,
        Err(e) => {
            return verify::BackendReport {
                name: "crucible",
                status: verify::BackendStatus::Failed,
                duration_ms: start.elapsed().as_millis(),
                detail: Some(format!("failed to parse spec: {e}")),
                log_path: None,
                counterexamples: Vec::new(),
                axioms: Vec::new(),
            }
        }
    };
    let prog = crucible_gen::spec_program_name(&parsed);
    let harness = harness_dir.unwrap_or_else(|| project_root.join("fuzz").join(&prog));

    let mut ctx = crucible_probe::FuzzProbeContext::new(spec, project_root, harness.clone());
    ctx.fuzz_budget = std::time::Duration::from_secs(budget_secs);
    if no_smoke {
        ctx.smoke_budget = std::time::Duration::ZERO;
    }
    ctx.stateful = stateful;

    let findings = match crucible_probe::run_fuzz_probe(&ctx) {
        Ok(r) => r.findings,
        Err(e) => {
            return verify::BackendReport {
                name: "crucible",
                status: verify::BackendStatus::Failed,
                duration_ms: start.elapsed().as_millis(),
                detail: Some(format!("crucible run failed: {e:#}")),
                log_path: None,
                counterexamples: Vec::new(),
                axioms: Vec::new(),
            }
        }
    };

    let duration_ms = start.elapsed().as_millis();
    let status = if findings.is_empty() {
        verify::BackendStatus::Passed
    } else {
        verify::BackendStatus::Failed
    };

    let counterexamples = findings
        .iter()
        .map(crucible_finding_to_counterexample)
        .collect::<Vec<_>>();

    let detail = if findings.is_empty() {
        Some(format!(
            "no findings in {}s ({} budget). \
             Pass `--crucible <larger>` to go deeper, or `--crucible-stateful` for chain coverage.",
            budget_secs, budget_secs
        ))
    } else {
        Some(format!(
            "{} distinct finding(s). \
             Replay via `crucible show {} <crash> --replay`.",
            findings.len(),
            harness.display(),
        ))
    };

    verify::BackendReport {
        name: "crucible",
        status,
        duration_ms,
        detail,
        log_path: None,
        counterexamples,
        axioms: Vec::new(),
    }
}

/// Map a Crucible Finding into the structured Counterexample shape the
/// v2.17 human renderer consumes. Action sequence flattens to one
/// (name, value) row per action, plus a leading row for the violation
/// category.
pub(crate) fn crucible_finding_to_counterexample(
    f: &probe::Finding,
) -> verify_counterexample::Counterexample {
    use verify_counterexample::{Counterexample, CounterexampleVar};
    let mut assignments = Vec::new();
    assignments.push(CounterexampleVar {
        name: "category".to_string(),
        value: f.category_tag.clone(),
        line: None,
    });
    if let Some(probe::Reproducer::Crucible {
        action_sequence,
        crucible_version,
        ..
    }) = &f.reproducer
    {
        for (i, action) in action_sequence.iter().enumerate() {
            assignments.push(CounterexampleVar {
                name: format!("action[{}]", i),
                value: format!(
                    "{}({}){}",
                    action.name,
                    serde_json::to_string(&action.params).unwrap_or_default(),
                    action
                        .error_code
                        .map(|c| format!(" → Custom({})", c))
                        .unwrap_or_else(|| if action.success {
                            " → ok".into()
                        } else {
                            " → fail".into()
                        }),
                ),
                line: None,
            });
        }
        assignments.push(CounterexampleVar {
            name: "crucible_version".to_string(),
            value: crucible_version.clone(),
            line: None,
        });
    }
    Counterexample {
        harness: format!("{} ({})", f.handler, f.category_tag),
        status: "failed".to_string(),
        assignments,
        seed: None,
        failure_message: Some(f.spec_silent_on.clone()),
        source_location: f.reproducer.as_ref().and_then(|r| match r {
            probe::Reproducer::Crucible { crash_path, .. } => Some(crash_path.clone()),
            _ => None,
        }),
    }
}

/// Expand the committed CI template by substituting `{{VERIFY_STEP}}`
/// and `{{RATCHET_STEP}}` with the caller-provided snippets, then
/// normalise trailing whitespace so the workflow file ends with
/// exactly one newline regardless of whether either step was set.
///
/// Factored out of the `Codegen` match arm so the substitution is
/// unit-testable without spawning a process — the template bytes are
/// `include_str!`'d at compile time, so the test wires them in the
/// same way.
/// Pick the Anchor / Quasar loader for `qedgen readiness` and
/// `qedgen check-upgrade`. Explicit `--quasar` always wins; otherwise
/// the framework is inferred from the project marker in the current
/// working directory (`Quasar.toml` → Quasar; default → Anchor). A
/// short stderr banner lights up the first time autodetect picks
/// Quasar so the dev sees which loader fired without re-reading
/// `--help`. Suppressed under `--json` to keep machine consumers'
/// output clean.
pub(crate) fn resolve_framework(explicit_quasar: bool, as_json: bool) -> ratchet::Framework {
    if explicit_quasar {
        return ratchet::Framework::Quasar;
    }
    let detected = ratchet::Framework::detect_from_cwd();
    if detected == ratchet::Framework::Quasar && !as_json {
        eprintln!(
            "qedgen: Quasar project detected (Quasar.toml in cwd) — using ratchet's Quasar IDL parser"
        );
    }
    detected
}

pub(crate) fn expand_ci_template(template: &str, verify_step: &str, ratchet_step: &str) -> String {
    let mut out = template
        .replace("{{VERIFY_STEP}}", verify_step)
        .replace("{{RATCHET_STEP}}", ratchet_step);
    while out.ends_with('\n') {
        out.pop();
    }
    out.push('\n');
    out
}

pub(crate) fn format_lint_warning(warning: &check::CompletenessWarning) -> String {
    let icon = match warning.severity {
        check::Severity::Error => "E",
        check::Severity::Warning => "!",
        check::Severity::Info => "i",
    };
    let mut out = format!(
        "  {} [P{}] [{}] {}\n    Fix: {}",
        icon, warning.priority, warning.rule, warning.message, warning.fix
    );
    if let Some(ref example) = warning.example {
        out.push_str("\n    Example:");
        for line in example.lines() {
            out.push_str("\n      ");
            out.push_str(line);
        }
    }
    if let Some(ref cx) = warning.counterexample {
        out.push_str("\n    Counterexample:");
        out.push_str(&format!(
            "\n      Pre-state:  {}  →  {} ✓",
            cx.pre_state
                .iter()
                .map(|(f, v)| format!("{} = {}", f, v))
                .collect::<Vec<_>>()
                .join(", "),
            cx.pre_check,
        ));
        out.push_str(&format!(
            "\n      Apply:      {} ({})",
            cx.handler,
            cx.effects.join(", "),
        ));
        out.push_str(&format!(
            "\n      Post-state: {}  →  {} {}",
            cx.post_state
                .iter()
                .map(|(f, v)| format!("{} = {}", f, v))
                .collect::<Vec<_>>()
                .join(", "),
            cx.post_check,
            if cx.invariant_holds { "✓" } else { "✗" },
        ));
    }
    if !warning.fix_options.is_empty() {
        out.push_str("\n    Fix options:");
        for (i, opt) in warning.fix_options.iter().enumerate() {
            let label = (b'A' + i as u8) as char;
            out.push_str(&format!(
                "\n      {}) {} — {}",
                label, opt.label, opt.rationale
            ));
            for line in opt.snippet.lines() {
                out.push_str(&format!("\n         {}", line));
            }
        }
    }
    out
}

/// The runtime-agnostic source scanners — arithmetic-symbol, lifecycle,
/// paired-validator — merged into EVERY `--program` probe envelope. They
/// are framework-neutral by construction (regex over `*.rs`), but until
/// #196 they were wired only into the Pinocchio branch, so Anchor/Native
/// runs returned zero per-site findings from scanners that would have
/// fired on them verbatim.
pub(crate) fn runtime_agnostic_findings(prog_root: &Path) -> Result<Vec<probe::Finding>> {
    let mut findings = arithmetic_symbol_probe::scan_program(prog_root)?;
    findings.extend(paired_validator_probe::scan_program(prog_root)?);
    findings.extend(lifecycle_probe::scan_program(prog_root)?);
    Ok(findings)
}

/// v3 (#227): report the source-walk engine's status for a `--program`
/// envelope. The regex scanners silently `continue` past any `.rs` file
/// they can't read; this re-walks the same file set and surfaces the
/// unreadable ones as a `partial` engine run so a zero-finding result
/// can't hide half-skipped source. Returns a single `EngineRun` named
/// `source_scan` (the arithmetic-symbol / paired-validator / lifecycle
/// scanners share one file walk, so one status covers them).
pub(crate) fn source_scan_engine_run(prog_root: &Path) -> probe::EngineRun {
    let src_dir = prog_root.join("src");
    let files = crate::fs_walk::collect_rs_files(&src_dir, crate::fs_walk::DEFAULT_SKIP_DIRS);
    let mut skipped: Vec<String> = files
        .iter()
        .filter(|f| std::fs::read_to_string(f).is_err())
        .map(|f| f.strip_prefix(prog_root).unwrap_or(f).display().to_string())
        .collect();
    skipped.sort();
    if skipped.is_empty() {
        probe::EngineRun {
            engine: "source_scan".to_string(),
            status: probe::EngineStatus::Passed,
            detail: None,
            candidates_dropped: 0,
            skipped_files: Vec::new(),
        }
    } else {
        probe::EngineRun {
            engine: "source_scan".to_string(),
            status: probe::EngineStatus::Partial,
            detail: Some(format!("{} source file(s) unreadable", skipped.len())),
            candidates_dropped: 0,
            skipped_files: skipped,
        }
    }
}

/// Outcome for a `--program` source-walk envelope: a `partial` engine run
/// downgrades an empty result to low-coverage; otherwise the scan is a
/// real (scanner-scoped) pass.
pub(crate) fn source_scan_outcome(engine: &probe::EngineRun) -> probe::ProbeOutcome {
    if engine.status == probe::EngineStatus::Partial {
        probe::ProbeOutcome::NoFindingsLowCoverage
    } else {
        probe::ProbeOutcome::PassedWithCoverage
    }
}

/// Anchor (and Quasar) probe path used by `qedgen probe --program <root>`.
/// Mirrors the Pinocchio branch's shape: runs the runtime-specific
/// extractor, clusters proto-clauses, optionally materializes the audit
/// working set, and prints the schema-v3 envelope. Per-site findings come
/// from the runtime-agnostic scanners (#196); Anchor-specific site
/// discovery stays at the agent layer (auditor SKILL.md via Read+Grep),
/// while the scaffold-to-spec interview works off the extractor's
/// clusters directly.
pub(crate) fn run_anchor_probe(
    prog_root: &Path,
    runtime_final: probe::Runtime,
    emit_spec_candidates: bool,
    audit_dir: Option<&Path>,
) -> Result<()> {
    let applicable = probe::applicable_categories_public(&runtime_final);
    // Source-walk enumerator (+ #235 IDL overlay applied inside
    // `run_bootstrap`); empty on non-standard layouts (don't fail —
    // ratify continues with what it has).
    let (handlers_opt, idl_path, derivable_idl, drift_candidates) =
        match probe::run_bootstrap(prog_root) {
            Ok(bs) => (bs.handlers, bs.idl_path, bs.derivable_idl, bs.candidates),
            Err(_) => (None, None, None, Vec::new()),
        };
    let clusters = if emit_spec_candidates {
        let protos = anchor_extractor::extract_proto_clauses(prog_root)?;
        Some(cluster::cluster_protos(protos))
    } else {
        None
    };

    if let (Some(dir), Some(clusters_ref)) = (audit_dir, clusters.as_ref()) {
        // The Anchor-compatible structural adapter feeds the pre-interview
        // skeleton; on failure fall back to a minimal stub ratify still
        // accepts (lenient_skeleton).
        write_audit_working_set(
            dir,
            prog_root,
            clusters_ref,
            program_model::ProgramFramework::Anchor,
            dossier_runtime_label(&runtime_final),
            /*lenient_skeleton=*/ true,
        )?;
    }

    let engine_run = source_scan_engine_run(prog_root);
    let outcome = source_scan_outcome(&engine_run);
    let mut output = probe::ProbeOutput {
        project_root: Some(prog_root.display().to_string()),
        runtime: Some(runtime_final),
        handlers: handlers_opt,
        applicable_categories: Some(applicable),
        findings: runtime_agnostic_findings(prog_root)?,
        candidates: drift_candidates,
        engine_runs: vec![engine_run],
        outcome,
        clusters,
        idl_path,
        derivable_idl,
        ..probe::ProbeOutput::envelope(probe::Mode::SpecLess)
    };
    probe::finalize_specless(&mut output, prog_root, audit_dir);
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Native (solana-program) probe path. Same envelope shape as Anchor;
/// reuses the Pinocchio-style source-walk for the skeleton because
/// Native has no IDL to drive a richer emitter.
pub(crate) fn run_native_probe(
    prog_root: &Path,
    runtime_final: probe::Runtime,
    emit_spec_candidates: bool,
    audit_dir: Option<&Path>,
) -> Result<()> {
    let applicable = probe::applicable_categories_public(&runtime_final);
    let clusters = if emit_spec_candidates {
        let protos = native_extractor::extract_proto_clauses(prog_root)?;
        Some(cluster::cluster_protos(protos))
    } else {
        None
    };

    if let (Some(dir), Some(clusters_ref)) = (audit_dir, clusters.as_ref()) {
        // Native skeleton accepts any `pub fn` (no `process_*`-style naming
        // convention to key on).
        write_audit_working_set(
            dir,
            prog_root,
            clusters_ref,
            program_model::ProgramFramework::Native,
            dossier_runtime_label(&runtime_final),
            /*lenient_skeleton=*/ false,
        )?;
    }

    // Shank central-match dispatcher detection — the richer envelope
    // (handlers + dispatcher_kind) is purely additive. Each handler body is
    // also classified to narrow `applicable_categories`.
    let (handlers, dispatcher_kind) = match shank_probe::detect_shank_dispatcher(prog_root)
        .ok()
        .flatten()
    {
        Some(cat) => {
            let hs: Vec<probe::BootstrapHandler> = cat
                .handlers
                .into_iter()
                .map(|sh| {
                    let (intent_tag, narrowed) =
                        narrow_shank_handler(&sh.name, &sh.entry_fn, prog_root, &applicable);
                    probe::BootstrapHandler {
                        name: sh.name,
                        source_file: sh.file,
                        enum_variant: Some(sh.enum_variant),
                        entry_fn: Some(sh.entry_fn),
                        line: Some(sh.line),
                        applicable_categories: narrowed,
                        intent_tag,
                        idl_accounts: None,
                        idl_args: None,
                        discovered_via: None,
                    }
                })
                .collect();
            (Some(hs), Some("shank_central_match".to_string()))
        }
        None => (None, None),
    };

    // #235 IDL overlay: enrich Shank-discovered handlers with account/arg
    // metas (declarative flags — no category narrowing on native), fill an
    // empty handler list from an on-disk IDL, surface drift as candidates.
    let mut handler_vec = handlers.unwrap_or_default();
    let overlay =
        probe::idl_overlay::apply(prog_root, &runtime_final, &mut handler_vec, &applicable);
    let handlers = (!handler_vec.is_empty()).then_some(handler_vec);

    let engine_run = source_scan_engine_run(prog_root);
    let outcome = source_scan_outcome(&engine_run);
    let mut output = probe::ProbeOutput {
        project_root: Some(prog_root.display().to_string()),
        runtime: Some(runtime_final),
        handlers,
        applicable_categories: Some(applicable),
        findings: runtime_agnostic_findings(prog_root)?,
        candidates: overlay.drift_candidates,
        engine_runs: vec![engine_run],
        outcome,
        clusters,
        idl_path: overlay.idl_path,
        derivable_idl: overlay.derivable_idl,
        dispatcher_kind,
        ..probe::ProbeOutput::envelope(probe::Mode::SpecLess)
    };
    probe::finalize_specless(&mut output, prog_root, audit_dir);
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// v2.20 §S2.2 helper for the `--program` native flow. Mirrors the
/// `probe::run_bootstrap` path: resolves the handler body, classifies
/// intent, and returns `(intent_tag_str, narrowed_categories)`. The
/// narrowed list is only emitted when the classifier actually drops
/// at least one category; an unchanged list is reported as `None`
/// so the global `applicable_categories` field stays authoritative.
pub(crate) fn narrow_shank_handler(
    handler_name: &str,
    entry_fn: &str,
    project_root: &Path,
    global: &[String],
) -> (Option<String>, Option<Vec<String>>) {
    let Some((_path, body)) = handler_intent::resolve_handler_body(entry_fn, project_root) else {
        return (None, None);
    };
    let tag = handler_intent::classify_handler_body(handler_name, &body);
    let tag_str = tag.map(|t| t.as_str().to_string());
    let narrowed = handler_intent::filter_categories(global, tag);
    if narrowed.len() == global.len() {
        return (tag_str, None);
    }
    (tag_str, Some(narrowed))
}

#[cfg(test)]
mod tests {
    use super::{
        expand_ci_template, format_lint_warning, redirect_kani_impl_to_src,
        runtime_agnostic_findings, write_audit_working_set,
    };
    use crate::check::{CompletenessWarning, Severity};
    use crate::cluster::{cluster_protos, ClusterKind, ProtoClause};
    use crate::program_model::ProgramFramework;
    use std::path::PathBuf;

    /// #290: resolved codegen output paths must never render a literal
    /// `/./` — clap defaults are `./x` and `PathBuf::join` preserves
    /// the `CurDir` component.
    #[test]
    fn resolved_output_paths_have_no_curdir_segment() {
        let resolved = super::resolve_output_against_spec(
            std::path::Path::new("/abs/path/to/project"),
            PathBuf::from("./programs"),
        );
        assert_eq!(resolved, PathBuf::from("/abs/path/to/project/programs"));
        assert!(
            !resolved.display().to_string().contains("/./"),
            "resolved path renders a literal /./: {}",
            resolved.display()
        );
        // Absolute paths pass through untouched.
        assert_eq!(
            super::resolve_output_against_spec(
                std::path::Path::new("/spec/dir"),
                PathBuf::from("/elsewhere/out")
            ),
            PathBuf::from("/elsewhere/out")
        );
    }

    /// #289: `--program .` resolves to a directory with a real name
    /// (`file_name()` is no longer `None`); nonexistent roots fall back
    /// to a lexical cwd-join that keeps the typed name.
    #[test]
    fn canonicalize_program_root_gives_dot_a_real_name() {
        let root = super::canonicalize_program_root(std::path::Path::new("."));
        assert!(root.is_absolute());
        assert!(
            root.file_name().is_some(),
            "`.` must resolve to a named directory, got {}",
            root.display()
        );
        let missing = super::canonicalize_program_root(std::path::Path::new("./no-such-dir-289"));
        assert!(missing.is_absolute());
        assert_eq!(
            missing.file_name().and_then(|n| n.to_str()),
            Some("no-such-dir-289")
        );
    }

    /// #196: the runtime-agnostic scanners fire on an ANCHOR-shaped crate —
    /// they used to be wired only into the Pinocchio probe branch, leaving
    /// Anchor/Native `--program` runs with zero per-site findings.
    #[test]
    fn agnostic_findings_fire_on_anchor_shaped_source() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        // The canonical silent_success_arithmetic shape (timestamp-shaped
        // saturating_sub + downstream gating comparison) inside an
        // Anchor-flavored handler.
        std::fs::write(
            dir.path().join("src/lib.rs"),
            r#"
use anchor_lang::prelude::*;
pub fn process_transfer(ctx: Context<T>, current_ts: i64) -> Result<()> {
    let elapsed = current_ts.saturating_sub(*period_start_ts);
    if elapsed >= period_length {
        advance_period(ctx)?;
    }
    Ok(())
}
"#,
        )
        .unwrap();
        let findings = runtime_agnostic_findings(dir.path()).unwrap();
        assert!(
            findings
                .iter()
                .any(|f| f.category_tag == "silent_success_arithmetic"),
            "agnostic scanner must fire on Anchor-shaped source; got {findings:#?}"
        );
    }

    #[test]
    fn audit_working_set_seeds_domain_dossier_and_run_manifest() {
        let root = tempfile::tempdir().unwrap();
        let audit_dir = root.path().join("audit");
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("src/lib.rs"),
            "pub fn update_fee(fee_bps: u16) {}\n",
        )
        .unwrap();
        let clusters = cluster_protos(vec![ProtoClause {
            kind: ClusterKind::AccountSignerCheck,
            handler: "update_fee".to_string(),
            finding_id: "finding_fee_signer".to_string(),
            evidence_text: "admin.is_signer".to_string(),
        }]);

        write_audit_working_set(
            &audit_dir,
            root.path(),
            &clusters,
            ProgramFramework::Native,
            "native-rust",
            false,
        )
        .unwrap();

        let dossier: serde_json::Value =
            serde_json::from_slice(&std::fs::read(audit_dir.join("domain-dossier.json")).unwrap())
                .unwrap();
        assert_eq!(dossier["schema_version"], 1);
        assert_eq!(
            dossier["schema_uri"],
            "https://qedgen.dev/schemas/auditor/domain-dossier-v1.schema.json"
        );
        assert_eq!(dossier["target"]["runtime"], "native-rust");
        assert_eq!(dossier["handlers"][0]["name"], "update_fee");
        assert_eq!(dossier["structural_candidates"][0]["id"], clusters[0].id);
        assert_eq!(
            dossier["structural_candidates"][0]["ratification"],
            "pending"
        );
        assert_eq!(dossier["economic_equations"], serde_json::json!([]));

        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(audit_dir.join("run-manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["status"], "running");
        assert_eq!(
            manifest["schema_uri"],
            "https://qedgen.dev/schemas/auditor/audit-run-manifest-v1.schema.json"
        );
        assert!(manifest["lanes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|lane| lane["name"] == "ordinary-probe" && lane["status"] == "passed"));
        assert!(audit_dir.join("domain-dossier.md").is_file());
    }

    #[test]
    fn kani_impl_path_redirects_tests_to_src_for_pinocchio() {
        // Default Anchor-shaped path → sibling src/.
        assert_eq!(
            redirect_kani_impl_to_src(&PathBuf::from("./programs/tests/kani_impl.rs")),
            PathBuf::from("./programs/src/kani_impl.rs"),
        );
        // Bare tests/ root → src/.
        assert_eq!(
            redirect_kani_impl_to_src(&PathBuf::from("tests/kani_impl.rs")),
            PathBuf::from("src/kani_impl.rs"),
        );
        // Non-tests parent passes through (explicit override respected).
        assert_eq!(
            redirect_kani_impl_to_src(&PathBuf::from("./custom/kani_impl.rs")),
            PathBuf::from("./custom/kani_impl.rs"),
        );
        assert_eq!(
            redirect_kani_impl_to_src(&PathBuf::from("./programs/src/kani_impl.rs")),
            PathBuf::from("./programs/src/kani_impl.rs"),
        );
    }

    #[test]
    fn plain_text_lint_output_includes_priority() {
        let warning = CompletenessWarning::new(
            "missing_effect",
            Severity::Warning,
            2,
            "operation 'borrow' takes params and transitions state but has no effect",
        )
        .subject("borrow".to_string())
        .fix("Add an effect block to describe state changes")
        .example("  operation borrow\n    effect: loan_amount add loan_amount".to_string());

        let rendered = format_lint_warning(&warning);
        assert!(rendered.contains("[P2] [missing_effect]"));
        assert!(rendered.contains("Fix: Add an effect block to describe state changes"));
        assert!(rendered.contains("Example:"));
    }

    // verify.yml carries {{VERIFY_STEP}} and {{RATCHET_STEP}} placeholders;
    // a refactor that drops or mangles either would be invisible elsewhere —
    // these three tests catch that cheaply.
    const CI_TEMPLATE: &str = include_str!("../../../templates/verify.yml");

    #[test]
    fn ci_template_unset_placeholders_produce_clean_workflow() {
        let out = expand_ci_template(CI_TEMPLATE, "", "");
        // Both placeholders fully consumed.
        assert!(!out.contains("{{VERIFY_STEP}}"));
        assert!(!out.contains("{{RATCHET_STEP}}"));
        // Neither optional step present when unset.
        assert!(!out.contains("Verify sBPF binary"));
        assert!(!out.contains("Ratchet readiness lint"));
        // Core workflow still intact.
        assert!(out.contains("Check spec coverage"));
        assert!(out.contains("Build proofs"));
        // Exactly one trailing newline — no blank line at EOF.
        assert!(out.ends_with('\n'));
        assert!(!out.ends_with("\n\n"));
    }

    #[test]
    fn ci_template_ratchet_step_injects_readiness_job() {
        let ratchet = "\n      - name: Ratchet readiness lint\n        run: qedgen readiness --idl target/idl/escrow.json\n";
        let out = expand_ci_template(CI_TEMPLATE, "", ratchet);
        assert!(out.contains("Ratchet readiness lint"));
        assert!(out.contains("qedgen readiness --idl target/idl/escrow.json"));
        assert!(!out.contains("{{RATCHET_STEP}}"));
        assert!(out.ends_with('\n'));
        assert!(!out.ends_with("\n\n"));
    }

    #[test]
    fn ci_template_both_steps_coexist_without_collision() {
        let verify = "\n      - name: Verify sBPF binary\n        run: qedgen check --spec program.qedspec --asm src/program.s\n";
        let ratchet = "\n      - name: Ratchet readiness lint\n        run: qedgen readiness --idl target/idl/x.json\n";
        let out = expand_ci_template(CI_TEMPLATE, verify, ratchet);
        assert!(out.contains("Verify sBPF binary"));
        assert!(out.contains("Ratchet readiness lint"));
        // sBPF step precedes proof build; ratchet step follows spec coverage.
        let verify_pos = out.find("Verify sBPF binary").unwrap();
        let proofs_pos = out.find("Build proofs").unwrap();
        let coverage_pos = out.find("Check spec coverage").unwrap();
        let ratchet_pos = out.find("Ratchet readiness lint").unwrap();
        assert!(verify_pos < proofs_pos);
        assert!(coverage_pos < ratchet_pos);
    }
}
