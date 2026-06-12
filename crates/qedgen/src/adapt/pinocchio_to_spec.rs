//! Pinocchio source → `.qedspec` skeleton (v2.19 M1.7).
//!
//! Walks a Pinocchio program's source tree, enumerates handlers by the
//! `pub fn process_*` convention, and emits a structural `.qedspec`
//! skeleton with empty handler stubs. M1.8's emitter takes this skeleton
//! and merges the user's ratified clauses (from M1.6) into the right
//! handler / invariant blocks.
//!
//! Out of scope for v1:
//! - Account-list inference from `&[AccountInfo]` destructuring patterns
//! - Parameter-type inference from `instruction_data` parses
//! - Error-enum inference from `TokenError` (Pinocchio-specific) and
//!   `ProgramError` returns
//!
//! These are reachable additions in v2.20 (the AST-round-trip milestone)
//! — for v1 the skeleton is structural-only and ratified clauses do the
//! semantic lifting.

use anyhow::Result;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Render a structural `.qedspec` skeleton for a Pinocchio program.
///
/// `program_name` is the human-readable spec name (typically derived
/// from the project directory). The output is a complete, parseable
/// `.qedspec` minus semantic clauses — handlers are present but bodies
/// are empty.
pub fn render_skeleton(project_root: &Path, program_name: &str) -> Result<String> {
    let handlers = enumerate_handlers(project_root)?;
    Ok(render_skeleton_from_handlers(&handlers, program_name))
}

/// Native variant — accepts any `pub fn` as a candidate handler.
/// Native programs don't have a canonical naming prefix.
pub fn render_skeleton_native(project_root: &Path, program_name: &str) -> Result<String> {
    let handlers = enumerate_handlers_with_prefix(project_root, "")?;
    Ok(render_skeleton_from_handlers(&handlers, program_name))
}

/// Hardened variant used by tests: takes handler names directly so the
/// rendering is exercised independent of the source walker.
pub fn render_skeleton_from_handlers(handlers: &[String], program_name: &str) -> String {
    let pascal = to_pascal_case(program_name);
    let mut s = String::new();
    s.push_str("// Skeleton emitted by `qedgen probe --emit-spec-candidates` (Pinocchio).\n");
    s.push_str("// Empty handler stubs only — semantic clauses (requires / effect / transfers /\n");
    s.push_str("// invariants) are written by the interview-ratification step.\n\n");
    s.push_str(&format!("spec {}\n\n", pascal));

    // Minimal state ADT — the user refines this post-interview as the
    // program's actual lifecycle becomes clear from accepted clauses.
    s.push_str("// TODO: replace with the program's actual lifecycle states.\n");
    s.push_str("type State\n");
    s.push_str("  | Init\n");
    s.push_str("  | Active\n\n");

    // Minimal error enum — placeholder, refined as the user maps
    // handler-level `else <Err>` clauses through the interview.
    s.push_str("// TODO: list domain errors raised by handlers. Mirror the program's\n");
    s.push_str("// TokenError / ProgramError enum here as ratified clauses reference them.\n");
    s.push_str("type Error\n");
    s.push_str("  | InvalidArgument\n");
    s.push_str("  | Unauthorized\n\n");

    if handlers.is_empty() {
        s.push_str("// No `pub fn process_*` handlers discovered under the project root.\n");
        s.push_str("// Add handler declarations manually or verify the source-walk worked.\n");
    } else {
        for h in handlers {
            s.push_str(&format!("/// `{}` — discovered via source-walk\n", h));
            s.push_str(&format!("handler {} : State.Init -> State.Active {{\n", h));
            s.push_str("  // accounts, requires, effect, transfers — filled by interview\n");
            s.push_str("}\n\n");
        }
    }

    s
}

/// Enumerate handler names from `pub fn process_*` declarations under
/// the project root. Walks every `.rs` file once; matches are
/// deduplicated and returned in sorted order so the skeleton is
/// deterministic.
pub fn enumerate_handlers(project_root: &Path) -> Result<Vec<String>> {
    enumerate_handlers_with_prefix(project_root, "process_")
}

/// Variant accepting an arbitrary prefix. Pinocchio convention is
/// `process_*`; Native programs vary (e.g., `processor::do_thing`,
/// `entrypoint`, or bare names). Pass `""` to accept every `pub fn`.
pub fn enumerate_handlers_with_prefix(project_root: &Path, prefix: &str) -> Result<Vec<String>> {
    let rs_files = collect_rust_files(project_root)?;
    let mut handlers: BTreeSet<String> = BTreeSet::new();
    let pattern = if prefix.is_empty() {
        r"^\s*pub(?:\([^)]+\))?\s+fn\s+([A-Za-z][A-Za-z0-9_]*)\s*\(".to_string()
    } else {
        format!(
            r"^\s*pub(?:\([^)]+\))?\s+fn\s+({}[A-Za-z0-9_]*)\s*\(",
            regex::escape(prefix)
        )
    };
    let re = regex::Regex::new(&pattern).expect("static regex compiles");

    for file in &rs_files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        for line in source.lines() {
            if let Some(caps) = re.captures(line) {
                handlers.insert(caps[1].to_string());
            }
        }
    }

    Ok(handlers.into_iter().collect())
}

/// Walk the project root collecting `.rs` files. Mirrors
/// `pinocchio_probe::collect_rust_files` (kept local so the two scanners
/// can diverge if Pinocchio's source layout differs from the probe's
/// site-scan rules).
fn collect_rust_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        // Skip vendor/build dirs that shouldn't contribute handlers.
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| matches!(n, "target" | ".git" | "node_modules" | "tests" | "fuzz"))
        {
            continue;
        }
        if path.is_dir() {
            walk(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// snake_case → PascalCase for the `spec Name` declaration. Borrows
/// from `anchor_adapt::to_pascal_case` semantics: split on underscores
/// and dashes, capitalize each segment.
fn to_pascal_case(name: &str) -> String {
    name.split(|c: char| c == '_' || c == '-' || !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut chars = s.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_has_required_top_level_decls() {
        let handlers = vec!["process_transfer".to_string(), "process_burn".to_string()];
        let out = render_skeleton_from_handlers(&handlers, "ptoken");
        assert!(out.contains("spec Ptoken"));
        assert!(out.contains("type State"));
        assert!(out.contains("type Error"));
        assert!(out.contains("handler process_transfer"));
        assert!(out.contains("handler process_burn"));
    }

    #[test]
    fn empty_handler_set_renders_placeholder_note() {
        let out = render_skeleton_from_handlers(&[], "test");
        assert!(out.contains("No `pub fn process_*` handlers discovered"));
    }

    #[test]
    fn pascal_case_handles_snake_case_and_dashes() {
        assert_eq!(to_pascal_case("my_program"), "MyProgram");
        assert_eq!(to_pascal_case("my-program"), "MyProgram");
        assert_eq!(to_pascal_case("p-token"), "PToken");
        assert_eq!(to_pascal_case("single"), "Single");
    }

    #[test]
    fn handler_lines_are_sorted_and_deduplicated() {
        let handlers = [
            "process_transfer".to_string(),
            "process_burn".to_string(),
            "process_transfer".to_string(),
        ];
        let set: BTreeSet<_> = handlers.iter().cloned().collect();
        let sorted: Vec<_> = set.into_iter().collect();
        let out = render_skeleton_from_handlers(&sorted, "p");
        let pos_burn = out.find("handler process_burn").unwrap();
        let pos_transfer = out.find("handler process_transfer").unwrap();
        assert!(pos_burn < pos_transfer, "alphabetical order");
        // Only one occurrence per handler.
        assert_eq!(out.matches("handler process_transfer").count(), 1);
    }

    #[test]
    fn enumerate_handlers_finds_pinocchio_style_decls() {
        // Build a fake source tree with a couple of handler files
        // and verify the enumerator picks them up.
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/processor")).unwrap();
        std::fs::write(
            root.join("src/processor/transfer.rs"),
            "pub fn process_transfer(accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult { Ok(()) }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/processor/burn.rs"),
            "pub fn process_burn(accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult { Ok(()) }\n\
             fn helper() {}\n",
        )
        .unwrap();
        // Should NOT be picked up — private function, no pub.
        std::fs::write(root.join("src/util.rs"), "fn process_internal() {}\n").unwrap();

        let handlers = enumerate_handlers(root).unwrap();
        assert_eq!(handlers, vec!["process_burn", "process_transfer"]);
    }

    #[test]
    fn enumerate_skips_target_and_tests_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("target/debug/junk.rs"),
            "pub fn process_should_be_skipped() {}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("tests/integration.rs"),
            "pub fn process_in_tests_should_be_skipped() {}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/real.rs"),
            "pub fn process_real() -> Result<(), Error> { Ok(()) }\n",
        )
        .unwrap();

        let handlers = enumerate_handlers(root).unwrap();
        assert_eq!(handlers, vec!["process_real"]);
    }
}
