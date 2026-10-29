# Label-Driven Proof Generation on a Pinocchio Fixture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One command takes a small Pinocchio program from source to kernel-checked Lean success-path theorems, with every success edge, authority checkpoint, no-return helper and readable field discovered from the binary pair instead of entered by hand.

**Architecture:** `qedgen build` (this repo) makes the V0 `debug = 2` pair. A new qedsvm tool, `qed-discover`, reads the pair plus the concept map and writes a versioned bindings file. A generalized `qed-success-proofs` turns the bindings into Lean modules that qedsvm's Lake project checks. A new `qedgen lift` command (this repo) runs the chain against a pinned qedsvm checkout and writes a report.

**Tech Stack:** Rust (qedgen, `qed-analysis` on `solana-sbpf` 0.14.4, gimli), Lean 4 v4.30.0 (qedsvm `SVM.SBPF.*`), Pinocchio 0.9.3, `solana-verify` 0.5.2, platform-tools v1.57.

**Spec:** `docs/superpowers/specs/2026-10-28-label-driven-fixture-design.md`

## Global Constraints

- Two repositories. `Q` tasks are in `/Users/abishek/code/qedsvm` (branch from its `main`). `S` tasks are in this repository (branch from `main`). Each task names its repository.
- sBPF V0 for the fixture; build with `--arch v0` and `--tools-version v1.57`. Do not change `DEFAULT_ARCH` (`"v3"`) in `crates/qedgen/src/verifiable_build/mod.rs`.
- New Lean checkers and theorems take `fetch : Nat → Option Insn` and must not mention V0. Only the emitter's version gate (`success_proofs.rs:254`) and the decode call (`Decode.decodeProgramV0`) are V0-specific, so the later v3 port touches only those.
- `[profile.release] debug = 2` in the fixture.
- No hand-entered PCs, success edges, checkpoints, guard anchors, no-return helpers or field offsets anywhere in the fixture path. The only allowed per-program input is the binary pair.
- The p-token baseline from PR #88 must still regenerate **byte for byte**: `qed-success-proofs qedsvm-rs/tests/fixtures/p_token.so examples/lean/PToken/success-proof-inputs.json examples/lean/PToken --check` passes after every Q task.
- "Checked" means a Lean theorem proves the byte relation. DWARF names and dispatch-to-handler matches are labels.
- Axiom gate: allow `propext`, `Classical.choice`, `Quot.sound` and names matching `._native.native_decide.ax_` and nothing else.
- `qedgen lift` writes its own report under `.qed/lift/<library>/`. It never writes `.qed/verify-evidence.json`.
- Missing, ambiguous or unsupported bindings produce a diagnostic or refusal. No fallback to guessed values.
- Prose (docs, comments, commit messages) in plain short sentences, no em dashes. This repo: one-line conventional commits, no trailers. qedsvm: one-line sentence-case commits (as in its log, e.g. `Add Outcome: ...`).
- Commit only with the user's go-ahead (repo rule). Commit steps below mean "stage and report, then commit when told".

## Review Focus

1. **Duplicate account markers.** A run where account 1 is a duplicate of account 0 changes every later input offset. Expected: the duplicate-marker guard appears in the dominating guards and the readable theorem names it; discovery does not bind fields through a duplicate header. Test: `q5_duplicate_marker_guard_is_dominating` in Task Q5.
2. **A DWARF struct whose size matches but whose members do not cover the loaded offsets.** Expected: the field label is refused (display-only), the byte-level theorem is still emitted. Test: `q4_struct_size_match_without_member_cover_is_refused` in Task Q4.
3. **Stale concept map** (the JSON's deployed SHA differs from the ELF given). Expected: `qed-discover` refuses before writing. Test: `q5_stale_concept_map_is_refused` in Task Q5.
4. **A qedsvm checkout at the wrong revision.** Expected: `qedgen lift` refuses and names the pinned revision. Test: `s2_wrong_qedsvm_rev_is_refused` in Task S2.
5. **An instruction whose only success path goes through a loop.** Expected: the instruction is listed as unsupported with reason `loop`, others still prove. Test: `q3_loop_success_path_is_unsupported` in Task Q3.

---

### Task S1: Fixture program and its committed binary pair (this repo, then copy into qedsvm)

**Files:**
- Create: `crates/qedgen/tests/fixtures/pinocchio-fixtures/label-vault/Cargo.toml`
- Create: `crates/qedgen/tests/fixtures/pinocchio-fixtures/label-vault/src/lib.rs`
- Create: `crates/qedgen/tests/fixtures/pinocchio-fixtures/label-vault/README.md`
- Modify: `crates/qedgen/tests/fixtures/pinocchio-fixtures/README.md` (one line for the new fixture)
- Create (qedsvm): `qedsvm-rs/tests/fixtures/label_vault.so`, `qedsvm-rs/tests/fixtures/label_vault.debug.so`, `qedsvm-rs/tests/fixtures/label_vault.build.json`

**Interfaces:**
- Produces: library name `label_vault`; the pair at `.qed/build/label_vault/label_vault.so` and `label_vault.debug.so`; the instruction set below, which later tasks' tests assert against.

Program shape (Pinocchio 0.9.3, `lazy_program_entrypoint!` so account parsing has no loop; `#![no_std]`, `no_allocator!`, `nostd_panic_handler!`):

| Disc | Name | Accounts | Checks | Effect |
|---|---|---|---|---|
| 0 | `initialize` | vault (w), authority (s) | `data_len == Vault::LEN` (48), `vault.initialized == 0`, authority `is_signer` | `initialized = 1`, `authority = authority.key`, `count = 0` |
| 1 | `increment(amount: u64)` | vault (w), authority (s) | len, `initialized == 1`, `is_signer`, `authority.key == vault.authority`, `checked_add` | `count += amount` |
| 2 | `reset` | vault (w), authority (s) | len, `initialized == 1`, `is_signer`, key compare | `count = 0` |

`#[repr(C)] struct Vault { initialized: u8, _pad: [u8; 7], authority: [u8; 32], count: u64 }` read by pointer cast over `vault.borrow_mut_data_unchecked()`. Errors: `ProgramError::Custom(1)` wrong authority, `Custom(2)` overflow, `InvalidInstructionData`, `InvalidAccountData`. `Cargo.toml` copies the gate program's pins (`[workspace]`, `[workspace.metadata.cli] solana = "3.0.1"`, `[profile.release] debug = 2`, `crate-type = ["cdylib", "lib"]`).

- [ ] **Step 1:** Write the program and `cargo check` it on the host.
  Run: `cargo check --manifest-path crates/qedgen/tests/fixtures/pinocchio-fixtures/label-vault/Cargo.toml`. Expected: success.
- [ ] **Step 2:** Build it with qedgen.
  Run: `cargo build --release && cp target/release/qedgen bin/qedgen && bin/qedgen build --program crates/qedgen/tests/fixtures/pinocchio-fixtures/label-vault --arch v0 --tools-version v1.57` (Docker; on Apple Silicon `DOCKER_DEFAULT_PLATFORM=linux/amd64`). Expected: success, `build.json` has `"arch": "v0"`, `"debug_setting": "2"`, `"text_matches": true`. If the Solana 3.0.1 image rejects `--arch v0`, record the error and pick the image that accepts V0 with v1.57; write the choice in the fixture README.
- [ ] **Step 3:** Prove there is no loop on any success path.
  Run (qedsvm): `cargo run --manifest-path qedsvm-rs/Cargo.toml -p qed-analysis --bin qed-concept-map -- <so> <debug.so> > /dev/null` (expected: success), then a throwaway `qed-analysis` test that builds `Cfg` and checks no strongly connected component reaches a success edge. Expected: none. If one does, change the entrypoint usage (not the analysis) and rebuild.
- [ ] **Step 4:** Copy the three files into qedsvm `qedsvm-rs/tests/fixtures/` and record their SHA-256 in the fixture README here.
- [ ] **Step 5: Commit** (both repos). This repo: `test(fixtures): add label-vault Pinocchio fixture for #450`. qedsvm: `Add the label-vault V0 debug fixture pair`.

---

### Task Q1: Generic no-return checker (qedsvm, Lean + Rust)

**Files:**
- Create: `SVM/SBPF/NoReturnCheck.lean`
- Modify: `SVM.lean` (import it)
- Create: `qedsvm-rs/qed-analysis/src/discover/mod.rs`, `qedsvm-rs/qed-analysis/src/discover/noreturn.rs`
- Modify: `qedsvm-rs/qed-analysis/src/lib.rs` (`pub mod discover;`)
- Create: `examples/lean/PToken/NoReturnGeneric.lean` (regression: the generic check on p-token)
- Modify: `lakefile.lean` (add `PToken.NoReturnGeneric` to `Examples` roots), `examples/lean/AxiomAudit.lean` (audit `noRetCheck_sound`)
- Test: `qedsvm-rs/qed-analysis/tests/discover_noreturn.rs`

**Interfaces:**
- Produces (Lean):
  ```lean
  inductive NoRetStep where
    | straight (pc len : Nat)        -- StraightRun fetch pc len
    | callLocal (pc target : Nat)    -- fetch pc = some (.call_local target)
    | panicCall (pc : Nat)           -- fetch pc = some (.call .sol_panic_) (or abort_)
  structure NoRetWitness where
    heads : List Nat                 -- helpers claimed never to return
    steps : List NoRetStep
  def noRetCheck (fetch : Nat → Option Insn) (w : NoRetWitness) : Bool
  theorem noRetCheck_sound (fetch : Nat → Option Insn) (w : NoRetWitness)
      (h : noRetCheck fetch w = true) : ∀ t ∈ w.heads, NoReturn fetch t
  ```
  `noRetCheck` must check that every head reaches a `panicCall` through `straight` and `callLocal` steps only, with each step's PC covered. Build the proof from `FailClosed.lean`'s `failsClosed_straight`, `failsClosed_enter` and the `stays*` pattern in `PToken/PanicChain.lean`, generalized over the witness.
- Produces (Rust): `pub fn noreturn_witness(image: &ProgramImage, cfg: &Cfg) -> Result<NoRetWitness, String>` in `discover/noreturn.rs`, with `pub struct NoRetWitness { pub heads: Vec<usize>, pub steps: Vec<NoRetStep> }` and `pub enum NoRetStep { Straight { pc: usize, len: usize }, CallLocal { pc: usize, target: usize }, PanicCall { pc: usize } }`; `pub fn to_lean(&self) -> String`. Heads are the members of `cfg.noreturn` called from reachable code. A head whose chain does not end in a panic call is left out, with a diagnostic, never guessed.

- [ ] **Step 1: Write the failing Rust test.**
  ```rust
  #[test] fn q1_ptoken_witness_matches_hand_chain() {
      let w = noreturn_witness(&ptoken_image(), &cfg).unwrap();
      assert_eq!(w.heads, vec![10881, 11177]);
      assert!(w.steps.contains(&NoRetStep::PanicCall { pc: 10829 }));
      assert!(w.steps.contains(&NoRetStep::Straight { pc: 11618, len: 22 }));
  }
  #[test] fn q1_label_vault_witness_nonempty_and_ends_in_panic() { /* heads non-empty, every head's chain ends in PanicCall */ }
  ```
- [ ] **Step 2:** Run `cargo test --manifest-path qedsvm-rs/Cargo.toml -p qed-analysis --test discover_noreturn`. Expected: FAIL (module missing).
- [ ] **Step 3:** Implement `noreturn_witness` and `to_lean`.
- [ ] **Step 4:** Run the test again. Expected: PASS.
- [ ] **Step 5:** Write `NoReturnCheck.lean` and `PToken/NoReturnGeneric.lean`. The latter emits the p-token witness (from `to_lean`) and proves `theorem ptNoret_generic : ∀ insns, ptokenInsns = some insns → ∀ t, (t == 11177 || t == 10881) = true → NoReturn (Runner.fetchFromArray insns) t` from `noRetCheck_sound` plus one `native_decide` on the check.
- [ ] **Step 6:** Run `lake build SVM.SBPF.NoReturnCheck PToken.NoReturnGeneric AxiomAudit`. Expected: success; `#print axioms noRetCheck_sound` shows only the standard three.
- [ ] **Step 7:** Run the p-token `--check` from Global Constraints. Expected: PASS.
- [ ] **Step 8: Commit** `Add a generic no-return checker with a soundness theorem`.

---

### Task Q2: Assumed input words in the abstract interpreter (qedsvm, Lean + Rust)

Decision 9 of the spec: data lengths are visible theorem hypotheses. Today a load whose address depends on a data length gives `top`. This task lets the initial abstract state carry assumed input words, so later offsets become constants under stated hypotheses.

**Files:**
- Modify: `SVM/SBPF/AbsInterp.lean`, `SVM/SBPF/InputGuards.lean`
- Modify: `qedsvm-rs/qed-analysis/src/success_proofs/provenance.rs`
- Test: `qedsvm-rs/qed-analysis/tests/provenance_assumed.rs`; Lean regression via existing `lake build PToken.SuccessInputs`

**Interfaces:**
- Produces (Lean): a field `assumed : List (Nat × Nat × Nat)` on `AState` (input offset, width in bytes, value), default `[]`. `absStep` on a load from `.inPtr k` with `(k, w, v) ∈ assumed` gives `.const v`. `Concrete s0 a s` gains the clause `∀ (o, w, v) ∈ a.assumed, readLE s0.mem (Memory.INPUT_START + o) w = v` (use the existing memory read used by `AVal.denote`). `afterCall` / `afterSyscall` / `afterStore` keep `assumed` only while `written` does not overlap the word (`rangeFree`). Re-prove `inv_holds` and `edge_input_conditions`.
- Produces (Rust): `AState` in `provenance.rs` mirrors the field: `pub assumed: Vec<(u64, u64, u64)>`, same transfer rules.
- p-token passes `assumed = []`, so its generated text must not change.

- [ ] **Step 1: Write the failing Rust test.**
  ```rust
  #[test] fn q2_assumed_data_len_makes_second_account_offset_constant() {
      // label_vault, entry state with assumed (0x58, 8, 48): the load of account 1's
      // is_signer byte must have provenance InputField at the constant offset
      // 8 + 96 + align8(48 + 10240) + 1.
  }
  #[test] fn q2_store_over_assumed_word_forgets_it() { /* unit test on AState transfer */ }
  ```
- [ ] **Step 2:** Run `cargo test ... --test provenance_assumed`. Expected: FAIL.
- [ ] **Step 3:** Implement the Rust mirror.
- [ ] **Step 4:** Run the test. Expected: PASS.
- [ ] **Step 5:** Implement the Lean change and fix the proofs.
- [ ] **Step 6:** Run `lake build SVM.SBPF.InputGuards PToken.SuccessInputs AxiomAudit` and the p-token `--check`. Expected: both pass, no new axioms on `inv_holds` / `edge_input_conditions`.
- [ ] **Step 7: Commit** `Let the abstract interpreter carry assumed input words`.

---

### Task Q3: Dispatch and success-edge discovery (qedsvm, Rust)

**Files:**
- Create: `qedsvm-rs/qed-analysis/src/discover/dispatch.rs`
- Test: `qedsvm-rs/qed-analysis/tests/discover_dispatch.rs`

**Interfaces:**
- Consumes: `Cfg`, `Outcomes::success_edges`, `provenance::abstract_map` (with Q2 assumptions), `ConceptMap`.
- Produces:
  ```rust
  pub struct Instruction { pub discriminator: u64, pub routes: Vec<usize> /* dispatch-target PCs */,
                           pub handler: Option<Label>, pub success_edges: Vec<usize>,
                           pub unsupported: Vec<(usize, Unsupported)> }
  pub struct Label { pub name: String, pub source: LabelSource /* ConceptMapFunction { id } */ }
  pub enum Unsupported { Loop, CallX, UnpinnedDiscriminator }
  pub fn discover_instructions(image: &ProgramImage, cfg: &Cfg, map: &ConceptMap,
                               assumed: &[(u64, u64, u64)]) -> Result<Vec<Instruction>, String>
  ```
  A dispatch guard is a `Branch` whose operand provenance is the instruction-data byte(s) after the account section (`InputField` at the instruction-data offset + 8, under `assumed`) compared with a constant. Merge every route with the same discriminator into one `Instruction` (p-token has fast paths plus match tables). A success edge belongs to an instruction when every path from entry to it passes one of that instruction's routes. Edges inside or behind an SCC are `Unsupported::Loop`. `handler` is the innermost concept-map function (not inlined) whose ranges contain the first route PC; `None` when absent or ambiguous.
- Also produces the length assumptions: `pub fn length_assumptions(image, cfg) -> Vec<(u64, u64, u64)>`, one per account in order, taken from a dominating `==` guard on that account's `data_len` word; stop at the first account without one.

- [ ] **Step 1: Write the failing tests.**
  ```rust
  #[test] fn q3_label_vault_has_three_instructions() {
      let ix = discover_instructions(..).unwrap();
      assert_eq!(ix.iter().map(|i| i.discriminator).collect::<Vec<_>>(), vec![0, 1, 2]);
      assert!(ix.iter().all(|i| !i.success_edges.is_empty()));
      assert_eq!(ix[1].handler.as_ref().map(|l| l.name.as_str()).map(|n| n.ends_with("increment")), Some(true));
  }
  #[test] fn q3_ptoken_merges_fast_path_and_table_routes() {
      // discriminator 3 (transfer) has >= 2 routes; its success edges include 4825, 4832, 5250.
  }
  #[test] fn q3_loop_success_path_is_unsupported() {
      // p-token batch (255): every success edge is Unsupported::Loop.
  }
  #[test] fn q3_length_assumptions_label_vault() { assert_eq!(length_assumptions(..)[0], (0x58, 8, 48)); }
  ```
- [ ] **Step 2:** Run `cargo test ... --test discover_dispatch`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run. Expected: PASS.
- [ ] **Step 5: Commit** `Discover instructions and success edges from the dispatcher`.

---

### Task Q4: Authority checkpoints and field labels (qedsvm, Rust)

**Files:**
- Create: `qedsvm-rs/qed-analysis/src/discover/authority.rs`, `qedsvm-rs/qed-analysis/src/discover/fields.rs`
- Test: `qedsvm-rs/qed-analysis/tests/discover_authority.rs`

**Interfaces:**
- Consumes: Q3 `Instruction`, `abstract_map`, `ConceptMap.structures`, `input_layout::aligned_input_offsets`.
- Produces:
  ```rust
  pub struct Checkpoint { pub pc: usize, pub signer_account: usize, pub key_compare: Option<KeyCompare> }
  pub struct KeyCompare { pub account: usize, pub against: InputWord /* account + data offset */ }
  pub fn discover_checkpoints(image, cfg, ix: &Instruction, edge: usize, assumed) -> Result<Vec<Checkpoint>, String>
  pub struct FieldLabel { pub account: usize, pub data_offset: u64, pub width: u64,
                          pub name: Option<String>, pub status: LabelStatus }
  pub enum LabelStatus { Labelled { structure_id: u64 }, Refused(String) }
  pub fn label_fields(map: &ConceptMap, account: usize, data_len: u64, loads: &[(u64, u64)]) -> Vec<FieldLabel>
  ```
  A checkpoint is the target-ward successor of a guard whose operand is the `is_signer` byte (account header + 1) of some account. The checkpoints of one edge must cut every path from entry to the edge (reuse the cut check in `success_proofs::generate`); if they do not, return an error. `key_compare` is set when the 4 u64 limbs of that account's key (header + 8 .. + 40) are each compared to the 4 limbs at one other input location; partial matches leave it `None`. `label_fields` picks the unique DWARF structure with `byte_size == data_len` whose members cover every `(offset, width)` load; zero or several candidates give `Refused`.

- [ ] **Step 1: Write the failing tests.**
  ```rust
  #[test] fn q4_increment_has_one_signer_checkpoint_with_key_compare() {
      // signer_account == 1, key_compare.against == (account 0, data offset 8)
  }
  #[test] fn q4_ptoken_transfer_edge_4825_checkpoints_match_manifest() {
      // discovered set == {4816, 5216, 5715, 6233} (from success-proof-inputs.json)
  }
  #[test] fn q4_vault_fields_are_labelled() { /* offsets 0,8,40 -> initialized, authority, count */ }
  #[test] fn q4_struct_size_match_without_member_cover_is_refused() { /* synthetic ConceptMap */ }
  #[test] fn q4_ambiguous_struct_is_refused() { /* two structs of size 48 that both cover */ }
  #[test] fn q4_wrong_checkpoint_candidate_does_not_cut() { /* removing one route -> Err */ }
  ```
- [ ] **Step 2:** Run `cargo test ... --test discover_authority`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run. Expected: PASS. The p-token checkpoint test is the main evidence that discovery is not fixture-shaped.
- [ ] **Step 5: Commit** `Discover authority checkpoints and label account fields`.

---

### Task Q5: Bindings file and `qed-discover` CLI (qedsvm, Rust)

**Files:**
- Create: `qedsvm-rs/qed-analysis/src/discover/bindings.rs`, `qedsvm-rs/qed-analysis/src/bin/qed-discover.rs`
- Create: `docs/BINDINGS.md`
- Test: `qedsvm-rs/qed-analysis/tests/discover_cli.rs`

**Interfaces:**
- Consumes: Q1, Q3, Q4.
- Produces:
  ```rust
  #[derive(Serialize, Deserialize)] #[serde(deny_unknown_fields)]
  pub struct Bindings { pub schema_version: u32 /* 1 */, pub trust: String /* "untrusted_discovered_bindings" */,
                        pub artifact_sha256: String, pub concept_map_sha256: String, pub entry: usize,
                        pub noreturn: NoRetWitness, pub assumed: Vec<(u64, u64, u64)>,
                        pub instructions: Vec<InstructionBinding>, pub diagnostics: Vec<Diagnostic> }
  pub struct InstructionBinding { pub discriminator: u64, pub handler: Option<Label>,
                                  pub edges: Vec<EdgeBinding>, pub unsupported: Vec<(usize, String)> }
  pub struct EdgeBinding { pub pc: usize, pub checkpoints: Vec<Checkpoint>, pub fields: Vec<FieldLabel> }
  pub fn discover(deployed: &[u8], debug: &[u8], map: &ConceptMap) -> Result<Bindings, String>
  ```
  CLI: `qed-discover <deployed.so> <debug.so> <concepts.json> > bindings.json`. Deterministic pretty JSON, sorted keys and lists. Writes nothing on error. The concept map's `artifact.deployed_sha256` must equal the ELF's SHA-256.

- [ ] **Step 1: Write the failing tests.**
  ```rust
  #[test] fn q5_label_vault_bindings_golden() { /* equals qedsvm-rs/tests/fixtures/label_vault.bindings.json */ }
  #[test] fn q5_deterministic() { /* two runs, identical bytes */ }
  #[test] fn q5_stale_concept_map_is_refused() { /* concepts from p_token, ELF label_vault -> nonzero exit, no stdout */ }
  #[test] fn q5_mismatched_pair_is_refused() { /* debug.so of another build -> error from ConceptMap */ }
  #[test] fn q5_missing_types_gives_diagnostic() { /* limited-debug fixture pair from PR #86 -> fields Refused, diagnostic present */ }
  #[test] fn q5_duplicate_marker_guard_is_dominating() { /* every label_vault edge's dominating guards include account 1's dup-marker byte */ }
  ```
- [ ] **Step 2:** Run `cargo test ... --test discover_cli`. Expected: FAIL.
- [ ] **Step 3:** Implement, then generate and commit the golden `label_vault.bindings.json` (review it by hand once).
- [ ] **Step 4:** Run. Expected: PASS.
- [ ] **Step 5:** Write `docs/BINDINGS.md`: schema, trust level, what each field means, what is a label vs. proven.
- [ ] **Step 6: Commit** `Add qed-discover: binary-derived success-proof bindings`.

---

### Task Q6: Generalize the generator and emit readable theorems (qedsvm, Rust + Lean templates)

**Files:**
- Modify: `qedsvm-rs/qed-analysis/src/success_proofs.rs`, `qedsvm-rs/qed-analysis/src/bin/qed-success-proofs.rs`
- Modify: `qedsvm-rs/qed-analysis/src/success_proofs/{paths_head,path_theorem,inputs_head,input_theorem}.lean.in` (placeholders only)
- Create: `qedsvm-rs/qed-analysis/src/success_proofs/noret_generic.lean.in`, `readable_theorem.lean.in`
- Test: `qedsvm-rs/qed-analysis/tests/success_proofs_bindings.rs`

**Interfaces:**
- Consumes: Q5 `Bindings`; Q1 `noRetCheck_sound`; Q2 `assumed`.
- Produces:
  ```rust
  pub struct ProgramNames { pub namespace: String /* "Examples.LabelVaultSuccessPaths" */,
                            pub insns_def: String /* "labelVaultInsns" */, pub module_prefix: String /* "LabelVault" */ }
  pub fn generate_from_bindings(image: &ProgramImage, b: &Bindings, names: &ProgramNames)
      -> Result<GeneratedModules, String>
  pub struct GeneratedModules { pub insns: String, pub noreturn: String, pub paths: String,
                                pub inputs: String, pub readable: String, pub coverage: Coverage }
  pub struct Coverage { pub instructions: usize, pub success_edges: usize, pub proven_edges: usize,
                        pub checked_bindings: usize, pub labels: usize, pub refused: usize,
                        pub unsupported: usize, pub native_decide_uses: usize }
  ```
  Keep `generate(image, &BaselineInputs)` as is for p-token: it must produce identical bytes. Share code by turning the hard-coded `PToken` / `ptokenInsns` / `198` / `ptNoret_sound` strings into placeholders filled from `ProgramNames` and the p-token values on the old path. The bindings path uses the generic no-return witness (`noRetCheck_sound`) instead of `ptNoret_sound`, `assumed` in the initial `AState`, and one `Route` per checkpoint. `insns` is `@[irreducible] def <insns_def> := Decode.decodeProgramV0 <Prefix>Text <Prefix>FnRegistry`.
  Readable theorem per proven edge, from `readable_theorem.lean.in`: hypotheses `hlen_<i>` for each `assumed` word (stated as byte reads of `s0`), conclusion a conjunction over the edge's pinned input guards, each written with named abbreviations `acct<i>_<field>` when the label is `Labelled`, and raw offsets with a `-- label refused: <reason>` comment otherwise. Proof: from `inputs<edge>` by `List.mem` simp. Display-only items never enter the conclusion.
- CLI: `qed-success-proofs --bindings <bindings.json> --names <Prefix> <ELF> <out-dir> [--check]`; old positional form unchanged.

- [ ] **Step 1: Write the failing tests.**
  ```rust
  #[test] fn q6_ptoken_baseline_still_byte_identical() { /* existing --check path */ }
  #[test] fn q6_label_vault_modules_golden() { /* compare to examples/lean/LabelVault/*.lean */ }
  #[test] fn q6_refused_label_not_in_conclusion() { /* bindings with a Refused field: offset appears raw, name absent */ }
  #[test] fn q6_coverage_counts_label_vault() { /* instructions == 3, proven_edges >= 1, refused/unsupported as golden */ }
  #[test] fn q6_bindings_for_other_elf_refused() { /* artifact_sha256 mismatch -> Err, no files */ }
  ```
- [ ] **Step 2:** Run `cargo test ... --test success_proofs_bindings`. Expected: FAIL.
- [ ] **Step 3:** Implement. Write the generated modules to `examples/lean/LabelVault/`.
- [ ] **Step 4:** Run. Expected: PASS.
- [ ] **Step 5: Commit** `Generate success proofs and readable theorems from discovered bindings`.

---

### Task Q7: Fixture text embedding, Lake targets and qedsvm CI (qedsvm)

**Files:**
- Modify: `qedsvm-rs/qedlift/src/input.rs`, `lift.rs` (a `--text-only <Prefix>` mode that writes only the shared-text module, reusing the `--shared-text` renderer)
- Create (generated): `examples/lean/Generated/LabelVaultText.lean`, `examples/lean/LabelVault/{Insns,NoReturn,SuccessPaths,SuccessInputs,Readable}.lean`
- Modify: `lakefile.lean` (`Examples` roots), `examples/lean/AxiomAudit.lean` (each `LabelVault` readable theorem, allowing the `native_decide` pattern), `.github/workflows/*.yml` (the job that runs the p-token `--check` also runs `qed-concept-map`, `qed-discover --check-golden` and `qed-success-proofs --bindings ... --check` for label_vault), `docs/SUCCESS_PROOFS.md` (new section)
- Test: `qedsvm-rs/qedlift/tests/text_only.rs`

**Interfaces:**
- Consumes: Q6 output; S1 pair.
- Produces: `lake build LabelVault.Readable` succeeds; the one-line regeneration command documented in `docs/SUCCESS_PROOFS.md`.

- [ ] **Step 1: Write the failing test** `q7_text_only_emits_registry_and_bytes` (output contains `def LabelVaultText : ByteArray` and `LabelVaultFnRegistry`, no path theorem).
- [ ] **Step 2:** Run `cargo test --manifest-path qedsvm-rs/Cargo.toml -p qedlift --test text_only`. Expected: FAIL.
- [ ] **Step 3:** Implement `--text-only`; generate all LabelVault modules.
- [ ] **Step 4:** Run the test (PASS), then `lake build LabelVault.Readable AxiomAudit`. Expected: success, no `sorry`; axioms limited to the gate set.
- [ ] **Step 5:** Run every regeneration step with `--check`. Expected: no drift.
- [ ] **Step 6: Commit** `Check label-vault success proofs end to end`. Open the qedsvm PR, link it on #450, and record the merge commit SHA for Task S2's pin.

---

### Task S2: `qedgen lift` command (this repo)

**Files:**
- Create: `crates/qedgen/src/lift/mod.rs` (orchestration), `crates/qedgen/src/lift/qedsvm.rs` (checkout + tool calls), `crates/qedgen/src/lift/report.rs` (report + axiom gate)
- Modify: `crates/qedgen/src/cli.rs` (`Lift` variant), `crates/qedgen/src/run.rs` (`command_name_of`, dispatch), `crates/qedgen/src/main.rs` (module re-export)
- Modify: `references/cli.md` (the new verb and every flag), `docs/framework-support.md` (Pinocchio row: binary lift, V0)
- Test: `crates/qedgen/tests/lift_cli.rs`

**Interfaces:**
- Consumes: `verifiable_build::{run_build, build_dir, load_record, BuildOptions}`; qedsvm tools from Q5 to Q7.
- Produces:
  ```rust
  pub const QEDSVM_LIFT_REV: &str = "<merge SHA from Q7>";
  pub struct LiftOptions { pub program: PathBuf, pub library: Option<String>, pub qedsvm: Option<PathBuf>,
                           pub skip_build: bool, pub json: bool }
  pub fn run_lift(opts: &LiftOptions) -> Result<LiftReport>
  #[derive(Serialize)] pub struct LiftReport { pub schema_version: u32, pub library: String, pub program_hash: String,
      pub qedsvm_rev: String, pub coverage: Coverage /* same fields as Q6 */, pub instructions: Vec<InstructionReport>,
      pub axioms: AxiomSummary, pub gaps: Vec<String> /* always includes the native_decide gap */ }
  pub fn axiom_allowed(name: &str) -> bool   // standard three or contains "._native.native_decide.ax_"
  ```
  CLI: `qedgen lift --program <dir> [--library <name>] [--qedsvm <checkout>] [--skip-build] [--json]`. Without `--skip-build` it runs `qedgen build --arch v0 --tools-version v1.57` first. Without `--qedsvm` it clones `https://github.com/QEDGen/qedsvm.git` at `QEDSVM_LIFT_REV` into the validation workspace (`QEDGEN_VALIDATION_WORKSPACE`, else the platform cache dir) under `qedsvm/<rev>`. A given checkout must have `HEAD == QEDSVM_LIFT_REV` and a clean tree. Steps: `qed-concept-map`, `qed-discover`, `qedlift --text-only`, `qed-success-proofs --bindings`, write modules to `<checkout>/examples/lean/Lift/<Prefix>/`, `lake build` them plus an emitted `Audit.lean` that runs `#print axioms` on every readable theorem, parse the axioms, apply `axiom_allowed`. Writes `.qed/lift/<library>/report.json`, `report.txt`, `bindings.json`, and copies of the generated `.lean` files. Exit nonzero on any build failure or disallowed axiom. Never touches `.qed/verify-evidence.json`.

- [ ] **Step 1: Write the failing tests.**
  ```rust
  #[test] fn s2_axiom_allowed() {
      assert!(axiom_allowed("propext"));
      assert!(axiom_allowed("Examples.LabelVaultSuccessPaths.check42._native.native_decide.ax_1_1"));
      assert!(!axiom_allowed("sorryAx"));
      assert!(!axiom_allowed("Lean.ofReduceBool"));
  }
  #[test] fn s2_wrong_qedsvm_rev_is_refused() { /* temp git repo at another commit -> Err naming QEDSVM_LIFT_REV */ }
  #[test] fn s2_dirty_checkout_is_refused() {}
  #[test] fn s2_missing_build_with_skip_build_is_refused() { /* no .qed/build/<lib> -> Err suggesting qedgen build */ }
  #[test] fn s2_report_lists_native_decide_gap() { /* LiftReport::new(...).gaps contains "native_decide (qedsvm#98)" */ }
  #[test] fn s2_lift_never_writes_verify_evidence() { /* after a stubbed run, .qed/verify-evidence.json absent */ }
  ```
  Stub the qedsvm tool calls behind a `trait QedsvmTools` so unit tests run without qedsvm or Lean.
- [ ] **Step 2:** Run `cargo test -p qedgen-solana-skills --test lift_cli`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run. Expected: PASS. Then `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`. Expected: clean.
- [ ] **Step 5:** Update `references/cli.md` and `docs/framework-support.md`.
- [ ] **Step 6: Commit** `feat(lift): add qedgen lift for binary-derived success proofs (#450)`.

---

### Task S3: End-to-end gate and CI job (this repo)

**Files:**
- Create: `crates/qedgen/tests/label_lift_gate.rs` (`#[ignore]`)
- Create: `.github/workflows/label-lift.yml`
- Modify: `crates/qedgen/tests/fixtures/pinocchio-fixtures/label-vault/README.md` (the one documented clean-checkout command)

**Interfaces:**
- Consumes: S1 fixture, S2 `qedgen lift`, qedsvm at `QEDSVM_LIFT_REV`.

- [ ] **Step 1: Write the gate tests.**
  ```rust
  #[test] #[ignore] fn s3_rebuild_matches_committed_pair() {
      // qedgen build --arch v0 --tools-version v1.57 on the fixture; sha256 of both outputs
      // equals the hashes in the fixture README (and the qedsvm copies).
  }
  #[test] #[ignore] fn s3_lift_end_to_end() {
      // qedgen lift --program <fixture> --json; exit 0; report.coverage.instructions == 3;
      // proven_edges >= 1 for discriminator 1; axioms all allowed; gaps lists native_decide.
  }
  ```
- [ ] **Step 2:** Run `cargo test -p qedgen-solana-skills --test label_lift_gate -- --ignored --nocapture --test-threads=1`. Expected: PASS (needs Docker, elan, about 30+ minutes on a cold Lean cache).
- [ ] **Step 3:** Write the workflow: same Docker setup and `SOLANA_VERIFY_VERSION` as `verifiable-build.yml`, plus elan with the qedsvm `lean-toolchain`, cache `~/.cache/qedgen`. Triggers: `workflow_dispatch`, push to `main`, PRs touching `crates/qedgen/src/lift/**`, `crates/qedgen/src/verifiable_build/**` or the fixture.
- [ ] **Step 4:** Write the clean-checkout command in the fixture README: `cargo build --release && bin/qedgen lift --program crates/qedgen/tests/fixtures/pinocchio-fixtures/label-vault`.
- [ ] **Step 5: Commit** `test(lift): gate the label-vault lift end to end`.
- [ ] **Step 6:** Draft (do not file) the follow-up issue for qedsvm: "Port the success-proof stack to sBPF v3". Show it to the user to file. Post the coverage report on #450 only after the user agrees.

---

## Self-review notes

- Spec coverage: pipeline steps 1 to 6 map to S1, Q1 to Q7, S2. Negative tests: Q4, Q5, Q6, S2. Determinism: Q5, Q7. Generality anti-goal: the p-token checkpoint test (Q4) and dispatch-merge test (Q3) run discovery on a second program with no code change. Full-debug measurement already on #450.
- Deviation from the spec, stated: the spec puts the fixture in this repo's `tests/fixtures/`. The source lives there, but the committed pair and generated Lean also live in qedsvm, because the Lean check and its determinism CI run there.
- Deviation from the spec, stated: decision 9 said "no new soundness work". Q2 is a small soundness extension of `AbsInterp` (assumed words), not guard sensitivity. Discharging the `hlen` hypotheses from the dominating length guards is left out: the pinning of later guards depends on the earlier assumptions, so a discharge needs an ordered induction. The hypotheses stay visible in each theorem. Do this as a follow-up.
