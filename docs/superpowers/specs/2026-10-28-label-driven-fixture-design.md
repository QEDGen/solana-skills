# Label-driven proof generation on a small Pinocchio fixture

Issue: [#450](https://github.com/QEDGen/solana-skills/issues/450). Depends on (all closed): #449 (`qedgen build`), QEDGen/qedsvm#81 (concept map, PR #86), QEDGen/qedsvm#82 (committed generators, PR #88). Downstream: QEDGen/qedsvm#87 (p-token migration).

## Intent

`qedgen build` produces a binary with enough information for qedsvm and its analysis tools to verify it. This design makes that true end to end for one framework, Pinocchio, before we go broad.

Today the p-token success proofs are generated from a manifest (`examples/lean/PToken/success-proof-inputs.json` in qedsvm) that a person wrote by hand. It names the success edges, the authority checkpoints, a guard anchor, the no-return helpers and the readable field offsets. This design replaces that manifest with bindings that the tools discover from the binary and its debug info. Proofs are produced from those bindings.

The target user is brownfield. A program may have no `.qedspec` and no IDL. The binary pair from one build is the only input that is always present, and the deployed code is what runs. So everything starts from the binary.

## Decisions (made with the user, 2026-10-28)

1. **Pinocchio first.** It has the same shape as p-token, so the result carries over to qedsvm#87. Account data is read in place, so DWARF member offsets map to input bytes through pointer arithmetic, not through a deserializer. Anchor comes later.
2. **No `.qedspec` dependency.** Brownfield programs often have none.
3. **No IDL dependency.** An IDL (for Pinocchio, through Codama) is a cross-check to add later. A generated client is the same: at best it confirms the list, it never defines it.
4. **Scope is every instruction the program exposes.** The user does not pick handlers. The instruction set comes from the dispatcher in the binary (the branch on the instruction discriminator). Each dispatch target is matched to a handler through the concept-map function ranges.
5. **`native_decide` stays for now.** The generated checks keep using it, as PR #88 does. This is a known gap against the #450 acceptance list ("kernel-checked `decide` or `rfl`"). QEDGen/qedsvm#98 tracks the fix. #450 does not block on it. The report must state the gap.
6. **Repository split, as the issue says.** Generic analysis and emitter changes go in qedsvm (`qed-analysis`). Build orchestration, the fixture, the driver command and the CI gates go in this repository.
7. **The fixture is sBPF V0 for this slice.** The whole success-proof stack is V0-only today (`success_proofs.rs` rejects other versions; the Lean frame model and the qedlift embedding are V0-shaped). V0 is the legacy deploy target, and p-token is V0 because it predates v3. sBPF v3 is the standard for all new program deploys. So the v3 port of the success-proof stack is the next priority after this slice, filed as its own qedsvm issue. The fixture build pins `--arch v0` explicitly. It must not change the `qedgen build` default (v3).
8. **No-return evidence comes from a generic checker.** A new no-return checker in qedsvm, with a soundness theorem, replaces the hand-written p-token panic proofs (`PanicChain.lean`) for this purpose. It must work on any program, not only the fixture. p-token can use it later (qedsvm#87). Treating no helper as no-return is allowed only as an interim step while the checker is built, and the report must say so.
9. **Account data lengths are theorem hypotheses.** The offsets of later accounts and of instruction data (including the discriminator) depend on earlier data lengths. Each generated theorem states the lengths it relies on as visible hypotheses, discharged from the dominating length checks in the program. This needs a new proof template. It does not make the Lean abstract interpreter follow guard outcomes; that would be new soundness work and is out of scope.

## Goals

- One documented clean-checkout command builds the fixture, extracts the concept map, discovers bindings, generates Lean and checks it.
- No per-fixture hand-entered PCs, success edges, authority checkpoints or field offsets.
- Every binding has an explicit status: **checked**, **display-only** or **refused (with a reason)**. A readable statement is generated only from checked bindings.
- A report: the instructions, handlers, fields and errors recovered; the bindings checked; the claim coverage; the unsupported cases; the `native_decide` gap.
- CI checks that regeneration is deterministic.
- Negative tests: mismatched artifacts, wrong branch or checkpoint candidates, wrong field bindings, and missing or ambiguous type information. None of these can produce a silently accepted claim.

## Non-goals

- Anchor, native, Quasar. Later.
- IDL or Codama ingestion. Later, as a cross-check.
- Removing `native_decide` (qedsvm#98).
- Loops. Any success path that goes through a loop is reported as unsupported.
- Failure-side proofs (error conversion, panic chains). The first slice is success paths and their authority routes, as for p-token. The fixture still has an error path, and the report must list it as recovered but not proven.
- Migrating p-token (qedsvm#87).
- Whole-program completeness and general serialization recovery.

## Anti-goals

These would make the work a failure even if it runs:

- **Invented semantics.** A label from DWARF is a proposal. A name such as `authority` becomes part of a readable statement only when the bytecode analysis has checked the binding to input bytes. Otherwise it is display-only, and the output says so.
- **Silent fallback.** A missing, ambiguous or unsupported binding produces a diagnostic or a refusal. It never falls back to a guess or a hand-entered value.
- **A fixture-shaped tool.** The discovery must not encode the fixture's PCs, names or layout. Test this: a second small Pinocchio program (or a variation of the fixture) must go through the same command with no code change.
- **Labels from a different build.** Already enforced by `qedgen build` and by the concept map's `.text` equality check. Do not add a path around it.

## The pipeline

```
qedgen build            (exists)  -> deploy .so + unstripped .so + build record
qed-concept-map         (exists)  -> concepts.json (untrusted labels)
binding discovery       (new, qedsvm) -> bindings.json (checked / display-only / refused)
generators              (exists, generalize, qedsvm) -> Lean modules
lake build + axiom gate (exists)
report                  (new, here)
```

One qedgen command runs all of it.

### 1. Build (exists)

`qedgen build` with `debug = 2`, pinned platform-tools v1.57 and `--arch v0` (decision 7). It keeps the stripped and unstripped binaries from the same build and records evidence. Measured: on Pinocchio, `debug = 2` does not change the deployed bytes (see the #450 issue comment of 2026-10-26).

### 2. Concept map (exists)

`ConceptMap::from_elf_bytes(deployed, debug)` from qedsvm PR #86. Function ranges, inline chains, lines, structs, members, enums. Schema version 1, `docs/CONCEPT_MAP.md` in qedsvm.

### 3. Binding discovery (new, in qedsvm `qed-analysis`)

Input: the deployed ELF and the concept map. Output: a versioned bindings file that can replace `BaselineInputs` (the struct that PR #88's generators read today). Each binding carries its status and its evidence (PCs, the check that passed or the reason for refusal).

What it must discover:

- **Entry and no-return helpers.** From the ELF and the CFG. DWARF names (panic and abort helpers) are hints only. The CFG fact decides.
- **The instruction set.** The dispatcher: the branch or branches on the instruction discriminator byte(s) read from instruction data. Each discriminator value maps to a dispatch target. A program can have more than one dispatch layer, and one instruction can be reached by more than one route. Discovery merges the routes per discriminator; it does not count each route as a separate instruction. p-token (checked 2026-10-28, `pinocchio/program/src/entrypoint.rs` in `solana-program/token`) has three layers: fast paths in `entrypoint` for `transfer` (3) and `transferChecked` (12) that check the raw input (account count, data lengths, duplicate markers, then the discriminator at a computed offset); `process_instruction`, which splits `batch` (255) from the rest; and two match tables, `inner_process_instruction` and `inner_process_remaining_instruction`. `batch` calls back into the match tables in a loop. The fixture does not need all of this, but the discovery design must not assume one dispatcher.
- **Handlers.** Each dispatch target is matched to a function in the concept map (for example `process_increment`). The match is a label. The dispatch target is the fact.
- **Success edges per instruction.** Paths from the dispatch target to the program exit with a success return. Loop paths are unsupported.
- **Authority checkpoints.** A branch on an account's `is_signer` byte in the input region, together with a key comparison against another account's key or a stored field. Both parts are found in the bytecode. A name from DWARF may label the account or field, but only after the byte binding is checked.
- **Readable fields.** A DWARF struct (for example the account state struct) is bound to an account's data region when dataflow shows the handler reads that region through a pointer cast to the struct. Each member then maps to a byte offset in the input. A Rust member offset alone does not establish the binding (the issue says this).
- **Input layout.** `input_layout.rs` already computes account offsets from account data lengths, as supplied assumptions. Discovery must get those lengths from the program's own length checks, or refuse.

### 4. Generators (exist, generalize)

PR #88's `qed-success-proofs` reads `BaselineInputs` and emits `SuccessPaths`, `SuccessInputs` and `ReadableFields`. The guard and pin parts look general. The readable statements for p-token (`TransferSpec.lean`) are written by hand.

For the fixture, readable statements are **generated** from checked bindings. Example shape: "`increment` succeeds only if account 1 signed and its key equals the `authority` field of account 0." Display-only bindings appear in comments, never in a theorem.

The Lean text embedding of the program (`Generated/<Name>Text.lean`, from `qedlift`) is also needed for the fixture.

### 5. Lean check (exists)

`lake build` on the generated modules, the no-sorry check, and the axiom gate. On the pinned Lean v4.30.0, `native_decide` does not add `Lean.ofReduceBool`. It adds one axiom per use, named `<decl>._native.native_decide.ax_<i>_<j>`. The gate allows the standard three axioms (`propext`, `Classical.choice`, `Quot.sound`) plus that name pattern, and nothing else. The report counts the `native_decide` axioms.

*My call:* the Lean check runs in a pinned qedsvm checkout. qedgen calls qedsvm's binaries (`qed-concept-map`, the discovery tool, `qed-success-proofs`, `qedlift`) and runs `lake build` there. qedgen does not take a Rust dependency on `qed-analysis`. This follows the rule to dispatch to upstream tools. It also means a qedgen PR does not wait for a qedsvm release and a `lean_solana` pin bump (`lean_solana` pins qedsvm v0.14.0, which lacks `SVM/SBPF/InputGuards.lean`).

### 6. Driver and report (new, here)

A qedgen command runs steps 1 to 5 and writes the report. The report is plain text for a person and JSON for an agent, as other qedgen reports are.

*My call:* the command writes its own report. It does not write `.qed/verify-evidence.json`, so it does not feed `stamp` and does not run into `verify --strict`, which rejects `native_decide` axioms since #444. Linking this lane to stamp evidence waits for qedsvm#98.

### What "checked" means

*My call:* a binding is **checked** only when a Lean theorem proves its byte-level relation. This follows qedsvm's rule that no Rust result is trusted. The Rust discovery proposes; Lean decides. Names from DWARF, and the association of a dispatch target with a handler function, are labels that the generator attaches. They are shown, but no theorem depends on them. A binding that Rust found but Lean does not prove is display-only.

## The fixture

A small Pinocchio program in this repository's `tests/fixtures/`. It must have:

- about three instructions, with distinct paths
- at least one authority check (signer plus key compare against a stored field)
- an account state struct with a few fields
- at least one error path
- no loop on any success path

## Testing and CI

- **Coverage bar** (*my call*). Every discovered instruction ends with a status: proven, or refused with a reason. At least the instruction with the authority check must end with a proven success path and a generated readable theorem. CI pins the exact coverage counts (instructions, success edges, checked bindings, refusals), so a change in coverage is a visible diff.
- **Default CI:** the built artifact pair is committed. CI runs discovery, generation and the Lean check from the committed pair and fails on any drift (`--check` style, as in PR #88).
- **Docker job, off by default** (like the existing Verifiable build gate): rebuilds the pair with `qedgen build` and checks the bytes match the committed pair.
- **Negative tests:** listed under Goals. Each must end in a refusal or a diagnostic, never in a generated claim.
- **Generality test:** see the anti-goal "a fixture-shaped tool".

## Left to the builder

- **The name of the qedgen command.** My call: a new verb (working name `qedgen lift`), not a `verify` flag, because `verify` is built around a spec. The builder may choose a better name.
- **The bindings file schema**, and whether it extends `BaselineInputs` or replaces it. It must stay versioned and reject unknown fields, as `BaselineInputs` does.
- **The Pinocchio entrypoint style for the fixture.** Pinocchio's default entrypoint walks the accounts in a loop. Check how p-token avoids a loop on its success paths and do the same, or pick an entrypoint form with no loop.
- **How PRs are cut across the two repositories.** Link the implementation PRs from both repositories on #450.
- **The report layout.**

## Open after this slice

- **Port the success-proof stack to sBPF v3** (qedsvm; file as its own issue). This is the first follow-up, because v3 is the standard for all new deploys.
- An IDL or Codama cross-check of the discovered instruction set. p-token has one: `idl.json` at the root of `solana-program/token` (Codama 1.0.0, 28 instructions including p-token's `withdrawExcessLamports` 38, `unwrapLamports` 45 and `batch` 255).
- Anchor (needs dataflow through Borsh deserialization).
- `native_decide` removal (qedsvm#98).
- Failure-side proofs for the fixture.
- p-token migration (qedsvm#87).
