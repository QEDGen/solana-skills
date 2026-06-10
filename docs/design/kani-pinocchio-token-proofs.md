# Pinocchio Kani Impl Proofs Without Program Special Cases

## Summary

This branch moves QEDGen's Pinocchio `--kani-impl` backend toward an upstreamable shape. The generator now keeps the implementation-targeted proof surface generic: it emits the shared Pinocchio stack-account scaffold, declares symbolic parameters from the spec, builds accounts from the handler `accounts {}` block, calls `crate::process_instruction`, and emits token balance delta assertions from `Token.transfer` calls.

The backend no longer selects custom proof code from `spec.program_name`. Project-specific account layouts, PDA choreography, config bytes, instruction-data narrowing, and handler arity records must come from a source or ABI-derived proof profile instead of being hardcoded in QEDGen.

## Motivation

The first Pinocchio token-balance proof spike showed that Kani can check useful implementation behavior against a real dispatcher. It also exposed an upstreaming problem: a general QEDGen library should not know a specific program's account order, config layout, PDA helpers, or instruction encoding rules by name.

The generic parts are still valuable. A spec-level `Token.transfer` has enough structure for QEDGen to snapshot source and destination token account amounts, assume the arithmetic preconditions needed for a successful transfer, and assert source-minus and destination-plus deltas after the dispatcher returns `Ok`.

The non-generic parts need a different source of truth. For Pinocchio programs, account ordering and byte encoding usually come from the program source, an ABI schema, or both. Those facts are now represented as an intermediate proof profile consumed by the Kani emitter.

## Current Behavior

For each Pinocchio handler with `ensures` or concrete state effects, QEDGen emits one implementation-targeted proof. The proof constructs stack-resident `AccountInfo` values using a Pinocchio layout mirror, creates SPL Token-shaped account data for non-program accounts declared as token accounts, creates SPL Mint-shaped account data for accounts declared or inferred as mints, creates symbolic non-token account data when an ABI account layout is available, packs a dispatcher instruction tag, appends numeric parameters, and calls the real `process_instruction`.

When the output path is next to an existing Pinocchio `src/` tree, `pinocchio_profile.rs` parses Rust files with `syn` and infers a proof profile from the syntax tree. The parser-backed path extracts dispatcher tags from `process_instruction` match arms, handler bodies from `process_*` functions, account order from `let [a, b, ..] = accounts` destructuring or sequential `next_account_info` reads, source-side role checks such as `account.is_signer()` and `account.is_writable()`, SPL token program identity checks, mint-decoding calls, token-account decoding calls, token account mint/owner bindings from helpers such as `require_token_account(account, mint.key(), owner.key())`, simple wrapper and struct-field aliases, local key variables assigned from `derive_*` calls, direct `require_key(account, &derive_*(...))` account-key checks, `require_key(account, &local_derived_key)` alias checks, numeric payload fields from `instruction_data.get(start..end)` parsing, and simple `derive_*` PDA seed lists from `find_program_address` calls. A conservative string-based extractor remains only as a fallback when a Rust file cannot be parsed. The profile also imports nearby line-oriented ABI schemas from `schema/*.schema` when present, including sibling workspace crates with schema directories. The ABI parser uses `instruction`, `account`, `record`, `field`, `repeat`, `instruction_record`, `account_record`, `magic`, and `seed` declarations to recover instruction tags, account order, optional account role metadata, scalar argument offsets, scalar widths, record layouts, account-to-record bindings, repeated-record count fields, repeated item fields, fixed byte literals, and literal seed bytes. The Kani emitter consumes the instruction-packing, account-role, account-layout, and PDA-seed facts when present and falls back to spec-declared order or placeholder keys when source or ABI evidence is missing.

When the spec contains `call Token.transfer(from = ..., to = ..., amount = ...)`, the proof snapshots `read_token_amount` on both accounts before dispatch. It assumes the source has at least the transfer amount and the destination addition cannot overflow. If the dispatcher succeeds, it asserts the concrete token-account byte deltas.

For arity-specialized model names such as `batch_16`, the fallback instruction tag helper strips the numeric suffix so the generated tag remains `crate::BATCH`. The proof profile lookup also prefers the base handler name for numeric arity suffixes, so ABI declarations such as `instruction BATCH 4` feed every generated proof for spec handlers named `batch`, `batch_2`, or `batch_16`.

ABI repeats with scalar item fields are packed by deriving a concrete item count from the spec handler's unsuffixed or indexed parameters. The emitter writes the count field, then writes each item field in ABI order using ABI scalar widths. For example, a schema record with `field COUNT u8` and `repeat ITEM transfer MAX COUNT` can pack handlers with `amount`, `from_lane_id`, `to_lane_id` or with indexed forms such as `amount_0`, `from_lane_id_0`, `to_lane_id_0`. Direct `Pubkey` handler parameters are declared as symbolic 32-byte arrays and packed as raw bytes, which gives later PDA seed projection a concrete local value to reference.

ABI `magic` declarations are matched to `bytesN` record fields when the normalized names match unambiguously. The emitter initializes those byte ranges after creating symbolic account data, so account discriminators and other fixed literals can be present without a project-specific account builder.

For PDA derivations whose source helper uses the dispatcher `program_id`, the emitter now computes the account key from profiled seeds. This works for exact account-name derivations, direct guards such as `require_key(account, &derive_*(...))`, local derived-key aliases checked by `require_key`, and repeated loop accounts whose source guard uses the loop item fields. The profile also records local `let key = derive_*(...).0` assignments inside PDA helper bodies. The emitter can render those one-level nested derived keys before an outer PDA and then use the nested key as a seed. It also supports source-derived PDA helpers that use a crate-level address-program constant, such as `&ASSOCIATED_TOKEN_PROGRAM_ID`, when the helper seeds are direct `Pubkey`, account-key, or one-level nested derived-key arguments. Literal seed names are resolved through ABI `seed` declarations, and dynamic numeric seeds use the source or ABI scalar width when one was recovered. Unsupported derivations fall back to placeholder keys instead of emitting a weak or program-shaped proof.

Accounts inferred or declared as `program type token` are keyed with the SPL Token program ID while still using the minimal program-account shape. Accounts inferred or declared as `type mint` use a compact initialized mint layout so handlers that read mint decimals can reach success paths without a project-specific mint builder.

When a token account binding is inferred, the generated token account data now writes the referenced mint key and owner key into the SPL Token account bytes before dispatch. If the owner is a local key variable from a known `derive_*` helper, the emitter derives that key from the profiled PDA seeds instead of using a placeholder. This keeps the existing amount-delta proof tied to the same concrete account layout checks the implementation performs.

Handlers without `ensures` do not emit Pinocchio impl proofs through a project-name exception. If such handlers need implementation-targeted checks, the spec should state the property to assert, or a future proof profile should describe concrete byte-level postconditions inferred from ABI data.

## Remaining Profile Work

The upstreamable long-term path is a Pinocchio proof profile produced from source plus ABI schema. The current parser-backed source profile covers account array order from destructuring and sequential iterator reads, source-derived signer/writable/program/token/mint role facts, source-derived token account mint/owner bindings, owner keys derived from local `derive_*` calls when the seed profile is known, source-guarded account keys from direct `require_key(account, &derive_*(...))` checks, local derived-key aliases checked by `require_key`, simple wrapper and struct-field source aliases, dispatcher tags, simple numeric payload fields, simple PDA seed discovery for derivation helpers, source-derived non-`program_id` PDA binding for direct, repeated-loop, and one-level nested account-key seeds, and SPL Token account and mint projection. ABI integration adds ABI-declared account roles, ABI-declared account ordering, ABI scalar field widths, ABI record layouts, account-to-record layout binding, repeated-record argument packing, fixed byte initialization for ABI data accounts, direct and repeated `Pubkey` parameter declaration and packing, ABI seed literal resolution, exact account-name PDA key binding for literal and numeric-param seeds, and sibling ABI-crate schema discovery. The remaining profile work is interprocedural role inference through arbitrary helper wrappers, deeper source expression aliasing across assignments and control flow, deeper nested PDA chains, non-literal data-field initialization, non-exact PDA/account matching, and concrete post-dispatch byte assertions.

The Kani emitter should keep consuming that profile without knowing the program name. This keeps QEDGen general while still allowing programs with rich ABI schemas to get precise implementation proofs.

## Proof Strength Boundaries

QEDGen now has several Kani proof shapes with different claims. The spec-model `--kani` backend proves consistency of the spec-translated transition model; it does not call the program implementation. The implementation-targeted `--kani-impl` backend calls the real dispatcher with symbolic accounts and instruction data; it can catch implementation paths that violate the modeled setup or trigger Kani overflow, underflow, or pointer checks.

The Pinocchio token delta proof is narrower than full CPI semantics. It currently proves byte-level SPL Token account amount deltas for successful dispatcher calls when the profile can construct the relevant token account bytes and the spec contains a `Token.transfer` call. For ABI-backed state accounts, the proof can also snapshot scalar fields before dispatch and assert concrete post-state effects plus unchanged fields when the spec states them. It does not yet prove arbitrary CPI semantics or all helper behavior outside the recovered source/ABI profile.

Every generated Pinocchio impl harness asserts that the generated ABI/profile witness reaches `Ok` from the real dispatcher. `kani::cover!(_result.is_ok(), ...)` remains useful reachability evidence, but it is supplemental only. A harness should be admitted to proof-completion gates only when it also asserts concrete post-state facts, such as token account debits and credits or ABI-backed state-field equality.

Generated `TODO` comments mark conservative fallback paths. A proof function that still contains placeholder packing, declaration, or repeat-count TODOs should be treated as partial generation requiring more profile evidence before it is presented as a concrete implementation proof for those facts.

Each generated Pinocchio impl proof now includes proof-profile notes for the handler. These comments report whether source account order, dispatcher or ABI tag, PDA derivations, and token owner/mint projections were found, missing, or unavailable. The notes are diagnostics for reviewers and users; the proof claim still comes from the concrete code that follows them.

## Validation

The focused repository check was run after removing the program-specific branch:

```sh
cargo test -p qedgen-solana-skills kani_impl -- --nocapture
```

It passed with 38 focused Kani-emitter tests. Additional profile tests cover parser-backed source-derived dispatcher/account/role/parameter extraction, account-order inference from sequential `next_account_info` reads, SPL token evidence from the source, direct account-key derivation from `require_key(account, &derive_*(...).0)`, context lookup for source-derived account-key derivations, multiline token account bindings through derived key aliases, nearby ABI schema import for tags/accounts/roles/scalar fields, sibling schema discovery from a spec-derived program path, record offset calculation, account-to-record layout binding, fixed byte extraction from ABI magic literals, repeated-record length calculation, repeated item-field exposure, generated `kani_impl.rs` exclusion, simple source-derived PDA seed extraction with ABI seed literal resolution, and numeric arity suffix lookup.

The updated Kani emitter tests assert that generic Pinocchio token-transfer balance deltas still emit. They also cover source-derived dispatcher tags, account order, payload widths, direct and repeated `Pubkey` parameter packing, token account mint/owner bytes, owner PDA variables from known `derive_*` seed profiles, account-key binding from direct and alias-based source `require_key` derivation guards, non-`program_id` PDA programs with direct and one-level nested account-key seeds, repeated loop account-key derivations through ABI item fields, ABI account-role projection for token accounts and mints, ABI data account byte lengths with fixed byte ranges, exact account-key binding from profiled PDA derivations, ABI repeated-record packing, arity-suffixed handler lookup, repeated token account binding through numeric suffixes, writable non-token account handling, runtime-neutral ABI packing, and project-shaped handlers without custom proof bodies.

The generated implementation proof gate is intentionally opt-in because it
requires `cargo kani`:

```sh
QEDGEN_RUN_CARGO_KANI_SMOKE=1 cargo test -p qedgen-solana-skills --test codegen_smoke generated_pinocchio_kani_impl_proves_with_cargo_kani -- --ignored --nocapture
```

That gate generates a generic Pinocchio fixture, writes `src/kani_impl.rs`,
checks that the green proof path has no generated `TODO:` placeholders, and
then proves `verify_ping_impl` with Kani.

The richer generic proof-lab fixture lives under
`crates/qedgen/tests/fixtures/pinocchio-fixtures/kani-profile-diversity`.
It is a compileable Pinocchio crate with source plus ABI schema. The fast test
asserts that emitted proofs use the expected source and ABI facts. The opt-in
Kani proof-lab gate proves selected generated harnesses with real `cargo kani`:

```sh
QEDGEN_RUN_CARGO_KANI_SMOKE=1 cargo test -p qedgen-solana-skills --test codegen_smoke generated_pinocchio_profile_diversity_kani_impl_proves_with_cargo_kani -- --ignored --nocapture
```

That proof-lab gate has two explicit layers. The non-ignored fixture test is an
emitter/profile regression check only: it verifies source and ABI facts such as
repeated-record packing, ABI magic-byte account layout, and non-`program_id` PDA
derivation. It is not proof-completion evidence.

The ignored proof-completion gate runs real `cargo kani` only for generated
harnesses that call the real dispatcher, assert `_result.is_ok()`, and assert
concrete post-state facts. The current proof-completion list covers basic token
byte deltas, fee-limit state writes, pause-flag state writes, a stable no-fee
two-leg router swap, withdraw token deltas, a one-transfer rebalance batch, and
a two-transfer router-style rebalance pair with indexed token delta assertions.
Layout-only and PDA-reachability-only harnesses remain regression checks until
they have concrete post-state assertions.

Downstream programs can validate their own generated `--kani-impl` proofs from
their qedspec, source, and ABI profile without adding program-specific branches
to QEDGen. If larger downstream cases hit solver limits, those limits should be
reported as proof-splitting or solver-scaling work, not as passed proof evidence.
