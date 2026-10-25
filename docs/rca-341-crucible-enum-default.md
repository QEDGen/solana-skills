# RCA: #341 — Crucible harness fails to compile for enums with large padding arrays

**Status:** investigation complete, 2026-07-24. Upstream issue and PR are NOT filed. The user holds the greenlight.
**Scope:** local document. Do not commit.

## Summary

`qedgen probe --fuzz` emits a Crucible harness that calls `crucible_idl_gen::declare_fuzz_program!`. The macro generates a `Default` impl for every IDL type. For an **enum** whose first variant contains a `[u8; N]` field with `N > 32`, the generated impl uses `Default::default()` for the field. Rust does not implement `Default` for arrays longer than 32 elements, so the harness fails to compile with `E0277`. Structs with the same field shape compile fine — upstream already fixed the struct path. The bug is confined to one function in upstream crucible. The fix is two lines. QEDGen needs no code change.

## Impact

- Any Anchor program whose IDL contains an enum with a `[u8; N>32]` field in its **first** variant cannot produce a compiling fuzz harness. Padding arrays of 64–512 bytes are the standard Anchor pattern for fixed-size account layouts, so this hits real programs often.
- Reported externally by user xeroc (issue #341, filed 2026-07-22) from one `probe --fuzz` run on a pull-payment program. It was the third of a trio; the other two (#340, #342) were qedgen bugs and are fixed (PRs #352/#353, merged 2026-07-24).
- The failure is a hard compile error, not a silent quality loss. Users see it immediately.

## Where the fault is

The fault is in **upstream crucible** (`asymmetric-research/crucible`), crate `crucible-idl-gen` 0.2.0, at HEAD `daeaa4d` (2026-07-03). It is not in qedgen. qedgen's role is only that the generated harness depends on the crate.

### The two code paths

`crucible-idl-gen` generates Rust types from the IDL. When a type cannot derive `Default` (the check `can_type_derive_default` correctly returns false for arrays with `N > 32`), the generator writes a manual `impl Default`.

**Struct path — correct.** `generate_manual_default` (`crates/crucible-idl-gen/src/codegen/types.rs:647`) routes every field through `type_default_value`, which emits a literal for arrays:

```rust
// type_default_value, arrays:
IdlType::Array(inner, len) => {
    // emits [0; N] for primitive elements, [Inner::default(); N] otherwise
}
```

Generated output for a struct with `padding: [u8; 97]`:

```rust
impl Default for PaddedStruct {
    fn default() -> Self {
        Self { amount: 0, padding: [0; 97usize] }   // compiles for any N
    }
}
```

**Enum path — broken.** `generate_enum_manual_default` (`types.rs:416`) does not call `type_default_value`. It emits a bare trait call for every field of the first variant:

```rust
Some(IdlDefinedFields::Named(fields)) => {
    let field_defaults = fields.iter().map(|f| {
        let field_name = format_ident!("{}", f.name);
        quote! { #field_name: Default::default() }   // <-- the bug
    });
    ...
}
Some(IdlDefinedFields::Tuple(types)) => {
    let defaults = types.iter().map(|_| quote! { Default::default() });   // <-- same bug
    ...
}
```

Generated output for an enum whose first variant has `padding: [u8; 97]`:

```rust
impl Default for MyPolicyType {
    fn default() -> Self {
        Self::Limited {
            amount: Default::default(),
            padding: Default::default(),   // E0277: [u8; 97]: Default not satisfied
        }
    }
}
```

This matches the error sketch in xeroc's report exactly. The report frames the bug as "every `[u8; N>32]` padding field", but the struct case works at upstream HEAD. The failing shape is specifically an enum with a large array (or any non-`Default` type) in its first variant. The enum path takes over only when the first variant is not a unit variant; unit-first enums derive `Default` and are unaffected.

## Reproduction

Done against a fresh clone of upstream at HEAD `daeaa4d`, in the session scratchpad.

1. Fixture `repro_341.json`: a minimal Anchor IDL with two types — enum `MyPolicyType` (first variant `Limited { amount: u64, padding: [u8; 97] }`) and struct `PaddedStruct` with the same two fields.
2. A test inserted into `crucible-idl-gen`'s existing `run_full_pipeline` harness prints both generated `Default` impls.
3. Result before the fix: enum emits `padding: Default::default()`; struct emits `padding: [0; 97usize]`. The asymmetry proves the diagnosis.

## Validated fix

Two lines in `generate_enum_manual_default`: route fields through the same `type_default_value` helper the struct path already uses.

```diff
             let field_defaults = fields.iter().map(|f| {
                 // Use the IDL field name directly (preserves camelCase)
                 let field_name = format_ident!("{}", f.name);
-                quote! { #field_name: Default::default() }
+                let default_value = type_default_value(&f.ty);
+                quote! { #field_name: #default_value }
             });
```

```diff
         Some(IdlDefinedFields::Tuple(types)) => {
-            let defaults = types.iter().map(|_| quote! { Default::default() });
+            let defaults = types.iter().map(type_default_value);
```

After the patch, the repro fixture emits `Self::Limited { amount: 0, padding: [0; 97usize] }`. The upstream test suite passes 229 of 231 tests. The two failures are `test_enum_manual_default_first_variant_named_fields` and `test_enum_manual_default_first_variant_tuple_fields` — they assert the literal string `Default :: default ()` in the output, so they encode the old behavior. An upstream PR must update those two assertions and should add a regression test for a large array inside an enum variant. The full patch file is at the session scratchpad as `crucible-341-fix.patch`; it is reproducible from the diff above if the scratchpad is gone.

## Mitigation analysis: why the qedgen-side clamp is rejected

The candidate mitigation on record was: rewrite `{"array": ["u8", N>32]}` to `N = 32` at the `write_synthesized_idl` seam, on the assumption that the harness never deserializes real account data. The investigation rejects this for two independent reasons.

1. **The assumption is false.** `crucible-test-context` deserializes real account bytes through the IDL-generated types: `read_anchor_account` skips the discriminator and calls `T::deserialize` on live account data (`crates/crucible-test-context/src/lib.rs:1974`), and the mock-oracle helpers do the same (`mock_oracles.rs:212`). Shrinking a padding array changes the Borsh layout, so every typed read of a real account would misparse or fail.
2. **The seam does not cover the failing path.** `write_synthesized_idl` serves only the Pinocchio flow. For Anchor-family programs — the reported case — the harness gets the real `anchor build` IDL by symlink (`crucible_probe::discover_idl`, `crucible_probe.rs:969`). A clamp would require qedgen to stop symlinking and start rewriting the user's IDL, which adds a divergence between the harness IDL and the program's true layout.

This also matches the repo principle: fix the root cause in source, do not work around it (`feedback_root_cause_not_patch`). The root cause is upstream, so the fix belongs upstream.

## Propagation: why no qedgen change is needed

The generated harness `Cargo.toml` pins the crucible crates by git branch, not by version (`crucible_gen.rs:239`):

```toml
crucible-idl-gen = { git = "https://github.com/asymmetric-research/crucible", branch = "main" }
```

Once the upstream fix merges to `main`, every freshly built harness resolves the fixed code. No crucible release, no qedgen release, and no template change are required. Existing harnesses with a stale `Cargo.lock` need `cargo update -p crucible-idl-gen` or a regenerated harness.

## Upstream state

- No existing issue or PR for this in `asymmetric-research/crucible` (searched all states, 2026-07-24).
- The struct-path fix already in the repo (`test_pipeline_anchor_whirlpool` asserts a manual impl for `TickArray` with `[Tick; 88]`) shows the maintainers know about the 32-element limit. The enum path was an oversight, which makes the PR easy to motivate.

## Next steps (on user greenlight — do not act before)

1. File the upstream issue. Frame it as enum-specific. Cite the working struct path as the in-repo precedent. Reference qedgen issue #341 for the field report.
2. Open the upstream PR: the two-line fix, the two test-assertion updates, and a regression test (enum first variant with `[u8; 97]`).
3. After the upstream merge: close qedgen #341 with a comment that explains the branch-pin propagation and the `cargo update` step for stale lockfiles.
4. No qedgen code change. Optionally add a line to `docs/limitations.md` while the upstream fix is pending.

## Evidence index

| Claim | Evidence |
|---|---|
| Enum path emits bare `Default::default()` | `types.rs:416` (`generate_enum_manual_default`), repro output before patch |
| Struct path is correct | `types.rs:647` (`generate_manual_default` → `type_default_value`), repro output |
| `>32` check exists and is right | `can_type_derive_default`, `types.rs` (`IdlArrayLen::Value(n) => *n <= 32 && …`) |
| Fix works | Repro output after patch; 229/231 upstream tests pass; 2 failures are stale assertions |
| Harness deserializes real accounts | `crucible-test-context/src/lib.rs:1974`, `mock_oracles.rs:212` |
| Clamp seam misses Anchor | `crucible_brownfield.rs` (`BrownfieldSynthesis.idl_json` is `None` for Anchor-family); `crucible_probe.rs:969` symlink |
| Fix propagates without a qedgen change | `crucible_gen.rs:239` (`branch = "main"` git pins) |
| No prior upstream report | `gh issue list` / `gh pr list` on `asymmetric-research/crucible`, 2026-07-24 |
