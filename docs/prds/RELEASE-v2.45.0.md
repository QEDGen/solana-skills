# QEDGen v2.45.0 — effect-semantics soundness and the IDL-complete skeleton

**Status:** released. **Scope:** 4 merged PRs since v2.44.0.
**Theme:** dogfooding v2.43/v2.44 on real vault and brownfield Anchor targets
surfaced a read-after-write soundness hole and a set of codegen miscompiles;
all are fixed at the emitter, and the scaffold-to-spec skeleton now fills in
everything the IDL already knows.

## 1. Read-after-write effect soundness — parallel semantics everywhere (#245)

For `effect { balance += amount, last_seen := balance }`, v2.43's artifacts
modeled two different programs: the Lean model and Kani conformance assertion
read `balance` at pre-state (parallel semantics), while the generated handler
body and the Kani/proptest transition fns read the post-add value
(sequential). All green except Kani's conformance harness — a proof about a
different program could reach `#[qed(verified)]`.

**Parallel is canonical** — it is what the proofs were already about, and the
atomic-transition reading is the spec-language convention. Every sequential
emitter now snapshots fields the block both writes and reads
(`let pre_<field> = …;`) and routes RHS reads through the snapshot:

- Kani + proptest transition fns (shared emitter),
- generated handler bodies — flat `mechanize_effect` and the destructured
  multi-variant ADT path,
- unit-test apply helpers and effect assertions.

Snapshots are emitted only when referenced, so specs without read-after-write
generate byte-identical output. `references/qedspec-dsl.md` documents the
semantics ("Effect semantics: parallel (pre-state) reads") with the
handler-`let` pattern for sequential dataflow.

Guardrail: the new **`duplicate_effect_target`** P1 lint rejects two writes
to one target in a single effect block (ill-defined under parallel
semantics); mutually-exclusive `match` arms stay legal.

Also in #245, three unit-test generator fixes: `s.<field>` binder leak in
apply helpers (E0425), bool `requires` atoms now pin guard fixtures (a
`requires seat_open == true` guard-accepts test no longer fails on correct
code), and `+`-sum clauses participate in the fixture raise fixpoint.

## 2. Codegen triage: wrong-target artifacts, missing dev-dep, account-key miscompile (#244)

- **Integration tests are Quasar-only**: `integration_test::generate` is
  QuasarSVM-shaped end-to-end; on Anchor/Pinocchio, `--integration`/`--all`
  now skips with a note instead of writing a non-compiling
  `tests/integration_tests.rs`.
- **`[dev-dependencies] proptest = "1"`** is emitted (and merge-upserted)
  in every generated Cargo.toml — `cargo test` works out of the box.
- **`field := <signer_account>` lowers to the runtime key load**
  (`self.vault.admin = self.new_admin.key();` on Anchor; Quasar/Pinocchio
  forms per target) instead of a bare account name (E0425/E0308). Guarded:
  set-only and Pubkey-destination-only; anything else stays on the
  `todo!()` fill path, and the new **`effect_type_mismatch`** lint surfaces
  an address-into-scalar assignment at check time.

## 3. Scaffold-to-spec skeleton: accounts{} + State from the IDL (#259 — issues #257, #258)

The guiding principle: anything mechanically derivable from the IDL /
`#[derive(Accounts)]` / account structs should not be a `TODO`.

- **`accounts { }` filled per handler** from its `#[derive(Accounts)]`
  struct: `Signer` → `signer`, `#[account(mut)]` → `writable`,
  `Program`/`Sysvar` → `program`, read-only `Account<T>` → `type T`; the
  declared signer surfaces as an `auth` hint comment. On a 39-handler
  brownfield Anchor program: 0 → 39/39 real accounts blocks.
- **`type State` seeded from an `#[account]` status enum**: the flat
  `Init | Active` placeholder becomes the real lifecycle (e.g. the 7-state
  proposal machine), `Init` retained as the pre-existence placeholder.
  Variants only — transition edges still need the impl, so `check` emits
  the expected `lifecycle_unreachable_state` hints until wired.
- Deferred: PDA seed rendering (a mut PDA renders `writable`); multi-state-
  machine programs seed from the richest enum only.

## 4. Warning-clean bundled examples (#247)

Every bundled example builds warning-free; the `no_properties` CPI example
refined. Keeps `qedgen check --frozen` release-gate baselines clean
(`multisig`'s one intentional P2 remains the sole exception).

## Compatibility

- Specs writing one field twice in a single effect block now fail
  `check` (P1 `duplicate_effect_target`) and codegen refuses them —
  previously ill-defined (Lean last-write vs Rust accumulate), so any
  affected spec was already miscompiled; split the writes into `match`
  arms or combine them.
- Generated Cargo.tomls gain `[dev-dependencies]`; regen diffs are
  insertions only.
- Read-after-write specs regenerate with `pre_<field>` snapshots — the
  Rust now matches what the Lean model always meant.
