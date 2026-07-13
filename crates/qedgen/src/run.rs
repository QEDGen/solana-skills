//! Command dispatch: `command_name_of` (telemetry/last-error label) and
//! the `dispatch` match that routes every parsed `Commands` variant to its
//! implementation. Split out of `main.rs` (v3.0 prep). `use crate::*` pulls
//! in the crate-root re-export hub (module aliases + the `cli` types);
//! `use crate::run_helpers::*` pulls in the dispatch-support helpers.

use crate::run_helpers::*;
use crate::*;
use anyhow::{Context as _, Result};
use std::path::{Path, PathBuf};

/// #182 Shape-1: deliver the `qedgen_kani_prelude` crate beside a just-generated
/// Kani harness and depend on it — but only when the harness actually references
/// it. The Pubkey/div stubs (impl path) and the `mul_div` imports (spec-model
/// path) are emitted conditionally, so gating on the emitted text is precise
/// across every harness shape.
fn deliver_prelude_if_referenced(harness_path: &Path) -> Result<()> {
    let refs_prelude = std::fs::read_to_string(harness_path)
        .map(|s| s.contains("qedgen_kani_prelude"))
        .unwrap_or(false);
    if refs_prelude {
        crate::project::deliver_kani_prelude_for_harness(harness_path)?;
    }
    Ok(())
}

/// Top-level subcommand name for telemetry and the last-error log
/// header. Aristotle's sub-verbs collapse to the single `"aristotle"`
/// label — that's the user-facing surface they invoked.
pub(crate) fn command_name_of(c: &Commands) -> &'static str {
    match c {
        Commands::Generate { .. } => "generate",
        Commands::FillSorry { .. } => "fill-sorry",
        Commands::Adapt { .. } => "adapt",
        Commands::Interface { .. } => "interface",
        Commands::Probe { .. } => "probe",
        Commands::Ratify { .. } => "ratify",
        Commands::Spec { .. } => "spec",
        Commands::Descriptor { .. } => "descriptor",
        Commands::Discharge { .. } => "discharge",
        Commands::Consolidate { .. } => "consolidate",
        Commands::Asm2Lean { .. } => "asm2lean",
        Commands::Setup { .. } => "setup",
        Commands::Init { .. } => "init",
        Commands::Check { .. } => "check",
        Commands::Verify { .. } => "verify",
        Commands::Readiness { .. } => "readiness",
        Commands::CheckUpgrade { .. } => "check-upgrade",
        Commands::Codegen { .. } => "codegen",
        Commands::Aristotle(_) => "aristotle",
        Commands::Reconcile { .. } => "reconcile",
        Commands::Feedback { .. } => "feedback",
    }
}

pub(crate) async fn dispatch(cmd: Commands) -> Result<()> {
    match cmd {
        Commands::Generate {
            prompt_file,
            output_dir,
            passes,
            temperature,
            max_tokens,
            validate,
            mathlib,
        } => {
            validate_dispatch_args(passes, temperature, max_tokens)?;
            if validate {
                deps::require_lean()?;
            }
            let prompt = std::fs::read_to_string(&prompt_file)?;
            api::generate_proofs(
                &prompt,
                &output_dir,
                passes,
                temperature,
                max_tokens,
                validate,
                None,
                mathlib,
            )
            .await?;
        }

        Commands::FillSorry {
            file,
            output,
            passes,
            temperature,
            max_tokens,
            validate,
            escalate,
        } => {
            validate_dispatch_args(passes, temperature, max_tokens)?;
            if validate {
                deps::require_lean()?;
            }
            api::fill_sorry(
                &file,
                output.as_deref(),
                passes,
                temperature,
                max_tokens,
                validate,
            )
            .await?;

            if escalate {
                let result_path = output.as_deref().unwrap_or(&file);
                let content = std::fs::read_to_string(result_path)?;
                if content.contains("sorry") {
                    eprintln!("\nSorry markers remain after Leanstral. Escalating to Aristotle...");
                    // Derive project dir from the file path (go up to lakefile.lean)
                    let project_dir = result_path
                        .parent()
                        .and_then(|p| {
                            if p.join("lakefile.lean").exists() {
                                Some(p.to_path_buf())
                            } else {
                                p.parent().and_then(|pp| {
                                    if pp.join("lakefile.lean").exists() {
                                        Some(pp.to_path_buf())
                                    } else {
                                        None
                                    }
                                })
                            }
                        })
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "Could not find lakefile.lean above {}. \
                                 Run `qedgen aristotle submit` manually with --project-dir.",
                                result_path.display()
                            )
                        })?;
                    let prompt = "Fill in all sorry placeholders with valid proofs".to_string();
                    aristotle::fill_sorry(&project_dir, &project_dir, &prompt, true, None).await?;
                } else {
                    eprintln!("All sorry markers filled by Leanstral.");
                }
            }
        }

        Commands::Adapt {
            program,
            spec,
            out,
            handler_overrides,
        } => {
            let mut overrides = std::collections::HashMap::new();
            for raw in &handler_overrides {
                let (name, parsed) = anchor_adapt::parse_handler_override(raw)?;
                overrides.insert(name, parsed);
            }
            match spec {
                Some(spec_path) => {
                    let entries =
                        anchor_adapt::compute_attributes(&program, &spec_path, &overrides)?;
                    let rendered = anchor_adapt::render_attributes(&entries);
                    if let Some(path) = out {
                        if let Some(parent) = path.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::write(&path, &rendered)?;
                        eprintln!("Wrote {} ({} bytes)", path.display(), rendered.len());
                    } else {
                        print!("{}", rendered);
                    }
                }
                None => {
                    let program_name = adapt::default_program_name(&program);
                    let adapter_config = adapt::AdapterConfig::new(&program_name, &overrides);
                    if let Some(path) = out {
                        adapt::render_skeleton_to_file(&program, &path, adapter_config)?;
                    } else {
                        let rendered = adapt::render_skeleton(&program, adapter_config)?;
                        print!("{}", rendered);
                    }
                }
            }
        }

        Commands::Interface { idl, out, vendor } => {
            if vendor {
                // `.qed/interfaces/<idl-stem>.qedspec`, resolved via the
                // nearest `.qed/` ancestor of cwd.
                let cwd = std::env::current_dir()?;
                let (qed_dir, config) = init::discover_qed_config(&cwd).ok_or_else(|| {
                    anyhow::anyhow!(
                        "--vendor requires a `.qed/` ancestor of {} — run `qedgen init` first or pass `--out`",
                        cwd.display()
                    )
                })?;
                let project_root = qed_dir.parent().unwrap_or(std::path::Path::new("."));
                let interfaces_dir = project_root.join(
                    config
                        .interfaces_dir
                        .as_deref()
                        .unwrap_or(".qed/interfaces"),
                );
                let stem = idl
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("interface");
                let target = interfaces_dir.join(format!("{}.qedspec", stem));
                interface_gen::generate_to_file(&idl, &target)?;
                eprintln!("Vendored interface to {}", target.display());
            } else if let Some(path) = out {
                interface_gen::generate_to_file(&idl, &path)?;
                eprintln!("Wrote Tier-0 interface to {}", path.display());
            } else {
                let rendered = interface_gen::generate(&idl)?;
                print!("{}", rendered);
            }
        }

        Commands::Probe {
            spec,
            bootstrap,
            root,
            program,
            runtime,
            fuzz,
            harness_dir,
            no_smoke,
            stateful,
            emit_spec_candidates,
            audit_dir,
        } => {
            // --program routes through the Pinocchio site enumerator; the
            // envelope's `findings` are the site catalogue mapped 1:1. The
            // audit subagent writes the reproducers.
            if let Some(prog_root) = &program {
                let detected = probe::detect_runtime_public(prog_root);
                let runtime_final = runtime
                    .map(probe::Runtime::from)
                    .unwrap_or_else(|| detected.clone());

                // Anchor/Quasar route through anchor_extractor for
                // scaffold-to-spec interviews; no per-site findings yet
                // (the auditor handles those via Read+Grep).
                if matches!(
                    runtime_final,
                    probe::Runtime::Anchor | probe::Runtime::Quasar
                ) {
                    return run_anchor_probe(
                        prog_root,
                        runtime_final,
                        emit_spec_candidates,
                        audit_dir.as_deref(),
                    );
                }

                // Native (solana-program, no framework) routes through
                // native_extractor; pattern coverage is narrower — see its
                // module docs.
                if matches!(runtime_final, probe::Runtime::Native) {
                    return run_native_probe(
                        prog_root,
                        runtime_final,
                        emit_spec_candidates,
                        audit_dir.as_deref(),
                    );
                }

                if !matches!(runtime_final, probe::Runtime::Pinocchio) {
                    eprintln!(
                        "warning: --program targets {} (detected: {:?}); \
                         emitting bootstrap envelope only. Pass --runtime <name> to force a specific extractor.",
                        prog_root.display(),
                        detected,
                    );
                    let output = probe::run_bootstrap(prog_root)?;
                    println!("{}", serde_json::to_string_pretty(&output)?);
                    return Ok(());
                }
                let catalogue = pinocchio_probe::scan_program(prog_root)?;
                let mut findings = pinocchio_probe::findings_from_catalogue(&catalogue);
                // Runtime-agnostic source scanners (arithmetic-symbol,
                // paired-validator, lifecycle) — same set every runtime
                // branch merges (#196).
                findings.extend(run_helpers::runtime_agnostic_findings(prog_root)?);
                // --emit-spec-candidates: lift findings into proto-clauses,
                // then cluster.
                let clusters = if emit_spec_candidates {
                    let protos = pinocchio_extractor::extract_proto_clauses(&findings);
                    Some(cluster::cluster_protos(protos))
                } else {
                    None
                };
                // --audit-dir: materialize the audit working set
                // (interview.md, clusters.json, skeleton.qedspec).
                if let (Some(dir), Some(clusters_ref)) = (audit_dir.as_ref(), clusters.as_ref()) {
                    write_audit_working_set(
                        dir,
                        prog_root,
                        clusters_ref,
                        program_model::ProgramFramework::Pinocchio,
                        /*lenient_skeleton=*/ false,
                    )?;
                }
                let output = probe::ProbeOutput {
                    version: probe::schema_version(),
                    mode: probe::Mode::SpecLess,
                    spec_path: None,
                    project_root: Some(prog_root.display().to_string()),
                    runtime: Some(probe::Runtime::Pinocchio),
                    handlers: None,
                    applicable_categories: Some(probe::applicable_categories_public(
                        &probe::Runtime::Pinocchio,
                    )),
                    findings,
                    clusters,
                    dispatcher_kind: None,
                };
                // Include the raw catalogue so the subagent has both
                // `findings[]` and the full site list to cross-reference.
                let mut value = serde_json::to_value(&output)?;
                if let Some(obj) = value.as_object_mut() {
                    obj.insert(
                        "pinocchio_catalogue".to_string(),
                        serde_json::to_value(&catalogue)?,
                    );
                }
                println!("{}", serde_json::to_string_pretty(&value)?);
                return Ok(());
            }

            // --fuzz drives the Crucible engine — separate from the
            // pattern-match predicates (run probe twice and merge JSON for
            // both). Accepts EITHER --spec (spec-driven) OR --root
            // (brownfield protocol-mode); both share the build → smoke →
            // run → triage pipeline and differ only in which `.qedspec` is
            // loaded and which invariant family is emitted.
            if let Some(budget_secs) = fuzz {
                let (
                    synthesised_spec,
                    synthesised_idl,
                    spec_path_for_ctx,
                    project_root_for_idl,
                    mode,
                ) = match (spec.clone(), root.clone()) {
                    (Some(spec_path), maybe_root) => {
                        let parsed = check::parse_spec_file(&spec_path)?;
                        let spec_parent = spec_path
                            .parent()
                            .map(|p| p.to_path_buf())
                            .unwrap_or_else(|| std::path::PathBuf::from("."));
                        // --spec + --root layers spec invariants on
                        // top of protocol-mode crash detection.
                        let mode = if maybe_root.is_some() {
                            crucible_gen::InvariantMode::Both
                        } else {
                            crucible_gen::InvariantMode::Spec
                        };
                        (parsed, None, spec_path, spec_parent, mode)
                    }
                    (None, Some(root_path)) => {
                        let resolved = crucible_brownfield::resolve_program_root(&root_path)?;
                        let detected = probe::detect_runtime_public(&resolved);
                        let runtime_final = runtime.map(probe::Runtime::from).unwrap_or(detected);
                        let synth = crucible_brownfield::synthesize_spec(&resolved, runtime_final)?;
                        (
                            synth.spec,
                            synth.idl_json,
                            resolved.clone(),
                            resolved,
                            crucible_gen::InvariantMode::Protocol,
                        )
                    }
                    (None, None) => {
                        return Err(anyhow::anyhow!(
                            "--fuzz requires either --spec <path> (spec-driven) \
                                 or --root <project-path> (brownfield protocol-mode). \
                                 See `qedgen probe --help` for details."
                        ));
                    }
                };
                // Same name normalization as crucible_gen so the computed
                // harness path matches the directory generate creates —
                // kebab-case names must become snake_case or the IDL lands
                // in a sibling directory of the real harness.
                let prog = crucible_gen::spec_program_name(&synthesised_spec);
                let harness_parent = if matches!(mode, crucible_gen::InvariantMode::Protocol) {
                    crucible_brownfield::brownfield_harness_parent(&project_root_for_idl)
                } else {
                    project_root_for_idl.join("fuzz")
                };
                let harness = harness_dir
                    .clone()
                    .unwrap_or_else(|| harness_parent.join(&prog));
                // Brownfield: emit the harness under `.qed/fuzz/<prog>/` if
                // absent. Spec mode expects a prior `codegen --crucible`;
                // never auto-regen.
                if matches!(mode, crucible_gen::InvariantMode::Protocol) && !harness.exists() {
                    std::fs::create_dir_all(&harness_parent)?;
                    crucible_gen::generate(&synthesised_spec, &harness_parent, mode)?;
                }
                // Brownfield ships its own IDL (no `anchor build` to feed
                // `discover_idl`): write the synthesised JSON to
                // `<harness>/idls/<prog>.json`, overwriting each run so
                // scanner improvements propagate.
                if let Some(idl_json) = synthesised_idl.as_deref() {
                    crucible_brownfield::write_synthesized_idl(&harness, &prog, idl_json)
                        .context("writing synthesised IDL")?;
                }
                // Budget 0 = emit the harness and exit (dry-run preview
                // without the Crucible build cost).
                if budget_secs == 0 {
                    let output = probe::ProbeOutput {
                        version: 1,
                        mode: if matches!(mode, crucible_gen::InvariantMode::Protocol) {
                            probe::Mode::SpecLess
                        } else {
                            probe::Mode::SpecAware
                        },
                        spec_path: spec.as_ref().map(|p| p.display().to_string()),
                        project_root: root.as_ref().map(|p| p.display().to_string()),
                        runtime: None,
                        handlers: None,
                        applicable_categories: None,
                        findings: Vec::new(),
                        clusters: None,
                        dispatcher_kind: None,
                    };
                    eprintln!(
                        "Budget = 0: harness ready at {}; skipping build + fuzz run.",
                        harness.display()
                    );
                    println!("{}", serde_json::to_string_pretty(&output)?);
                    return Ok(());
                }
                let mut ctx = crucible_probe::FuzzProbeContext::new(
                    &spec_path_for_ctx,
                    project_root_for_idl,
                    harness,
                );
                ctx.fuzz_budget = std::time::Duration::from_secs(budget_secs);
                if no_smoke {
                    ctx.smoke_budget = std::time::Duration::ZERO;
                }
                ctx.stateful = stateful;
                ctx.invariant_mode = mode;
                let findings = crucible_probe::run_fuzz_probe(&ctx)?;
                let output = probe::ProbeOutput {
                    version: 1,
                    mode: if matches!(mode, crucible_gen::InvariantMode::Protocol) {
                        probe::Mode::SpecLess
                    } else {
                        probe::Mode::SpecAware
                    },
                    spec_path: spec.as_ref().map(|p| p.display().to_string()),
                    project_root: root.as_ref().map(|p| p.display().to_string()),
                    runtime: None,
                    handlers: None,
                    applicable_categories: None,
                    findings,
                    clusters: None,
                    dispatcher_kind: None,
                };
                println!("{}", serde_json::to_string_pretty(&output)?);
                return Ok(());
            }

            let _ = (harness_dir, no_smoke, stateful);
            let output = if bootstrap {
                let root = root
                    .ok_or_else(|| anyhow::anyhow!("--bootstrap requires --root <project-path>"))?;
                probe::run_bootstrap(&root)?
            } else {
                let spec = spec.ok_or_else(|| {
                    anyhow::anyhow!("provide --spec <path> for spec-aware mode, or --bootstrap --root <path> for spec-less")
                })?;
                probe::run_probe(&spec)?
            };
            let rendered = serde_json::to_string_pretty(&output)?;
            println!("{}", rendered);
        }

        Commands::Ratify {
            audit_dir,
            out,
            scoping_out,
            findings_dir,
        } => {
            let opts = ratify::RatifyOpts {
                audit_dir,
                spec_out: out,
                scoping_out,
                findings_dir,
            };
            let report = ratify::run(&opts)?;
            eprintln!(
                "Ratification complete: {} accepted, {} narrowed, {} rejected, {} flagged-as-bug, {} deferred",
                report.accepted,
                report.narrowed,
                report.rejected,
                report.flagged_as_bug,
                report.deferred,
            );
            eprintln!("Wrote spec to {}", report.spec_path.display());
            if report.rejected > 0 {
                eprintln!("Wrote scoping notes to {}", report.scoping_path.display());
            }
            for p in &report.findings_paths {
                eprintln!("Wrote bug-flagged finding to {}", p.display());
            }
        }

        Commands::Spec { idl, output_dir } => {
            let stem = idl
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            std::fs::create_dir_all(&output_dir)?;
            let output_file = output_dir.join(format!("{}.qedspec", stem));
            idl2spec::generate_qedspec(&idl, &output_file)?;
        }

        Commands::Descriptor {
            spec,
            handler,
            account,
        } => {
            let parsed = check::parse_spec_file(&spec)?;
            let descriptor = descriptor::build_descriptor(&parsed, &handler, account)?;
            println!("{}", serde_json::to_string_pretty(&descriptor)?);
        }

        Commands::Discharge {
            spec,
            handler,
            account,
            so,
            idl,
            qedlift,
            module,
            out_dir,
            transition,
        } => {
            let parsed = check::parse_spec_file(&spec)?;
            if transition {
                descriptor::run_discharge_transition(
                    &parsed,
                    &handler,
                    account,
                    &so,
                    idl.as_deref(),
                    &qedlift,
                    out_dir.as_deref(),
                )?;
            } else {
                descriptor::run_discharge(
                    &parsed,
                    &handler,
                    account,
                    &so,
                    idl.as_deref(),
                    &qedlift,
                    module,
                    out_dir.as_deref(),
                )?;
            }
        }

        Commands::Consolidate {
            input_dir,
            output_dir,
        } => {
            consolidate::consolidate_proofs(&input_dir, &output_dir)?;
        }

        Commands::Asm2Lean {
            input,
            output,
            namespace,
        } => {
            asm2lean::asm2lean(&input, &output, namespace.as_deref())?;
        }

        Commands::Setup { workspace, mathlib } => {
            deps::require_lean()?;
            validate::setup_workspace(workspace.as_deref(), mathlib).await?;
        }

        Commands::Init {
            name,
            spec,
            asm,
            mathlib,
            target,
            output_dir,
        } => {
            // Program scaffolding parses the `.qedspec` directly (init's
            // Spec.lean skeleton isn't enough) — refuse `--target` without
            // `--spec`.
            let scaffold_target = target;
            if scaffold_target.is_some() && spec.is_none() {
                anyhow::bail!(
                    "`--target` requires `--spec <path.qedspec>` — the \
                     program codegen runs against the spec directly."
                );
            }

            // .qed/ lives at the program root — see init::resolve_program_root.
            let cwd = std::env::current_dir()?;
            let program_root = init::resolve_program_root(spec.as_deref(), &output_dir, &cwd);
            // The spec pointer is stored relative to program_root so
            // `qedgen check` from anywhere under the project resolves it
            // via .qed/config.json → project_root / <spec>.
            let spec_rel = spec.as_ref().map(|p| {
                p.strip_prefix(&program_root)
                    .unwrap_or(p.as_path())
                    .to_string_lossy()
                    .to_string()
            });
            init::init_qed_dir(&program_root, &name, spec_rel.as_deref())?;

            init::init(
                &name,
                &output_dir,
                asm.as_deref(),
                mathlib,
                scaffold_target.is_some(),
            )?;

            if let (Some(target), Some(qedspec_path)) = (scaffold_target, spec.as_ref()) {
                let program_dir = program_root.join(format!("programs/{}", name));
                // Tests live INSIDE the program package so cargo-kani /
                // cargo-test resolve the governing Cargo.toml.
                let kani_path = program_dir.join("tests/kani.rs");

                // Rust skeleton via the MIR path (codegen_mir) — same as the
                // `codegen` command.
                let parsed = check::parse_spec_file(qedspec_path)?;
                let mir = mir::lower(&parsed);
                codegen_mir::generate(&mir, &parsed, qedspec_path, &program_dir, target)?;

                // Kani harnesses are framework-neutral (pure spec-derived
                // state model).
                kani_mir::generate(&mir, &parsed, &kani_path)?;
                // #182: spec-model harness imports mul_div from the crate when
                // its guards use them — deliver + depend if so.
                deliver_prelude_if_referenced(&kani_path)?;

                // Unit tests are framework-neutral too.
                let test_path = program_dir.join("src/tests.rs");
                unit_test::generate(qedspec_path, &test_path)?;
            }
        }

        // ==================================================================
        // check — unified spec validation
        // ==================================================================
        Commands::Check {
            spec,
            proofs,
            coverage,
            explain,
            output,
            code,
            anchor_project,
            drift,
            update_hashes,
            deep,
            kani,
            asm,
            json,
            frozen,
            strict,
            no_cache,
            regen_drift,
            examples_root,
            write,
        } => {
            require_git_repo()?;
            let cwd = std::env::current_dir()?;

            if regen_drift {
                let examples_root = if examples_root.is_absolute() {
                    examples_root
                } else {
                    cwd.join(examples_root)
                };
                let mode = if write {
                    regen_drift::WriteMode::Write
                } else {
                    regen_drift::WriteMode::Check
                };
                let report = regen_drift::check_examples_with(&examples_root, mode)?;
                regen_drift::print_report(&report);
                // In Write mode, drift entries are expected (the writer
                // resolved them). Only error if anything's still unresolved:
                // missing manifests always, MissingGeneratedCounterpart
                // always (writer can't synthesize a file the regen
                // pipeline didn't produce), and Changed entries when
                // running in Check mode.
                let unresolved = !report.missing_manifests.is_empty()
                    || report.drift.iter().any(|d| match d.kind {
                        regen_drift::DriftKind::MissingGeneratedCounterpart => true,
                        regen_drift::DriftKind::Changed => {
                            !matches!(mode, regen_drift::WriteMode::Write)
                        }
                    });
                if unresolved {
                    std::process::exit(1);
                }
                return Ok(());
            }

            let spec = init::resolve_spec_path(spec.as_deref(), &cwd)?;
            let spec_name = spec
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "Spec".to_string());

            // --frozen elevates qed.lock drift to a hard error (CI); Auto
            // writes the lock on drift (right for local dev).
            let lock_mode = if frozen {
                qed_lock::LockMode::Frozen
            } else {
                qed_lock::LockMode::Auto
            };

            // --no-cache forces a fresh github fetch for every imported dep
            // (skips the TTL window). Path sources unaffected.
            let cache_opts = import_resolver::CacheOpts {
                force_refresh: no_cache,
            };

            let mut has_issues = false;

            // `check --frozen` runs the upstream binary-hash diff
            // opportunistically: mismatches are P2 warnings (exit 0);
            // `--strict` escalates to CRIT and gates exit (verify parity).
            // Fetch errors (no `solana` CLI / network) never gate either
            // mode — a sandbox without the toolchain mustn't fail CI.
            if frozen {
                let spec_dir = spec.parent().unwrap_or_else(|| Path::new("."));
                let pinned = qed_lock::read(spec_dir)
                    .ok()
                    .flatten()
                    .as_ref()
                    .map(upstream_check::lock_has_pinned_hash)
                    .unwrap_or(false);
                if pinned {
                    match upstream_check::check_lock(spec_dir, None, false) {
                        Ok(results) => {
                            let gate = if strict {
                                upstream_check::Gate::CheckFrozenStrict
                            } else {
                                upstream_check::Gate::CheckFrozen
                            };
                            let routed = upstream_check::route_findings(results, gate);
                            let blocking = upstream_check::print_routed_report(&routed);
                            if blocking {
                                has_issues = true;
                            }
                        }
                        Err(e) => {
                            // Couldn't open the lock — note but never gate
                            // exit (parity with verify's Error routing).
                            eprintln!("note: --frozen upstream check skipped: {}", e);
                        }
                    }
                }

                // proof_hash drift routing (sibling of the binary_hash
                // dispatch): parse under Frozen so handle_lock populates
                // `proof_hash_findings`. P2 under `--frozen`, CRIT with
                // `--strict`. Parse errors re-raise at the main parse
                // below — don't double-report here.
                if let Ok(parsed) = check::parse_spec_file_with_opts(&spec, lock_mode, cache_opts) {
                    if !parsed.proof_hash_findings.is_empty() {
                        let gate = if strict {
                            upstream_check::Gate::CheckFrozenStrict
                        } else {
                            upstream_check::Gate::CheckFrozen
                        };
                        let routed = upstream_check::route_findings(
                            parsed.proof_hash_findings.clone(),
                            gate,
                        );
                        let blocking = upstream_check::print_routed_report(&routed);
                        if blocking {
                            has_issues = true;
                        }
                    }
                }
            }

            // sBPF verification (--asm)
            if let Some(ref asm_path) = asm {
                sbpf_verify::verify(asm_path, &proofs)?;
            }

            // Drift detection (--drift)
            if let Some(ref drift_path) = drift {
                if update_hashes {
                    let count = drift::update(drift_path)?;
                    eprintln!("Updated {} hash(es).", count);
                } else {
                    let entries = drift::check(drift_path)?;
                    drift::print_report(&entries);
                    if entries
                        .iter()
                        .any(|e| !matches!(e.status, drift::DriftStatus::Ok))
                    {
                        has_issues = true;
                    }
                    if deep {
                        let deep_entries = drift::check_deep(drift_path)?;
                        drift::print_deep_report(&deep_entries);
                        if !deep_entries.is_empty() {
                            has_issues = true;
                        }
                    }
                }
            }

            // Unified code/kani drift (--code, --kani)
            if code.is_some() || kani.is_some() {
                let report =
                    check::check_unified(&spec, &proofs, code.as_deref(), kani.as_deref())?;
                check::print_unified_report(&spec_name, &report);
                if report.issue_count() > 0 {
                    has_issues = true;
                }
            }

            // Anchor cross-check (--anchor-project) — spec handler list vs
            // the existing program; catches stale specs and uncovered
            // handlers as a CI gate.
            if let Some(ref project_path) = anchor_project {
                if check::render_anchor_project_report(&spec, project_path, json)? {
                    has_issues = true;
                }
            }

            // Verification report (--explain). `--json` emits the
            // verification-status data layer for the agent to render; without
            // it the CLI prints the inline Markdown human fallback.
            if explain {
                check::render_explain_report(&spec, &spec_name, &proofs, json, output.as_deref())?;
            }

            // One parse shared by every remaining ParsedSpec consumer
            // (--coverage, the orphan scan, --code todo lints) — all use the
            // same lock_mode/cache_opts. The --frozen proof-hash peek above
            // keeps its own parse (it deliberately swallows parse errors),
            // and --anchor-project keeps its default-opts parse inside
            // `check::render_anchor_project_report` — folding it into this
            // one would change gating under `--frozen`.
            let parsed_shared = if coverage || proofs.exists() || code.is_some() {
                Some(check::parse_spec_file_with_opts(
                    &spec, lock_mode, cache_opts,
                )?)
            } else {
                None
            };

            // Coverage matrix (--coverage)
            if coverage {
                let parsed = parsed_shared.as_ref().expect("parsed under coverage guard");
                let matrix = check::coverage_matrix(parsed);
                if json {
                    println!("{}", serde_json::to_string_pretty(&matrix)?);
                } else {
                    check::print_coverage_table(&matrix);
                }
            }

            // Orphan / missing preservation theorems in Proofs.lean — runs
            // whenever the proofs dir exists; no-op without obligations.
            if proofs.exists() {
                let parsed = parsed_shared
                    .as_ref()
                    .expect("parsed under proofs-dir guard");
                let findings = proofs_bootstrap::check_orphans(parsed, &proofs)?;
                if !findings.is_empty() {
                    // A foreign Proofs.lean (generated from a DIFFERENT spec,
                    // #166) is informational — a Kani-first workflow may
                    // legitimately never regenerate Lean — so it must not
                    // fail the check. Real same-spec drift still does.
                    let only_foreign = findings.iter().all(|f| {
                        matches!(f, proofs_bootstrap::OrphanFinding::ForeignProofs { .. })
                    });
                    if json {
                        let as_json: Vec<serde_json::Value> = findings
                            .iter()
                            .map(|f| match f {
                                proofs_bootstrap::OrphanFinding::Orphan(n) => {
                                    serde_json::json!({"kind": "orphan", "theorem": n})
                                }
                                proofs_bootstrap::OrphanFinding::Missing(n) => {
                                    serde_json::json!({"kind": "missing", "theorem": n})
                                }
                                proofs_bootstrap::OrphanFinding::ForeignProofs {
                                    declared,
                                    expected,
                                } => serde_json::json!({
                                    "kind": "foreign_proofs",
                                    "declared": declared,
                                    "expected": expected,
                                }),
                            })
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&as_json)?);
                    } else {
                        eprintln!("Proofs.lean drift:");
                        for f in &findings {
                            eprintln!("  {}", f);
                        }
                    }
                    if !only_foreign {
                        has_issues = true;
                    }
                }
            }

            // Lint — always runs (core of spec validation)
            {
                let mut warnings = check::lint_with_opts(&spec, lock_mode, cache_opts)?;
                // Code-aware lints (residual `todo!()` placeholders in
                // user-owned handler bodies) only fire when --code is set.
                // Merge them in here so JSON consumers see one combined list.
                if let Some(ref code_dir) = code {
                    let parsed = parsed_shared.as_ref().expect("parsed under --code guard");
                    warnings.extend(check::check_handler_todos(parsed, code_dir)?);
                }
                if json {
                    println!("{}", serde_json::to_string_pretty(&warnings)?);
                } else if warnings.is_empty() {
                    eprintln!("Spec is complete — no issues found.");
                } else {
                    let warns = warnings
                        .iter()
                        .filter(|w| w.severity == check::Severity::Warning)
                        .count();
                    let infos = warnings
                        .iter()
                        .filter(|w| w.severity == check::Severity::Info)
                        .count();
                    for w in &warnings {
                        eprintln!("{}\n", format_lint_warning(w));
                    }
                    eprintln!("{} warning(s), {} info", warns, infos);
                    if warns > 0 {
                        has_issues = true;
                    }
                }
            }

            if has_issues {
                std::process::exit(1);
            }
        }

        // ==================================================================
        // verify — run generated harnesses against generated code
        // ==================================================================
        Commands::Verify {
            spec,
            proptest,
            proptest_path,
            kani,
            kani_path,
            lean,
            lean_dir,
            miri,
            fail_fast,
            json,
            check_upstream,
            rpc_url,
            offline,
            upstream_stale_ok,
            probe_repros,
            crucible,
            crucible_harness_dir,
            crucible_no_smoke,
            crucible_stateful,
            require_verified,
            recursive,
        } => {
            require_git_repo()?;

            // Fall back to .qed/config.json's `spec` like check/codegen so
            // flag-less `qedgen verify` works.
            let cwd = std::env::current_dir()?;
            let spec = init::resolve_spec_path(spec.as_deref(), &cwd)?;

            // Parse once if either gate needs the ParsedSpec; both are
            // pre-checks that may exit before backends dispatch.
            let parsed_for_gates = if require_verified || recursive {
                Some(check::parse_spec_file(&spec)?)
            } else {
                None
            };

            // --require-verified: fail fast before backends — results
            // against a not-fully-proven dep graph still rest on Stance-1
            // axiom discharge.
            if require_verified {
                let parsed = parsed_for_gates.as_ref().expect("parsed under gate guard");
                verify::require_verified_gate(parsed);
            }

            // --recursive: `lake build` every imported proof package.
            if recursive {
                let parsed = parsed_for_gates.as_ref().expect("parsed under gate guard");
                verify::recursive_lake_walk(parsed);
            }

            // --check-upstream diffs each pinned binary hash against the
            // on-chain `.so` (`solana program dump`); runs independently of
            // the harnesses; --offline refuses network. Auto-on when
            // qed.lock has any pinned hash — skipping the flag no longer
            // bypasses the gate; `--upstream-stale-ok` suppresses it for
            // offline dev.
            let spec_dir = spec.parent().unwrap_or_else(|| Path::new("."));
            let run_upstream = if upstream_stale_ok {
                // Honored even when --check-upstream is explicit — the
                // local-dev escape hatch, not a "render warnings anyway" knob.
                false
            } else if check_upstream {
                true
            } else {
                qed_lock::read(spec_dir)
                    .ok()
                    .flatten()
                    .as_ref()
                    .map(upstream_check::lock_has_pinned_hash)
                    .unwrap_or(false)
            };
            if run_upstream {
                let results = upstream_check::check_lock(spec_dir, rpc_url.as_deref(), offline)?;
                let gate = upstream_check::Gate::Verify;
                let routed = upstream_check::route_findings(results, gate);
                let blocking = upstream_check::print_routed_report(&routed);
                if blocking {
                    std::process::exit(1);
                }
                // When --check-upstream is the only verb, exit cleanly
                // without firing the backend runners. Combine with
                // --proptest etc. to do both in one invocation.
                let any_backend_flag = proptest || kani || lean || miri || probe_repros;
                if check_upstream && !any_backend_flag {
                    return Ok(());
                }
            } else if check_upstream && upstream_stale_ok {
                // Allowed combination — breadcrumb that the suppression
                // flag won.
                eprintln!(
                    "note: --upstream-stale-ok suppressed --check-upstream (offline-dev mode)"
                );
                let any_backend_flag = proptest || kani || lean || miri || probe_repros;
                if !any_backend_flag {
                    return Ok(());
                }
            }

            // --probe-repros runs the per-probe Mollusk reproducers — a
            // separate stage with its own report shape (not folded into the
            // BackendReport rollup), run first so the auditor has the
            // gating data.
            if probe_repros {
                let project_root = spec.parent().map(Path::to_path_buf).unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                });
                let report = verify_probe_repros::run(&project_root)?;
                if json {
                    verify_probe_repros::print_json(&report)?;
                } else {
                    verify_probe_repros::print_human(&report);
                }
                if !report.all_fired_or_inconclusive() {
                    std::process::exit(1);
                }
                let any_backend_flag = proptest || kani || lean || miri;
                if !any_backend_flag {
                    return Ok(());
                }
            }

            // No explicit backend flags -> run every backend whose artifact
            // is present on disk.
            let any_flag = proptest || kani || lean || miri;
            // Project root used by Miri repro discovery — spec parent dir.
            let project_root = spec
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let miri_default = !project_root
                .join(".qed")
                .join("probes")
                .join("pinocchio")
                .read_dir()
                .map(|mut it| it.next().is_none())
                .unwrap_or(true);
            let opts = if any_flag {
                verify::VerifyOpts {
                    spec: spec.clone(),
                    proptest,
                    proptest_path,
                    kani,
                    kani_path,
                    lean,
                    lean_dir,
                    miri,
                    fail_fast,
                    project_root: project_root.clone(),
                }
            } else {
                verify::VerifyOpts {
                    spec: spec.clone(),
                    proptest: proptest_path.exists(),
                    proptest_path,
                    kani: kani_path.exists(),
                    kani_path,
                    lean: lean_dir.join("lakefile.lean").exists()
                        || lean_dir.join("lakefile.toml").exists(),
                    lean_dir,
                    miri: miri_default,
                    fail_fast,
                    project_root: project_root.clone(),
                }
            };

            let mut report = verify::run(&opts)?;

            // --crucible is a thin alias over the probe engine; findings
            // wrap into one BackendReport so they render alongside
            // Kani/proptest.
            if let Some(budget_secs) = crucible {
                let backend = crucible_backend_report(
                    &spec,
                    crucible_harness_dir.clone(),
                    budget_secs,
                    crucible_no_smoke,
                    crucible_stateful,
                );
                report.backends.push(backend);
            }
            let _ = (crucible_harness_dir, crucible_no_smoke, crucible_stateful);

            if json {
                verify::print_json(&report)?;
            } else {
                verify::print_human(&report);
            }

            if !report.ok() {
                std::process::exit(1);
            }
        }

        // ==================================================================
        // readiness — preflight lint for first-deploy mainnet-readiness
        // ==================================================================
        //
        // Exit codes match ratchet: findings map to 1/2 via
        // `ratchet::exit_code`; caller-side failures (missing IDL, bad
        // JSON) exit 3 so CI can tell breakage from misconfiguration.
        Commands::Readiness {
            idl,
            list_rules,
            quasar,
            json,
        } => {
            if list_rules {
                ratchet::print_rules_preflight(json)?;
                return Ok(());
            }
            // clap's `required_unless_present = "list_rules"` guarantees
            // `idl` is Some here — unwrap is safe in shape.
            let idl = idl.expect("--idl is required unless --list-rules");
            let framework = resolve_framework(quasar, json);
            let report = match ratchet::run_readiness(&ratchet::ReadinessOpts { idl, framework }) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Error: {:#}", e);
                    std::process::exit(3);
                }
            };
            if json {
                ratchet::print_json(&report)?;
            } else {
                ratchet::print_human(&report);
            }
            let code = ratchet::exit_code(&report);
            if code != 0 {
                std::process::exit(code);
            }
        }

        // ==================================================================
        // check-upgrade — diff two IDLs under ratchet's R-rules
        // ==================================================================
        Commands::CheckUpgrade {
            old,
            new,
            unsafes,
            migrated_accounts,
            realloc_accounts,
            list_rules,
            quasar,
            json,
        } => {
            if list_rules {
                ratchet::print_rules_diff(json)?;
                return Ok(());
            }
            let old = old.expect("--old is required unless --list-rules");
            let new = new.expect("--new is required unless --list-rules");
            let framework = resolve_framework(quasar, json);
            let report = match ratchet::run_check_upgrade(&ratchet::CheckUpgradeOpts {
                old,
                new,
                unsafes,
                migrated_accounts,
                realloc_accounts,
                framework,
            }) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Error: {:#}", e);
                    std::process::exit(3);
                }
            };
            if json {
                ratchet::print_json(&report)?;
            } else {
                ratchet::print_human(&report);
            }
            let code = ratchet::exit_code(&report);
            if code != 0 {
                std::process::exit(code);
            }
        }

        // ==================================================================
        // codegen — generate committed artifacts
        // ==================================================================
        Commands::Codegen {
            spec,
            target,
            output_dir,
            kani,
            kani_output,
            kani_impl,
            kani_impl_output,
            kani_impl_brownfield,
            kani_impl_context,
            test,
            test_output,
            proptest,
            proptest_output,
            crucible,
            crucible_output,
            integration,
            integration_output,
            lean,
            lean_output,
            ci,
            ci_output,
            ci_asm,
            ci_ratchet,
            all,
            fill,
            handler,
            fill_tests,
        } => {
            require_git_repo()?;
            // `is_pinocchio` gates the post-regen stamped-drift scan below
            // (the Pinocchio scaffold carries no `#[qed(verified)]` stamps).
            let is_pinocchio = matches!(target, Target::Pinocchio);
            let cwd = std::env::current_dir()?;
            let spec = init::resolve_spec_path(spec.as_deref(), &cwd)?;
            // One parse + one MIR lowering, shared by every artifact stage
            // below (the arm previously re-parsed per artifact).
            let parsed = check::parse_spec_file(&spec)?;
            let mir = mir::lower(&parsed);
            // sBPF specs model assembly, not a Rust state machine — every
            // Rust-shaped artifact is meaningless for them. Decide up front
            // so the scaffold's handlers gate can't fire before the Lean
            // branch (#88): assembly targets emit only `--lean` and `--ci`.
            let is_assembly = parsed.is_assembly_target();
            if is_assembly {
                eprintln!(
                    "note: sBPF spec — skipping Rust scaffold (assembly programs \
                     generate Lean proofs via --lean; runtime checks belong in \
                     client-side tests)."
                );
            } else if kani_impl_brownfield || kani_impl_context || parsed.is_struct_mirror() {
                // Brownfield: the program pre-exists, so the greenfield Rust
                // scaffold is meaningless — and it would `map_type` the real
                // struct's field types (enums, imported types) that a mirror
                // State legitimately names but the scaffold isn't meant to
                // synthesize. Emit only the impl-Kani harness below (if asked).
                //
                // Triggers on the `--kani-impl-brownfield` flag OR a spec that
                // is a struct mirror by construction (`pragma state_struct` /
                // `state_module`) — so a mirror spec never drops a throwaway
                // crate under ANY invocation (default, `--kani-impl`, `--all`),
                // not just when the flag is passed.
                eprintln!(
                    "note: brownfield spec (pragma state_struct / --kani-impl-brownfield) \
                     — skipping greenfield Rust scaffold; the program already exists."
                );
            } else {
                codegen_mir::generate(&mir, &parsed, &spec, &output_dir, target)?;
            }

            if kani || all {
                // sBPF is verified by Lean proofs over the assembly; the
                // harness generator has no sBPF awareness and would emit
                // meaningless Anchor-shaped harnesses. Skip.
                if is_assembly {
                    if kani {
                        note_sbpf_skip("Kani");
                    }
                } else {
                    // Codegen is pure text generation — the hard cargo-kani
                    // gate lives in `qedgen verify --kani`. Warn so the
                    // install hint surfaces; don't block.
                    if let Err(e) = deps::require_kani() {
                        eprintln!("warning: {e}");
                    }
                    kani_mir::generate(&mir, &parsed, &kani_output)?;
                    // #182: deliver + depend on the crate if the spec-model
                    // harness imports mul_div from it.
                    deliver_prelude_if_referenced(&kani_output)?;
                }
            }

            // Impl-targeted Kani harness emits when `--kani-impl` is
            // explicit, or `--all` + at least one handler auto-triggers
            // (modifies ⊋ effect.lhs — the LP-shape signal;
            // `kani_impl::spec_triggers_impl_harness`). Plain `--kani`
            // stays model-only so the spec-transition and implementation
            // gates remain separable. sBPF never emits Kani — suppress the
            // auto-trigger too.
            let auto_impl_trigger = if is_assembly {
                false
            } else {
                kani_impl::spec_triggers_impl_harness(&parsed)
            };
            let want_kani_impl = !is_assembly
                && (kani_impl
                    || kani_impl_brownfield
                    || kani_impl_context
                    || (all && auto_impl_trigger));
            if want_kani_impl {
                if let Err(e) = deps::require_kani() {
                    eprintln!("warning: {e}");
                }
                // Pinocchio/Quasar harnesses must live in `src/` — `cargo
                // kani` only discovers `#[kani::proof]` in the lib, and the
                // harness's `crate::<Pascal>` refs only resolve there.
                // Anchor keeps its `tests/` default.
                let kani_impl_path = if matches!(target, Target::Pinocchio | Target::Quasar) {
                    redirect_kani_impl_to_src(&kani_impl_output)
                } else {
                    kani_impl_output.clone()
                };
                let impl_mode = if kani_impl_context {
                    kani_impl::KaniImplMode::Context
                } else if kani_impl_brownfield {
                    kani_impl::KaniImplMode::Brownfield
                } else {
                    kani_impl::KaniImplMode::Greenfield
                };
                kani_impl::generate_with_mode(
                    &spec,
                    &kani_impl_path,
                    /*explicit_flag=*/
                    kani_impl || kani_impl_brownfield || kani_impl_context,
                    target,
                    impl_mode,
                )?;

                // #182 Shape-1: the brownfield state-driven shape stubs
                // Pubkey/div through the crate (greenfield symbolic-accounts does
                // not) — deliver + depend only if the emitted harness references it.
                deliver_prelude_if_referenced(&kani_impl_path)?;
            }

            if test || all {
                // Meaningless for assembly targets (same rationale as Kani).
                if is_assembly {
                    if test {
                        note_sbpf_skip("unit-test");
                    }
                } else {
                    unit_test::generate(&spec, &test_output)?;
                }
            }
            if proptest || all {
                // Meaningless for assembly targets (same rationale as Kani).
                if is_assembly {
                    if proptest {
                        note_sbpf_skip("proptest");
                    }
                } else {
                    proptest_gen_mir::generate(&mir, &parsed, &proptest_output)?;
                }
            }
            if crucible || all {
                // Crucible fuzzes the Rust handler surface — likewise
                // meaningless for assembly targets.
                if is_assembly {
                    if crucible {
                        note_sbpf_skip("Crucible");
                    }
                } else {
                    crucible_gen::generate(
                        &parsed,
                        &crucible_output,
                        crucible_gen::InvariantMode::Spec,
                    )?;
                }
            }
            if integration || all {
                if is_assembly {
                    if integration {
                        note_sbpf_skip("integration-test");
                    }
                } else {
                    integration_test::generate(&spec, &integration_output)?;
                }
            }
            if lean || all {
                // Pure text writers; `lake` is only needed to *build*, which
                // `qedgen verify --lean` gates. Warn without blocking.
                if let Err(e) = deps::require_lean() {
                    eprintln!("warning: {e}");
                }
                // `lean_gen_mir` handles every spec shape — single/multi-
                // account, indexed records, ADTs, and sBPF (via the MIR
                // `is_assembly` flag).
                lean_gen_mir::generate(&mir, &parsed, &lean_output)?;
                // Bootstrap Proofs.lean alongside Spec.lean. Never overwrites
                // an existing file — the user-owned theorems survive regen.
                if let Some(proofs_dir) = lean_output.parent() {
                    proofs_bootstrap::bootstrap_if_missing(&parsed, proofs_dir)?;
                }
            }
            if ci || all {
                const CI_TEMPLATE: &str = include_str!("../../../templates/verify.yml");
                let verify_step = if let Some(ref asm) = ci_asm {
                    format!("\n      - name: Verify sBPF binary\n        run: qedgen check --spec program.qedspec --asm {}\n", asm)
                } else {
                    String::new()
                };
                let ratchet_step = if let Some(ref idl) = ci_ratchet {
                    format!(
                        "\n      - name: Ratchet readiness lint\n        run: qedgen readiness --idl {}\n",
                        idl
                    )
                } else {
                    String::new()
                };
                let workflow = expand_ci_template(CI_TEMPLATE, &verify_step, &ratchet_step);
                if let Some(parent) = ci_output.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&ci_output, workflow)?;
                eprintln!("Generated CI workflow: {}", ci_output.display());
            }

            // Surface stale `#[qed(verified)]` stamps right after regen so
            // users get the re-stamp command before the proc-macro's
            // `compile_error!` fires on the next build. Skipped for
            // Pinocchio (no stamps), assembly targets (no scaffold), and a
            // missing output_dir.
            if !is_pinocchio && !is_assembly && output_dir.exists() {
                match drift::check_stamped_drift(&output_dir) {
                    Ok(stamped) if !stamped.is_empty() => {
                        eprintln!(
                            "cargo:warning={} verified handler(s) have stale stamps after regen:",
                            stamped.len()
                        );
                        for entry in &stamped {
                            eprintln!(
                                "cargo:warning=  {}::{}",
                                entry.file.display(),
                                entry.fn_name
                            );
                        }
                        // All stamped fns share the same `--drift` root, so
                        // one invocation re-stamps the whole tree.
                        eprintln!(
                            "cargo:warning=hint: run `qedgen check --drift {} --update-hashes` \
                             to re-stamp",
                            output_dir.display()
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("warning: stamped-drift scan failed: {}", e);
                    }
                }
            }

            if fill {
                eprintln!("warning: `qedgen codegen --fill` is deprecated.");
                eprintln!("         The agent can fill `todo!()` sites directly via Read / Edit.");
                eprintln!("         Pattern: grep for `todo!()` in programs/, read the spec's");
                eprintln!("         handler/accounts blocks, edit each body in place. The");
                eprintln!("         prompt-emission layer is redundant with the agent's own");
                eprintln!("         file tools. Slated for hard-removal in v3.0; flag remains");
                eprintln!("         functional for now to avoid breaking existing scripts.");
                let opts = fill::FillOpts {
                    spec: &parsed,
                    spec_path: &spec,
                    programs_dir: &output_dir,
                    only_handler: handler.as_deref(),
                };
                fill::emit_prompts(&opts)?;
            }

            if fill_tests {
                eprintln!("warning: `qedgen codegen --fill-tests` is deprecated.");
                eprintln!("         The agent can fill integration-test `todo!()` sites directly.");
                eprintln!("         Slated for hard-removal in v3.0; flag remains functional.");
                let opts = fill::FillTestsOpts {
                    spec: &parsed,
                    spec_path: &spec,
                    tests_path: &integration_output,
                };
                fill::emit_test_prompts(&opts)?;
            }
        }

        Commands::Aristotle(cmd) => match cmd {
            AristotleCommands::Submit {
                project_dir,
                prompt,
                output_dir,
                wait,
                poll_interval,
            } => {
                deps::require_lean()?;
                validate_poll_interval(poll_interval)?;
                let prompt = prompt.unwrap_or_else(|| {
                    "Fill in all sorry placeholders with valid proofs".to_string()
                });
                let output = output_dir.unwrap_or_else(|| project_dir.clone());
                aristotle::fill_sorry(&project_dir, &output, &prompt, wait, poll_interval).await?;
            }

            AristotleCommands::Status {
                project_id,
                wait,
                poll_interval,
                output_dir,
            } => {
                validate_poll_interval(poll_interval)?;
                let project = aristotle::status(&project_id).await?;
                println!("Project:  {}", project.project_id);
                println!("Status:   {}", project.status);
                println!("Progress: {}%", project.percent_complete.unwrap_or(0));
                println!("Created:  {}", project.created_at);
                println!("Updated:  {}", project.last_updated_at);
                if let Some(summary) = &project.output_summary {
                    println!("Summary:  {}", summary);
                }

                if wait {
                    match project.status.as_str() {
                        "QUEUED" | "IN_PROGRESS" | "NOT_STARTED" => {
                            eprintln!("\nPolling until completion...");
                            let final_project = aristotle::poll(&project_id, poll_interval).await?;
                            match final_project.status.as_str() {
                                "COMPLETE" | "COMPLETE_WITH_ERRORS" => {
                                    if final_project.status == "COMPLETE_WITH_ERRORS" {
                                        eprintln!("Warning: Aristotle completed with some errors.");
                                    }
                                    aristotle::download_result(
                                        &final_project.project_id,
                                        &output_dir,
                                    )
                                    .await?;
                                    if let Some(summary) = &final_project.output_summary {
                                        eprintln!("\nSummary: {}", summary);
                                    }
                                }
                                status => {
                                    eprintln!("Project ended with status: {}", status);
                                    if let Some(summary) = &final_project.output_summary {
                                        eprintln!("Summary: {}", summary);
                                    }
                                }
                            }
                        }
                        _ => {
                            eprintln!("Project already in terminal state, nothing to poll.");
                        }
                    }
                }
            }

            AristotleCommands::Result {
                project_id,
                output_dir,
            } => {
                aristotle::download_result(&project_id, &output_dir).await?;
            }

            AristotleCommands::Cancel { project_id } => {
                let project = aristotle::cancel(&project_id).await?;
                eprintln!(
                    "Project {} cancelled (status: {})",
                    project.project_id, project.status
                );
            }

            AristotleCommands::List { limit, status } => {
                let projects = aristotle::list(limit, status.as_deref()).await?;
                if projects.is_empty() {
                    println!("No projects found.");
                } else {
                    println!("{:<38} {:<22} {:>5}  CREATED", "ID", "STATUS", "%");
                    for p in &projects {
                        println!(
                            "{:<38} {:<22} {:>4}%  {}",
                            p.project_id,
                            p.status,
                            p.percent_complete.unwrap_or(0),
                            p.created_at
                        );
                    }
                }
            }
        },

        // ==================================================================
        // reconcile — unified drift report (Rust handlers + Lean proofs)
        // ==================================================================
        Commands::Reconcile {
            spec,
            code,
            proofs,
            json,
        } => {
            require_git_repo()?;
            let cwd = std::env::current_dir()?;
            let spec = init::resolve_spec_path(spec.as_deref(), &cwd)?;
            let report = reconcile::reconcile(&spec, &code, &proofs)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                reconcile::print_report(&report);
            }
            if report.has_drift() {
                std::process::exit(1);
            }
        }

        // ==================================================================
        // feedback — bundle last-error context into a GitHub issue
        // ==================================================================
        Commands::Feedback {
            note,
            title,
            spec,
            dry_run,
            yes,
            no_open,
        } => {
            feedback::run(
                spec.as_deref(),
                note.as_deref(),
                title.as_deref(),
                dry_run,
                yes,
                no_open,
            )?;
        }
    }

    Ok(())
}
