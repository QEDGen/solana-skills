//! Dispatch-support glue for `run::dispatch`: git-repo guards, the
//! Crucible-backend bridge, CI-template expansion, lint-warning
//! formatting, and the Anchor/native probe runners. Split out of
//! `main.rs` (v3.0 prep). `use crate::*` pulls in the crate-root
//! re-export hub (the `crate::<module>` aliases these helpers reach).

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

/// Materialize the audit working set (`interview.md`, `clusters.json`,
/// `skeleton.qedspec`) under `dir` — shared by the Pinocchio (run.rs),
/// Anchor, and Native probe paths, which previously each carried this
/// block inline. `lenient_skeleton`: the Anchor path falls back to a
/// minimal stub when the structural adapter fails (non-standard
/// layouts); the other paths propagate the error.
pub(crate) fn write_audit_working_set(
    dir: &Path,
    prog_root: &Path,
    clusters: &[cluster::Cluster],
    framework: program_model::ProgramFramework,
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
    let skeleton = match adapt::render_skeleton_for_framework(framework, prog_root, adapter_config)
    {
        Ok(s) => s,
        Err(e) if lenient_skeleton => {
            eprintln!(
                "warning: program adapter failed ({}); writing minimal skeleton",
                e
            );
            format!(
                "spec {}\n\ntype State | Init | Active\ntype Error | InvalidArgument\n",
                program_name
            )
        }
        Err(e) => return Err(e),
    };
    std::fs::write(dir.join("skeleton.qedspec"), skeleton)?;
    eprintln!("Wrote audit working set to {}", dir.display());
    Ok(())
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
        Ok(f) => f,
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
    // IDL-aware enumerator; empty on non-standard layouts (don't fail —
    // ratify continues with what it has).
    let handlers_opt = match probe::run_bootstrap(prog_root) {
        Ok(bs) => bs.handlers,
        Err(_) => None,
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
            /*lenient_skeleton=*/ true,
        )?;
    }

    let output = probe::ProbeOutput {
        version: probe::schema_version(),
        mode: probe::Mode::SpecLess,
        spec_path: None,
        project_root: Some(prog_root.display().to_string()),
        runtime: Some(runtime_final),
        handlers: handlers_opt,
        applicable_categories: Some(applicable),
        findings: runtime_agnostic_findings(prog_root)?,
        clusters,
        dispatcher_kind: None,
    };
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
                    }
                })
                .collect();
            (Some(hs), Some("shank_central_match".to_string()))
        }
        None => (None, None),
    };

    let output = probe::ProbeOutput {
        version: probe::schema_version(),
        mode: probe::Mode::SpecLess,
        spec_path: None,
        project_root: Some(prog_root.display().to_string()),
        runtime: Some(runtime_final),
        handlers,
        applicable_categories: Some(applicable),
        findings: runtime_agnostic_findings(prog_root)?,
        clusters,
        dispatcher_kind,
    };
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
        runtime_agnostic_findings,
    };
    use crate::check::{CompletenessWarning, Severity};
    use std::path::PathBuf;

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
