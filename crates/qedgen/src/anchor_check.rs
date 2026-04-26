//! Cross-check spec handlers against an Anchor program (v2.9 M5).
//!
//! `qedgen check --anchor-project <path>` opt-in: given the user's
//! existing Anchor program crate, parse its `#[program]` mod and
//! verify that the spec's handler set lines up with the program's
//! instruction set. Catches two real adoption-time slips:
//!
//!   1. **Typo / stale spec** — a `handler foo { ... }` block in the
//!      spec that has no matching `pub fn foo(...)` in the program
//!      mod. Either the user renamed the instruction in code and
//!      forgot to update the spec, or the spec was authored against
//!      a different program.
//!   2. **Uncovered handler** — a `pub fn bar(...)` in the program
//!      mod with no corresponding spec handler. Verification can't
//!      say anything about a handler that isn't modelled. The user
//!      either needs to add a spec block or mark the instruction as
//!      out-of-scope.
//!
//! Pure read: no codegen, no writes. Intended for CI gates.

use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

use crate::anchor_project::parse_anchor_project;
use crate::anchor_resolver::{resolve_handler, HandlerLocation};
use crate::check::ParsedSpec;

/// Cross-check finding from comparing spec handlers to program
/// instructions. Severity is fixed to "error" for both shapes —
/// either condition means the spec and the deployed program disagree
/// about what handlers exist.
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

/// One handler-effect coverage finding: a spec effect whose target
/// state field is *not* referenced as the LHS of any assignment-like
/// expression in the resolved Rust handler body. Heuristic but
/// useful for catching the "I added an effect to the spec but
/// forgot to wire it in code" footgun.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectCoverageFinding {
    pub handler: String,
    pub field: String,
    /// What we observed instead. `None` = the body has no assignment
    /// to a path matching this field anywhere; the user likely
    /// hasn't implemented the effect yet.
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

/// Walk a `syn::Block` and collect the set of field-name "leaves" that
/// appear on the LHS of any assignment-like expression. `self.x.y = z`
/// contributes `y`; `self.balance += 1` contributes `balance`. Used by
/// the effect-coverage check to compare against the spec's effect
/// field set.
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

/// Pull the trailing field name out of an LHS expression. `self.x.y`
/// → `y`; `*ptr.field` → `field`; bare ident `foo` → `foo`. Returns
/// `None` for shapes we can't pin a leaf to (calls, indexing, etc.).
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

/// Cross-check: for every spec handler's effect block, verify the
/// resolved Rust handler body contains at least one assignment-like
/// mutation whose LHS leaf matches the effect field. Misses are
/// reported as `EffectCoverageFinding`s — heuristic, not a proof of
/// semantic correctness, but catches the "spec effect with no code
/// wire-up" case that body-hash sealing alone doesn't surface.
///
/// Handlers that resolve to `Unrecognized` are skipped (no body to
/// inspect); the existing `check_anchor_coverage` already calls them
/// out.
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
        let mut field_names: Vec<&String> = handler.effects.iter().map(|(f, _, _)| f).collect();
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

/// Run the cross-check. Returns the list of findings (empty when the
/// two handler sets agree exactly).
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

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

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
        // `gamma` is spec-only; `beta` is program-only. Spec-only
        // findings come first, both groups are alphabetised.
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
        // `state.field = value;` (not just `+=`) should also count.
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
        // Custom-dispatcher shape — classifier returns Unrecognized.
        // Effect coverage should skip silently rather than blow up.
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
