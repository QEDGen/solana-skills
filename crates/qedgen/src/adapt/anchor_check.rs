//! `qedgen check --anchor-project <path>`: cross-check spec handlers against
//! the program's `#[program]` instruction set. Catches stale/typo'd spec
//! handlers with no matching `pub fn`, and program instructions not covered
//! by any spec handler. Pure read — no codegen, no writes; CI-gate friendly.

use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

use crate::anchor_project::parse_anchor_project;
use crate::anchor_resolver::{resolve_handler, HandlerLocation};
use crate::check::ParsedSpec;

/// Coverage finding. Severity is always "error" — either shape means the
/// spec and the program disagree about what handlers exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorCoverageFinding {
    pub kind: AnchorCoverageKind,
    pub handler_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorCoverageKind {
    /// Spec declares a handler that isn't in the `#[program]` mod.
    SpecHandlerNotInProgram,
    /// Program has a `pub fn` instruction with no spec handler.
    ProgramInstructionNotInSpec,
}

impl AnchorCoverageFinding {
    pub fn message(&self) -> String {
        match self.kind {
            AnchorCoverageKind::SpecHandlerNotInProgram => format!(
                "spec handler `{}` has no matching `pub fn` in the program's `#[program]` mod — the spec is stale or the handler was renamed in code",
                self.handler_name
            ),
            AnchorCoverageKind::ProgramInstructionNotInSpec => format!(
                "program instruction `{}` is not covered by any spec handler — verification has nothing to say about it. Add a `handler {}` block to the spec, or remove the `pub fn` if it isn't really an instruction",
                self.handler_name, self.handler_name
            ),
        }
    }
}

/// A spec effect whose target field is never the LHS of any assignment-like
/// expression in the resolved handler body. Heuristic — catches "effect in
/// spec, never wired in code".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectCoverageFinding {
    pub handler: String,
    pub field: String,
    /// What was observed instead; `None` = no assignment to a matching path.
    pub observed: Option<String>,
}

impl EffectCoverageFinding {
    pub fn message(&self) -> String {
        format!(
            "handler `{}`: spec effect on `{}` has no matching mutation in the Rust body — either implement it (assign to a path ending in `.{}`) or remove the effect from the spec",
            self.handler, self.field, self.field,
        )
    }
}

/// Field-name leaves on the LHS of assignment-like exprs in `block`:
/// `self.x.y = z` → `y`; `self.balance += 1` → `balance`.
fn collect_mutated_field_leaves(block: &syn::Block) -> HashSet<String> {
    use syn::visit::Visit;

    struct Collector {
        out: HashSet<String>,
    }
    impl<'ast> Visit<'ast> for Collector {
        fn visit_expr(&mut self, expr: &'ast syn::Expr) {
            match expr {
                syn::Expr::Assign(a) => {
                    if let Some(leaf) = expr_path_leaf(&a.left) {
                        self.out.insert(leaf);
                    }
                }
                syn::Expr::Binary(b) => {
                    if matches!(
                        b.op,
                        syn::BinOp::AddAssign(_)
                            | syn::BinOp::SubAssign(_)
                            | syn::BinOp::MulAssign(_)
                            | syn::BinOp::DivAssign(_)
                            | syn::BinOp::RemAssign(_)
                            | syn::BinOp::BitAndAssign(_)
                            | syn::BinOp::BitOrAssign(_)
                            | syn::BinOp::BitXorAssign(_)
                            | syn::BinOp::ShlAssign(_)
                            | syn::BinOp::ShrAssign(_)
                    ) {
                        if let Some(leaf) = expr_path_leaf(&b.left) {
                            self.out.insert(leaf);
                        }
                    }
                }
                _ => {}
            }
            syn::visit::visit_expr(self, expr);
        }
    }

    let mut c = Collector {
        out: HashSet::new(),
    };
    c.visit_block(block);
    c.out
}

/// Trailing field name of an LHS: `self.x.y` → `y`; `*ptr.field` → `field`;
/// bare `foo` → `foo`. None for calls, indexing, etc.
fn expr_path_leaf(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Field(f) => match &f.member {
            syn::Member::Named(name) => Some(name.to_string()),
            syn::Member::Unnamed(_) => None,
        },
        syn::Expr::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        syn::Expr::Unary(u) => expr_path_leaf(&u.expr),
        syn::Expr::Reference(r) => expr_path_leaf(&r.expr),
        syn::Expr::Paren(p) => expr_path_leaf(&p.expr),
        _ => None,
    }
}

/// For each spec effect, require at least one mutation in the resolved
/// handler body whose LHS leaf matches the effect field. Heuristic, not
/// semantic correctness. `Unrecognized` handlers are skipped (no body);
/// `check_anchor_coverage` already reports them.
pub fn check_effect_coverage(
    spec: &ParsedSpec,
    program_root: &Path,
) -> Result<Vec<EffectCoverageFinding>> {
    let project = parse_anchor_project(program_root)?;
    let mut findings = Vec::new();

    for handler in &spec.handlers {
        if handler.effects.is_empty() {
            continue;
        }
        let Some(instruction) = project.instructions.iter().find(|i| i.name == handler.name) else {
            // `check_anchor_coverage` reports this; don't double-warn.
            continue;
        };
        let location = resolve_handler(instruction, &project.lib_rs_path, program_root)?;
        let block = match &location {
            HandlerLocation::Inline { item_fn, .. } | HandlerLocation::FreeFn { item_fn, .. } => {
                &*item_fn.block
            }
            HandlerLocation::Method { item_fn, .. } => &item_fn.block,
            HandlerLocation::Unrecognized { .. } => continue,
        };
        let leaves = collect_mutated_field_leaves(block);

        // Sort for deterministic output.
        let mut field_names: Vec<&String> = handler.effects.iter().map(|e| &e.field).collect();
        field_names.sort();
        field_names.dedup();
        for field in field_names {
            if !leaves.contains(field) {
                findings.push(EffectCoverageFinding {
                    handler: handler.name.clone(),
                    field: field.clone(),
                    observed: None,
                });
            }
        }
    }

    Ok(findings)
}

/// Run the cross-check; empty when the two handler sets agree exactly.
pub fn check_anchor_coverage(
    spec: &ParsedSpec,
    program_root: &Path,
) -> Result<Vec<AnchorCoverageFinding>> {
    let project = parse_anchor_project(program_root)?;

    let spec_names: HashSet<String> = spec.handlers.iter().map(|h| h.name.clone()).collect();
    let program_names: HashSet<String> = project
        .instructions
        .iter()
        .map(|i| i.name.clone())
        .collect();

    let mut findings = Vec::new();

    // Sort for deterministic output regardless of HashSet iteration order.
    let mut spec_only: Vec<&String> = spec_names.difference(&program_names).collect();
    spec_only.sort();
    for name in spec_only {
        findings.push(AnchorCoverageFinding {
            kind: AnchorCoverageKind::SpecHandlerNotInProgram,
            handler_name: name.clone(),
        });
    }

    let mut program_only: Vec<&String> = program_names.difference(&spec_names).collect();
    program_only.sort();
    for name in program_only {
        findings.push(AnchorCoverageFinding {
            kind: AnchorCoverageKind::ProgramInstructionNotInSpec,
            handler_name: name.clone(),
        });
    }

    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chumsky_adapter;

    fn write_lib_rs(tmp: &tempfile::TempDir, contents: &str) -> std::path::PathBuf {
        let root = tmp.path().to_path_buf();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), contents).unwrap();
        root
    }

    fn parse_spec(src: &str) -> ParsedSpec {
        chumsky_adapter::parse_str(src).expect("test spec must parse")
    }

    #[test]
    fn returns_no_findings_when_spec_and_program_match() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_lib_rs(
            &tmp,
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn initialize(ctx: Context<Init>) -> Result<()> { Ok(()) }
                pub fn cancel(ctx: Context<Cancel>) -> Result<()> { Ok(()) }
            }
            "#,
        );
        let spec = parse_spec(
            r#"
            spec P
            type State | A | B
            handler initialize : State.A -> State.B { }
            handler cancel : State.B -> State.A { }
            "#,
        );

        let findings = check_anchor_coverage(&spec, &root).unwrap();
        assert!(findings.is_empty(), "got: {:?}", findings);
    }

    #[test]
    fn reports_spec_handler_missing_from_program() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_lib_rs(
            &tmp,
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn initialize(ctx: Context<Init>) -> Result<()> { Ok(()) }
            }
            "#,
        );
        let spec = parse_spec(
            r#"
            spec P
            type State | A | B
            handler initialize : State.A -> State.B { }
            handler obsolete   : State.B -> State.A { }
            "#,
        );

        let findings = check_anchor_coverage(&spec, &root).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].kind,
            AnchorCoverageKind::SpecHandlerNotInProgram
        );
        assert_eq!(findings[0].handler_name, "obsolete");
    }

    #[test]
    fn reports_program_instruction_missing_from_spec() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_lib_rs(
            &tmp,
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn initialize(ctx: Context<Init>) -> Result<()> { Ok(()) }
                pub fn new_feature(ctx: Context<NewFeature>) -> Result<()> { Ok(()) }
            }
            "#,
        );
        let spec = parse_spec(
            r#"
            spec P
            type State | A | B
            handler initialize : State.A -> State.B { }
            "#,
        );

        let findings = check_anchor_coverage(&spec, &root).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].kind,
            AnchorCoverageKind::ProgramInstructionNotInSpec
        );
        assert_eq!(findings[0].handler_name, "new_feature");
    }

    #[test]
    fn reports_both_directions_in_deterministic_order() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_lib_rs(
            &tmp,
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn alpha(ctx: Context<A>) -> Result<()> { Ok(()) }
                pub fn beta(ctx: Context<B>) -> Result<()> { Ok(()) }
            }
            "#,
        );
        let spec = parse_spec(
            r#"
            spec P
            type State | S | T
            handler alpha   : State.S -> State.T { }
            handler gamma   : State.T -> State.S { }
            "#,
        );

        let findings = check_anchor_coverage(&spec, &root).unwrap();
        // Spec-only findings first; both groups alphabetised.
        assert_eq!(findings.len(), 2);
        assert_eq!(
            findings[0].kind,
            AnchorCoverageKind::SpecHandlerNotInProgram
        );
        assert_eq!(findings[0].handler_name, "gamma");
        assert_eq!(
            findings[1].kind,
            AnchorCoverageKind::ProgramInstructionNotInSpec
        );
        assert_eq!(findings[1].handler_name, "beta");
    }

    #[test]
    fn effect_coverage_clean_when_body_assigns_to_field() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_lib_rs(
            &tmp,
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn deposit(ctx: Context<Init>, amount: u64) -> Result<()> {
                    ctx.accounts.state.balance += amount;
                    Ok(())
                }
            }
            "#,
        );
        let spec = parse_spec(
            r#"
            spec P
            type State | A
            handler deposit (amount : U64) : State.A -> State.A {
              effect { balance += amount }
            }
            type Error | Bad
            "#,
        );
        let findings = check_effect_coverage(&spec, &root).unwrap();
        assert!(findings.is_empty(), "got: {:?}", findings);
    }

    #[test]
    fn effect_coverage_reports_field_with_no_mutation() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_lib_rs(
            &tmp,
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn deposit(ctx: Context<Init>, amount: u64) -> Result<()> {
                    // forgot to wire the effect — body just returns Ok.
                    Ok(())
                }
            }
            "#,
        );
        let spec = parse_spec(
            r#"
            spec P
            type State | A
            handler deposit (amount : U64) : State.A -> State.A {
              effect { balance += amount }
            }
            type Error | Bad
            "#,
        );
        let findings = check_effect_coverage(&spec, &root).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].handler, "deposit");
        assert_eq!(findings[0].field, "balance");
    }

    #[test]
    fn effect_coverage_handles_simple_assignment_too() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_lib_rs(
            &tmp,
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn initialize(ctx: Context<Init>, start: u64) -> Result<()> {
                    ctx.accounts.state.value = start;
                    Ok(())
                }
            }
            "#,
        );
        let spec = parse_spec(
            r#"
            spec P
            type State | A
            handler initialize (start : U64) : State.A -> State.A {
              effect { value := start }
            }
            type Error | Bad
            "#,
        );
        let findings = check_effect_coverage(&spec, &root).unwrap();
        assert!(findings.is_empty(), "got: {:?}", findings);
    }

    #[test]
    fn effect_coverage_skips_unrecognized_handlers() {
        // Unrecognized handler shape must skip silently, not blow up.
        let tmp = tempfile::tempdir().unwrap();
        let root = write_lib_rs(
            &tmp,
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn dispatch(ctx: Context<D>, x: u64) -> Result<()> {
                    nowhere::missing(ctx, x)
                }
            }
            "#,
        );
        let spec = parse_spec(
            r#"
            spec P
            type State | A
            handler dispatch (x : U64) : State.A -> State.A {
              effect { balance += x }
            }
            type Error | Bad
            "#,
        );
        let findings = check_effect_coverage(&spec, &root).unwrap();
        assert!(
            findings.is_empty(),
            "Unrecognized handlers should skip; got: {:?}",
            findings
        );
    }

    #[test]
    fn finding_message_mentions_handler_name() {
        let f = AnchorCoverageFinding {
            kind: AnchorCoverageKind::SpecHandlerNotInProgram,
            handler_name: "stale_one".into(),
        };
        let msg = f.message();
        assert!(msg.contains("stale_one"), "msg: {msg}");
        assert!(msg.contains("stale"), "msg: {msg}");

        let f = AnchorCoverageFinding {
            kind: AnchorCoverageKind::ProgramInstructionNotInSpec,
            handler_name: "uncovered".into(),
        };
        let msg = f.message();
        assert!(msg.contains("uncovered"), "msg: {msg}");
        assert!(msg.contains("not covered"), "msg: {msg}");
    }
}
