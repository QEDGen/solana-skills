//! Pinocchio source → structural `.qedspec` skeleton: enumerate handlers by
//! the `pub fn process_*` convention and emit empty handler stubs; the
//! ratification step merges accepted clauses in. Account-list, parameter-type,
//! and error-enum inference are out of scope — the skeleton is
//! structural-only and ratified clauses do the semantic lifting.

use anyhow::Result;
use std::collections::BTreeSet;
use std::path::Path;

use crate::program_model::{
    HandlerModel, HandlerShape, ProgramAdapter, ProgramFramework, ProgramModel,
};

pub struct PinocchioAdapter<'a> {
    program_name: &'a str,
}

impl<'a> PinocchioAdapter<'a> {
    pub fn new(program_name: &'a str) -> Self {
        Self { program_name }
    }
}

impl ProgramAdapter for PinocchioAdapter<'_> {
    fn framework(&self) -> ProgramFramework {
        ProgramFramework::Pinocchio
    }

    fn detect(&self, root: &Path) -> bool {
        enumerate_handlers(root)
            .map(|handlers| !handlers.is_empty())
            .unwrap_or(false)
    }

    fn extract(&self, root: &Path) -> Result<ProgramModel> {
        let handlers = enumerate_handlers(root)?;
        Ok(program_model_from_handlers(
            &handlers,
            self.program_name,
            ProgramFramework::Pinocchio,
        ))
    }

    fn render_spec(&self, model: &ProgramModel) -> Result<String> {
        Ok(render_skeleton_from_model(model))
    }
}

pub struct NativeAdapter<'a> {
    program_name: &'a str,
}

impl<'a> NativeAdapter<'a> {
    pub fn new(program_name: &'a str) -> Self {
        Self { program_name }
    }
}

impl ProgramAdapter for NativeAdapter<'_> {
    fn framework(&self) -> ProgramFramework {
        ProgramFramework::Native
    }

    fn detect(&self, root: &Path) -> bool {
        enumerate_handlers_with_prefix(root, "")
            .map(|handlers| !handlers.is_empty())
            .unwrap_or(false)
    }

    fn extract(&self, root: &Path) -> Result<ProgramModel> {
        let handlers = enumerate_handlers_with_prefix(root, "")?;
        Ok(program_model_from_handlers(
            &handlers,
            self.program_name,
            ProgramFramework::Native,
        ))
    }

    fn render_spec(&self, model: &ProgramModel) -> Result<String> {
        Ok(render_skeleton_from_model(model))
    }
}

/// Takes handler names directly so rendering is testable independent of the
/// source walker.
#[cfg_attr(not(test), allow(dead_code))] // production goes through render_skeleton_from_model; kept as the test entry
pub fn render_skeleton_from_handlers(handlers: &[String], program_name: &str) -> String {
    let model = program_model_from_handlers(handlers, program_name, ProgramFramework::Pinocchio);
    render_skeleton_from_model(&model)
}

pub fn program_model_from_handlers(
    handlers: &[String],
    program_name: &str,
    framework: ProgramFramework,
) -> ProgramModel {
    let mut model = ProgramModel::new(framework, program_name);
    model.handlers = handlers
        .iter()
        .map(|name| HandlerModel {
            name: name.clone(),
            args: Vec::new(),
            accounts_type: None,
            accounts: Vec::new(),
            source_path: None,
            shape: HandlerShape::SourceWalk,
        })
        .collect();
    model
}

pub fn render_skeleton_from_model(model: &ProgramModel) -> String {
    let pascal = to_pascal_case(&model.name);
    let mut s = String::new();
    s.push_str("// Skeleton emitted by `qedgen probe --emit-spec-candidates` (Pinocchio).\n");
    s.push_str("// Empty handler stubs only — semantic clauses (requires / effect / transfers /\n");
    s.push_str("// invariants) are written by the interview-ratification step.\n\n");
    s.push_str(&format!("spec {}\n\n", pascal));

    // Minimal state ADT; refined post-interview.
    s.push_str("// TODO: replace with the program's actual lifecycle states.\n");
    s.push_str("type State\n");
    s.push_str("  | Init\n");
    s.push_str("  | Active\n\n");

    // Placeholder error enum; refined through the interview.
    s.push_str("// TODO: list domain errors raised by handlers. Mirror the program's\n");
    s.push_str("// TokenError / ProgramError enum here as ratified clauses reference them.\n");
    s.push_str("type Error\n");
    s.push_str("  | InvalidArgument\n");
    s.push_str("  | Unauthorized\n\n");

    if model.handlers.is_empty() {
        s.push_str("// No `pub fn process_*` handlers discovered under the project root.\n");
        s.push_str("// Add handler declarations manually or verify the source-walk worked.\n");
    } else {
        for handler in &model.handlers {
            s.push_str(&format!(
                "/// `{}` — discovered via source-walk\n",
                handler.name
            ));
            s.push_str(&format!(
                "handler {} : State.Init -> State.Active {{\n",
                handler.name
            ));
            s.push_str("  // accounts, requires, effect, transfers — filled by interview\n");
            s.push_str("}\n\n");
        }
    }

    s
}

/// Handler names from `pub fn process_*` declarations under the project
/// root; deduplicated and sorted so the skeleton is deterministic.
pub fn enumerate_handlers(project_root: &Path) -> Result<Vec<String>> {
    enumerate_handlers_with_prefix(project_root, "process_")
}

/// Arbitrary-prefix variant: Pinocchio convention is `process_*`; Native
/// naming varies. Pass `""` to accept every `pub fn`.
pub fn enumerate_handlers_with_prefix(project_root: &Path, prefix: &str) -> Result<Vec<String>> {
    let rs_files =
        crate::fs_walk::collect_rs_files(project_root, crate::fs_walk::DEFAULT_SKIP_DIRS);
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

/// snake/kebab-case → PascalCase for the `spec Name` declaration.
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
        assert_eq!(out.matches("handler process_transfer").count(), 1);
    }

    #[test]
    fn builds_program_model_from_handlers() {
        let handlers = vec!["process_transfer".to_string()];
        let model = program_model_from_handlers(&handlers, "p-token", ProgramFramework::Pinocchio);

        assert_eq!(model.framework, ProgramFramework::Pinocchio);
        assert_eq!(model.name, "p-token");
        assert_eq!(model.handlers.len(), 1);
        assert_eq!(model.handlers[0].name, "process_transfer");
        assert_eq!(model.handlers[0].shape, HandlerShape::SourceWalk);
    }

    #[test]
    fn pinocchio_adapter_detects_extracts_and_renders() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn process_transfer(accounts: &[AccountInfo]) -> ProgramResult { Ok(()) }\n",
        )
        .unwrap();
        let adapter = PinocchioAdapter::new("p-token");

        assert_eq!(adapter.framework(), ProgramFramework::Pinocchio);
        assert!(adapter.detect(root));

        let model = adapter.extract(root).unwrap();
        assert_eq!(model.handlers.len(), 1);
        assert_eq!(model.handlers[0].name, "process_transfer");
        let rendered = adapter.render_spec(&model).unwrap();
        assert!(rendered.contains("spec PToken"));
        assert!(rendered.contains("handler process_transfer"));
    }

    #[test]
    fn native_adapter_uses_all_public_functions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn initialize() {}\nfn helper() {}\npub fn close_account() {}\n",
        )
        .unwrap();
        let adapter = NativeAdapter::new("native-program");

        assert_eq!(adapter.framework(), ProgramFramework::Native);
        assert!(adapter.detect(root));

        let model = adapter.extract(root).unwrap();
        let names: Vec<&str> = model.handlers.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["close_account", "initialize"]);
        assert!(adapter
            .render_spec(&model)
            .unwrap()
            .contains("handler close_account"));
    }

    #[test]
    fn enumerate_handlers_finds_pinocchio_style_decls() {
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
