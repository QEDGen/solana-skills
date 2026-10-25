#[path = "../src/probe/crucible_artifacts.rs"]
mod crucible_artifacts;
use crucible_artifacts::*;

#[test]
fn identical_inputs_reuse_manual_source_edits() {
    let dir = tempfile::tempdir().unwrap();
    let inputs = GenerationInputs::new("domain", Some(b"spec"), Some(b"idl"), Some(b"bindings"));
    record_generation(dir.path(), &inputs).unwrap();
    std::fs::write(dir.path().join("manual.rs"), "hand wired accounts").unwrap();
    validate_generation(dir.path(), &inputs).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("manual.rs")).unwrap(),
        "hand wired accounts"
    );
}

#[test]
fn each_generation_input_change_blocks_reuse_without_overwriting_source() {
    for changed in [
        GenerationInputs::new("protocol", Some(b"spec"), Some(b"idl"), Some(b"bindings")),
        GenerationInputs::new("domain", Some(b"other"), Some(b"idl"), Some(b"bindings")),
        GenerationInputs::new("domain", Some(b"spec"), Some(b"other"), Some(b"bindings")),
        GenerationInputs::new("domain", Some(b"spec"), Some(b"idl"), Some(b"other")),
        GenerationInputs::new("domain", Some(b"spec"), None, Some(b"bindings")),
    ] {
        let dir = tempfile::tempdir().unwrap();
        record_generation(
            dir.path(),
            &GenerationInputs::new("domain", Some(b"spec"), Some(b"idl"), Some(b"bindings")),
        )
        .unwrap();
        std::fs::write(dir.path().join("manual.rs"), "manual").unwrap();
        assert!(validate_generation(dir.path(), &changed)
            .unwrap_err()
            .to_string()
            .contains("regenerate"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("manual.rs")).unwrap(),
            "manual"
        );
    }
}

#[test]
fn legacy_and_corrupt_manifests_cannot_establish_freshness() {
    let dir = tempfile::tempdir().unwrap();
    let inputs = GenerationInputs::new("domain", None, None, None);
    assert!(validate_generation(dir.path(), &inputs).is_err());
    record_generation(dir.path(), &inputs).unwrap();
    std::fs::write(dir.path().join(".qedgen/generation-inputs.json"), "{}").unwrap();
    assert!(validate_generation(dir.path(), &inputs).is_err());
}

#[test]
fn manual_action_layout_change_invalidates_domain_seed_ordinals() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("src/main.rs");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, "struct Fixture; impl Fixture { pub fn action_a(&mut self, amount: u64) -> bool { true } pub fn action_b(&mut self) -> bool { true } }").unwrap();
    let inputs = GenerationInputs::new("domain", Some(b"spec"), None, None);
    record_generation(dir.path(), &inputs).unwrap();
    std::fs::write(&source, "struct Fixture; impl Fixture { pub fn action_a(&mut self, amount: u64) -> bool { false } pub fn action_b(&mut self) -> bool { true } }").unwrap();
    validate_generation(dir.path(), &inputs).unwrap();
    std::fs::write(&source, "struct Fixture; impl Fixture { pub fn action_b(&mut self) -> bool { true } pub fn action_a(&mut self, amount: u64) -> bool { false } }").unwrap();
    assert!(validate_generation(dir.path(), &inputs).is_err());
}
