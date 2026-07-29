# QEDGen v2.49.0: the generated artifact has to compile

**Status:** released. **Scope:** 9 merged PRs since v2.48.1 (#373, #375, #376, #386, #387, #388, #391, #393, #394), closing 13 issues (#363, #364, #365, #366, #367, #368, #369, #370, #371, #372, #382, #383, #389, #390).

**Theme:** every gap this release closes has one shape. Something checked the generated output by reading it, or by compiling something adjacent to it, and the difference between the checked path and the shipped path is where the defect lived. Snapshots proved text stability while the text was stably wrong. A compile gate built a hand-written stub whose APIs the real framework does not provide. A reproducer sized an account by a guess that happened to be right for the one type the fixture used.

The fix in each case was to compile or execute the real thing.

## 1. Quasar codegen emitted programs that could not build (#372, #383, #365)

`codegen --target quasar` produced Rust that did not compile, and had for as long as the target existed. Three bundled examples ship as Quasar crates, so all three shipped broken.

- `quasar-lang` is `#![no_std]` and defines no `Pubkey`; its prelude re-exports `solana_address::Address`. The type mapping shared one arm between Anchor and Quasar, so every generated Quasar program named a type the framework does not have (#372).
- The Parallax integration scaffold built its account fixture as a struct literal. Quasar's `#[account]` replaces the annotated struct with a `repr(transparent)` view over `AccountView` and moves the fields into a hidden zero-copy companion, so there is nothing to construct and nothing meaningful to serialize (#383). The fixture now emits account BYTES: the type's own `DISCRIMINATOR` followed by each field in declaration order, little-endian, no padding.
- The fixture was also a field short. It walked the spec's declared fields plus a hardcoded `bump`, while the struct emitter appends a synthesized `bump` AND `status`. `codegen_shared::flat_state_fields` is now the single list both read.

**Why it survived:** the compile gate built a hand-written stub crate that invented two APIs. Its state struct was a plain `wincode`-derived struct, and it hand-wrote `impl From<VaultError> for u32`, which Quasar's `#[error_code]` does not emit. Both fictions hid a defect that shipped. The gate now generates both halves and compiles them together (#365).

## 2. Anchor joins the integration lane (#366)

`codegen --integration` emitted the Parallax/LiteSVM scaffold only for Quasar, so the dominant brownfield target could not use it.

Everything Parallax-side is framework-neutral — world setup, `execute_with`, `Outcome` checks, account fixtures — so this is an adapter rather than a second scaffold. Quasar builds instructions through the generated `program::client`; Anchor has no generated client, so the scaffold spells out the ABI: `sha256("global:<handler>")[..8]`, the declared account metas in order, then Borsh-encoded arguments. `codegen/anchor_ix.rs` holds that encoding and the reproducer lane shares it.

Two framework differences are silent-wrong-answer shaped rather than compile errors, and both were found by compiling:

- **The address type.** Anchor's `declare_id!` yields a `solana_pubkey::Pubkey`; Parallax speaks `solana_address::Address`. Same 32 bytes, two crates, no `From` between them. Both scaffolds now route through a generated `program_id()` helper.
- **The error code.** Anchor's `#[error_code]` generates `From<E> for u32` that ADDS `ERROR_CODE_OFFSET` (6000), so the bare variant is already the on-chain code. Quasar generates no such impl, so the scaffold emits `Err::X as u32` there. Naming it the same way on both compiles fine and asserts the wrong error.

Pinocchio still skips, with an honest reason: it dispatches on a leading discriminant byte, which is a different builder.

## 3. The reproducer lane sized accounts by a guess (#389)

The Parallax reproducer installs a pre-state account before driving its attack. It sized that account by asking a helper that knows `U8`..`U128` and gave everything else a flat 32 bytes. Correct for `Pubkey`, which is the only reason the lane worked. Wrong for every signed integer, `Bytes64`, and every `Map[N] T` — a `Map[4] Pubkey` came out 96 bytes short.

A wrong-length account fails deserialization before the guard under test runs, so the attack is recorded as refused and the finding is dropped. That is a false negative in the one lane whose entire job is evidence.

`codegen_shared::fixed_byte_width` now derives the width from the declared type and refuses what it cannot size, which makes `generate` fallible: a spec carrying an unsizable type produces no reproducer and a stated reason instead of one built on a guess.

**Why it survived:** the gate fixture declared exactly `owner : Pubkey` and `total : U64`, the two types the old code sized correctly. It now carries `Map[4] Pubkey` and `I64`, and the failure was reproduced end to end before the fix and after.

## 4. Drift detection read one leg of three (#382)

`check --drift` computed each stamp's status from the body hash alone. A stamp whose `spec_hash` had gone stale reported `OK` and counted as verified — the one leg the proc macro rejects the build on.

The asymmetry was the tell: the reporter checked one leg, the updater refreshed three, and a correct three-leg checker sat between them, used by neither. `drifted_legs()` is now the single definition of "a stamp is current", shared by the verb, the post-codegen scan, and the exit-code gate. The report names the stale leg, because "regenerate" and "re-verify" are different fixes.

## 5. Nothing compiled the generated program crate (#364)

`check` is spec-level lint, and `verify` built the harnesses in isolated crates on purpose, so a codegen defect reached the user as a red `cargo build` with no qedgen diagnostic. `verify --scaffold` runs `cargo check --tests` over the generated crate and reports it as a backend like proptest or Kani.

This converts the whole "codegen emits non-compiling Rust" family from user-discovered to tool-discovered, and does it on the user's own spec rather than on whatever a bundled example happens to exercise.

## 6. Error variants had two independent lists (#363)

Error-variant declaration and use were derived separately, so a `requires … else <Variant>` could name a variant the enum never declared: generated program does not compile, `check` reports `0 error(s)`. One resolver now serves the enum emission, the lint, and the integration scaffold.

## 7. Correctness and hygiene

- **`program_id` placeholder (#368).** A spec with no `program_id` got the System Program's address stamped as its own `declare_id!` — a valid base58 pubkey, so nothing rejected it on shape. Now marked in the emitted source, linted by `check`, and refused outright by the reproducer lane, which had been aiming attack transactions at the System Program and reporting "no bug".
- **Pinocchio regen-drift (#367).** The framework detector had two arms and an `else None`, and `None` meant "skip this crate", so a Pinocchio program's generated sources were never compared.
- **Retired output paths (#369).** When a generator default moves, the file at the old path was never swept: not compiled, not removed, still drift-compared. Four orphans are gone from the bundled examples.
- **Manifest regeneration was not idempotent.** Every `codegen` run added one blank line per section to the generated `Cargo.toml`, unbounded — 24 lines to 36 over five runs. Fixing it exposed a second half: the merged form carried a trailing blank line the greenfield render did not, which would have read as `regen-drift` on every example forever once the growth stopped masking it.
- **Output-path doubling warning (#370).** Relative `--*-output` paths resolve against the spec's directory. A path typed against the invoker's directory silently doubled it; `--help` now states the rule and codegen warns on the shape.
- **Pin liveness is mechanical (#371).** `check-parallax-pin.sh` was a manual release-checklist step, but its failing condition breaks generated scaffolds in a user's crate and has nothing to do with release timing. It now runs weekly and from the release gate.
- **Integration ordering (#390).** `codegen --integration` before the program crate exists has no manifest to upsert the Parallax dev-dependencies into. It skipped silently and wrote the scaffold anyway; it now says so and names the fix.
- **Proptest harness correctness (#375, #376, #386).** The sequence harness respects each property's `preserved_by` scope, `arb_state` satisfies conservation invariants by construction, and a rejected op restores the pre-state.

## Upgrading

No spec changes are required and no CLI flags were removed.

Regenerate to pick up the fixes: `qedgen codegen --all` for the artifacts, then `qedgen verify --scaffold` to confirm the generated crate compiles. Quasar users should regenerate — the pre-2.49 output did not build.

Anchor users can now add `--integration` for a Parallax/LiteSVM scaffold. It needs the program crate to exist first, because the dev-dependency upsert writes into its `Cargo.toml`.

## Known gaps

- Pinocchio has no integration-test instruction builder; `--integration --target pinocchio` skips with a note.
- The artifact gate no longer compiles what `--all` emits, because that gate executes what it generates and the integration scaffold needs a built `.so`. `--all` is exercised for exit status and determinism, not compilation (#395).
- `Fin[N]` has no single fixed byte width: Quasar packs it as `PodU32`, Anchor maps it to `usize`. `fixed_byte_width` refuses it rather than pick one.
