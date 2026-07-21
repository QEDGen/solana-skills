# QEDGen v2.47.0 — generated artifacts now run: the executable gate, runtime journeys, and the codegen bugs they caught

**Status:** released. **Scope:** 17 merged PRs since v2.46.0 (#263, #300–#303, #304, #306, #309–#310, #315–#316, #318–#322) plus one direct hardening commit.
**Theme:** closing #294. Snapshot suites prove generated text is stable; users consume it as compiled and executed software. This release adds the gates that compile and run every generated artifact, plus a runtime journey that drives generated constraints through real transactions — and fixes the eight codegen defect classes those gates caught on their first runs. It also lands three structural refactors so the fixed bug classes cannot be reconstructed.

## 1. Executable generated-artifact gate (#294 P0, #300)

New CI job `generated-artifacts` (`tests/generated_artifact_gate.rs`). For each bundled Anchor example (escrow, lending, multisig), from a clean tempdir:

1. run `qedgen codegen --all`;
2. assert every expected Rust artifact exists (silent-skip guard);
3. compile the scaffold and every test target, and run the generated unit tests and proptests (`cargo test --no-fail-fast`);
4. type-check the generated Kani harness with ordinary rustc against the new `crates/kani-compile-stub` crate (the harness is `#![cfg(kani)]`, so step 3 alone compiles it to nothing).

The gate's first full run caught four latent defect classes (§3). The old Anchor scaffold/proptest smokes in `codegen_smoke.rs` are superseded and removed.

## 2. Runtime journeys — constraints that are wrong only at runtime (#294 P0, #309, #310)

PDA seeds, bump, payer, space, and `init` constraints compile cleanly and are invisible to every compile-only gate. The new journey (in `qedgen-sandbox`, which owns the Mollusk + Agave dependency) regenerates the fixture program from its committed spec, overlays the agent-filled handler bodies, builds it to SBF bytecode, and drives real transactions through Mollusk:

- **Signer/init journey (#309):** asserts the success path (program ownership, exact space, stored canonical bump, lifecycle `Uninitialized -> Active`) and four failure paths: wrong seeds, non-canonical bump, unsigned payer, re-initialization.
- **PDA-authority SPL Token CPI (#310):** a real vault, both CPI directions — deposit with the owner as token authority (ordinary signed CPI), withdraw with the vault PDA as authority, so the program signs with the vault's seeds and bump. This succeeds only when those seeds agree with the generated `#[account(seeds = …, bump)]` constraint. Token program ELF comes from `mollusk-svm-programs-token`; no vendored `.so`.

The journey found a real bug on its first run: codegen emitted `has_one = <auth>` on `init` accounts. Anchor allocates and zeroes the account before evaluating constraints, so the check compared an all-zero field against the payer and always failed with `ConstraintHasOne` — every generated Anchor init handler whose `auth` matched a state field could never be opened. `has_one` is now suppressed on init (#309), later encoded structurally (§4).

## 3. Codegen defects the gates caught — fixed with regression tests

- **Generated unit tests were dead code (#299 → #300):** the default `--test-output` was `./programs/src/tests.rs`, but no scaffold `lib.rs` ever emitted a `mod tests;` hook — the generated unit tests never compiled or ran, in any project. New default: `./programs/tests/unit.rs`, where cargo auto-discovers the target. Legacy `src/tests.rs` files are still recognized by regen-drift.
- **Account-valued effects leaked into the account-less unit-test model (#297 → #301):** effect RHS account reads rendered verbatim into `apply_*` (E0425). Effects that are account-valued (Pubkey-typed destination, or RHS reads an account binding) are now structurally suppressed, matching the existing pubkey-skip and guard-suppression contracts.
- **Three proptest emitter defects on multisig-shaped specs (#295 → #302):** bare account names surviving guard suppression (E0425), Pubkey handler params rendered as numeric ranges (syntax error), and a u8-subscript defect shared with the Kani conformance emitter.
- **Proptest model wrapped checked-default effects (#296 → #303):** the transition emitter forced default `+=`/`-=` to wrap and report success, so every `<op>_no_overflow_on_<field>` test failed on correct specs. Default effects now model the deployed `checked_add(..).ok_or(err)?`, matching the Kani lane; explicit `+=!`/`+=?` tiers keep their declared semantics.
- **Model-only out-of-bounds subscripts (#298 → #304):** the model state space is wider than any deployed state, so a bounded-container subscript could panic the proptest harness or spuriously fail the Kani proof. Synthesized bounds conjuncts now lead the collected guard; deployed code aborts, the model rejects the transition.
- **Anchor init space target and missing `InitSpace` derive (#305 → #306):** the `space = 8 + <T>::INIT_SPACE` attribute and the state-struct emission were derived at separate sites that could disagree (E0433), and the flat single-account branch omitted `#[derive(InitSpace)]` (E0599). Both sites now share `codegen_shared::state_struct_name`.
- **Guard tests asserted over unverified fixtures (#312 → #315):** the fixture solver could return an assignment that left the guard true, producing generated tests that fail against correct code (six across the bundled examples). Every fixture is now evaluated against the exact guard before an assertion is emitted — Solved asserts, Contradicted and Unsupported emit the fixture with an explanation instead. Result: lending 22/22 and multisig 51/51 generated unit tests pass; the artifact gate went fully green for the first time.
- **Five v2.45.0 codegen-quality gaps from vault-spec dogfooding (#263):** bool fixtures emitting `0` (E0308); chained comparisons in Kani conformance asserts (parenthesization); **PDA-signer seeds on vault-authority CPIs** — `Token.transfer(authority = <PDA>, …)` now assembles the account's declared `pda [...]` seeds and emits `CpiContext::new_with_signer` (proven by the #310 journey); a bounded constraint-propagation solver for compound cross-field guard fixtures; and init/payer/space gated on the resolved state account rather than a name heuristic.

## 4. Structural refactors — bug classes made unconstructible

- **One canonical account plan (#311 → #316):** account attributes were assembled from facts recomputed at each point of use; #305 and #307 were exactly two such sites disagreeing. `AccountPlan::derive` is now the only place those facts are decided, and `render_account_attr` is a pure projection. The three scattered `is_init` rules are encoded as reachability in `AccountLifecycle` — `has_one` exists only on Existing, `token_authority` only on Init, `mut` only on Existing — so the illegal combinations cannot be constructed.
- **CPI mechanization contract (#314 → #318):** a typed complete-vs-agent-fill boundary (`CpiPlan`). The PDA-signer rule is a post-condition on the emitted artifact: a call whose signer slots bind caller PDAs is Complete only if the emitted code signs for them (`new_with_signer`/`invoke_signed`); every unsigned path resolves to an explicit agent-fill site. Pinocchio lifecycle creation, events, and transfers are hard fill sites. The per-target ownership matrix is documented in `docs/framework-support.md`.
- **Canonical seed plan (#317 → #319):** one `SeedPlan` (none, macro, runtime) consumed by both the R28 runtime PDA check and `InvalidPda`; the duplicated suppression predicate is removed, and init state-field seeds stay macro-enforced without a redundant runtime syscall.
- **Typed expression composition (#313 → #320, #321):** effect LHS paths carry name-resolved, typed MIR paths from the parser adapter through every emitter; boolean/guard composition and proptest strategy expressions are precedence-aware typed syntax before rendering. The duplicated unit-test subscript rewrite is removed; state-field/parameter name collisions are covered.

## 5. Smaller fixes

- **Regen-drift paths free of `CurDir` segments (#293 → #322):** root-layout scans stored candidates as `./Cargo.toml`, so `--write` reported `<example>/./Cargo.toml`. Comparable paths now assert no `.` component.
- **Hardened user-owned recovery paths:** preflight tightening on the `--force`/`--merge-accounts` recovery lane shipped in v2.46.0.

## Compatibility

- Generated unit tests move from `./programs/src/tests.rs` to `./programs/tests/unit.rs` by default. Existing projects with the legacy path keep working: regen-drift recognizes both locations. Regenerate with `--all` (or `--test`) to migrate.
- Regenerated Anchor scaffolds no longer emit `has_one` on `init` accounts and now sign PDA-authority builder CPIs with `new_with_signer` when seeds come from the account's declared `pda [...]`. Both change generated output; `qedgen check --regen-drift --write` picks them up.
- No spec-language changes. No new CLI flags; only the `--test-output` default moved.
