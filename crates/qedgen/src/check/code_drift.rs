use super::*;
use anyhow::Result;
use regex::Regex;
use std::sync::LazyLock;

/// Check code drift — compare generated files against current spec.
pub fn check_code_drift(
    spec: &ParsedSpec,
    fp: &crate::fingerprint::SpecFingerprint,
    code_dir: &std::path::Path,
) -> Result<Vec<DriftResult>> {
    let mut results = Vec::new();

    // Files codegen owns and stamps with `spec-hash:<hex>` — these are the
    // ones drift detection should compare against the spec fingerprint.
    let mut codegen_owned_files: Vec<String> = vec![
        "src/lib.rs".to_string(),
        "src/state.rs".to_string(),
        "src/instructions/mod.rs".to_string(),
        "Cargo.toml".to_string(),
    ];
    // src/guards.rs is codegen-owned whenever any handler has a `requires`
    // clause that lowers to runtime guard logic — omitting it lets material
    // guard drift report "in sync".
    let any_handler_has_guards = spec.handlers.iter().any(|h| !h.requires.is_empty());
    if any_handler_has_guards {
        codegen_owned_files.push("src/guards.rs".to_string());
    }
    if !spec.events.is_empty() {
        codegen_owned_files.push("src/events.rs".to_string());
    }
    if !spec.error_codes.is_empty() {
        codegen_owned_files.push("src/errors.rs".to_string());
    }
    // ref_impls.rs is codegen-owned whenever the spec declares any
    // `ref_impl` — the file holds one `pub fn` per impl.
    if !spec.ref_impls.is_empty() {
        codegen_owned_files.push("src/ref_impls.rs".to_string());
    }

    // Per-handler files at `src/instructions/<handler>.rs` are user-owned
    // and never re-stamped after the initial scaffold, so NoHash is their
    // expected steady state, not a drift signal — only Missing matters.
    let user_owned_handler_files: Vec<String> = spec
        .handlers
        .iter()
        .map(|h| format!("src/instructions/{}.rs", h.name))
        .collect();

    for file in user_owned_handler_files
        .iter()
        .chain(codegen_owned_files.iter())
    {
        let path = code_dir.join(file);
        if !path.exists() {
            results.push(DriftResult {
                file: file.clone(),
                status: DriftStatus::Missing,
                detail: Some("expected by spec but not found".to_string()),
            });
            continue;
        }

        // User-owned handler files don't carry a spec-hash by design;
        // their existence is the only thing drift detection asserts.
        if user_owned_handler_files.contains(file) {
            results.push(DriftResult {
                file: file.clone(),
                status: DriftStatus::InSync,
                detail: None,
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

/// Flag residual `todo!()` placeholders in user-owned handler files.
/// `cargo check` passes through `todo!()` (returns `!`) and drift detection
/// only covers codegen-owned files, so without this lint a scaffolded
/// program ships with placeholder business logic uncaught. A `todo!()`
/// inside a `#[qed(verified, ...)]` body means events / transfers / CPIs /
/// non-mechanical effects haven't been filled.
pub fn check_handler_todos(
    spec: &ParsedSpec,
    code_dir: &std::path::Path,
) -> Result<Vec<CompletenessWarning>> {
    let mut warnings = Vec::new();

    let instructions_dir = code_dir.join("src").join("instructions");
    if !instructions_dir.exists() {
        return Ok(warnings);
    }

    for handler in &spec.handlers {
        let path = instructions_dir.join(format!("{}.rs", handler.name));
        if !path.exists() {
            continue;
        }
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let parsed = match syn::parse_file(&source) {
            Ok(f) => f,
            Err(_) => continue,
        };
        if !file_has_qed_verified_todo(&parsed) {
            continue;
        }

        let mut hints: Vec<String> = Vec::new();
        for emit in &handler.emits {
            hints.push(format!("emit `{}` event", emit));
        }
        for t in &handler.transfers {
            hints.push(format!("token transfer `{} -> {}`", t.from, t.to));
        }
        for call in &handler.calls {
            hints.push(format!(
                "CPI `{}.{}`",
                call.target_interface, call.target_handler
            ));
        }
        let hint_text = if hints.is_empty() {
            "non-mechanical effects".to_string()
        } else {
            hints.join(", ")
        };

        let rel = path
            .strip_prefix(code_dir)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.display().to_string());

        warnings.push(CompletenessWarning::new("handler_unfilled_todo", Severity::Warning, 2, format!(
                "handler `{}` has an unfilled `todo!()` in {} — spec expects: {}",
                handler.name, rel, hint_text
            )).subject(handler.name.clone()).fix(format!(
                "Open `{}` and fill the body using guard calls, state structs, and the spec's declared {} as the contract. Codegen leaves `todo!()` so the agent closes the loop on business logic; the placeholder type-checks but panics at runtime.",
                rel, hint_text
            )));
    }

    Ok(warnings)
}

fn file_has_qed_verified_todo(file: &syn::File) -> bool {
    use syn::visit::Visit;

    struct V {
        in_verified: u32,
        any: bool,
    }

    impl V {
        fn enter_with<F>(&mut self, attrs: &[syn::Attribute], visit: F)
        where
            F: FnOnce(&mut Self),
        {
            let verified = has_qed_verified_attr(attrs);
            if verified {
                self.in_verified += 1;
            }
            visit(self);
            if verified {
                self.in_verified -= 1;
            }
        }
    }

    impl<'ast> Visit<'ast> for V {
        fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
            let attrs = f.attrs.clone();
            self.enter_with(&attrs, |v| syn::visit::visit_item_fn(v, f));
        }
        fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
            let attrs = f.attrs.clone();
            self.enter_with(&attrs, |v| syn::visit::visit_impl_item_fn(v, f));
        }
        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            if self.in_verified > 0 {
                if let Some(seg) = mac.path.segments.last() {
                    if seg.ident == "todo" {
                        self.any = true;
                    }
                }
            }
            syn::visit::visit_macro(self, mac);
        }
    }

    let mut v = V {
        in_verified: 0,
        any: false,
    };
    v.visit_file(file);
    v.any
}

fn has_qed_verified_attr(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if !attr.path().is_ident("qed") {
            continue;
        }
        if let syn::Meta::List(list) = &attr.meta {
            if list.tokens.to_string().contains("verified") {
                return true;
            }
        }
    }
    false
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
    let mut completeness = check_completeness(&spec);

    // 2. Code drift + residual `todo!()` lint (both code-aware).
    let code_drift = if let Some(dir) = code_dir {
        completeness.extend(check_handler_todos(&spec, dir)?);
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

    // 4. Lean coverage
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
    // Spec completeness — tally through the exhaustive-match counter so
    // Error entries can't drop out of the summary (#260/#270).
    let counts = SeverityCounts::of(&report.completeness);

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
    eprintln!(
        "  {} error(s), {} warning(s), {} info\n",
        counts.errors, counts.warnings, counts.infos
    );

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
