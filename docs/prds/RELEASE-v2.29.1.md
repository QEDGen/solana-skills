# Release v2.29.1

**Date:** 2026-05-24
**Type:** Patch — small feature + documentation + a non-trivial bundled example.

Follow-up to v2.29.0 surfaced by user review the day v2.29.0 cut. One real
feature (dotted `auth`), two policy docs, plus the bundled regression for
the v2.29-era cross-program shape.

## What's in

### Feature

**Dotted `auth <acct>.<field>` sugar** (`crates/qedgen/src/chumsky_parser.rs`,
`crates/qedgen/src/chumsky_adapter.rs`). The `auth` clause now accepts a
qualified path so the signing identity can live on an imported program's
account without a phantom state field workaround. Closes the canonical
cross-program-auth shape directly (v2.29.0 closed it via the
`requires <acct>.<field> == <signer>.pubkey else Unauthorized` longhand
form, which still works for multi-signer handlers).

```fsharp
handler emergency_close : State.Active -> State.Active {
  auth admin_config.admin           // reads from imported AdminConfig.State
  accounts {
    admin         : signer
    admin_config  : type AdminConfig.State
    ...
  }
  ...
}
```

The adapter desugars to `requires admin_config.admin == admin.pubkey else
Unauthorized` against the handler's lone signer. Handlers with 0 or 2+
signers skip the desugar (the `requires` longhand is unambiguous in those
shapes).

### Bundled example

**`examples/rust/cross-program-vault/`** — first bundled regression for
v2.29's cross-program-state + CPI shape end-to-end. Vault imports an
`AdminConfig` data-shape spec (cross-program state read via the v2.29
unified-import path) AND issues SPL Token CPIs (`call Token.transfer(amount
= state.total_deposits, …)`). The companion `examples/imports/cross-program-vault-admin/`
holds the foreign data-shape spec — located outside `examples/rust/`
because the `--regen-drift` gate walks `examples/rust/*` as separate
examples and the admin-side has no handlers to codegen.

The drift script (`regen_drift::copy_interfaces`) now copies
`examples/imports/` into the temp regen dir so the consumer's
`path = "../../imports/X"` resolves at drift-check time.

### Documentation

**SKILL.md — Cross-program patterns** now lists three lowering paths
in preference order: dotted `auth` (preferred when there's a lone
signer), persist-on-init via `<account>.pubkey`, inline `requires`
field comparison (general fallback).

**SKILL.md — "When the spec hits a wall: fail fast, file an issue"** is
the binding behavior contract for spec-authoring agents. Trip-wires
extend beyond `qedgen check` errors to cover:

- `cargo check` / `cargo build` on the generated Rust scaffold
- `cargo kani` on the generated Kani harness (elaboration failure)
- `cargo test` on the generated proptest harness
- `lake build` on the generated `Spec.lean` / `Proofs.lean`

Framing: when a codegen-output target doesn't compile, the user wrote
a valid spec and the GENERATOR emitted broken output. Hand-editing the
generated file is the worst possible response — the next `qedgen
codegen` regenerates over the edit.

Explicit anti-pattern list:
- Phantom state fields to satisfy `auth` / `requires`
- Manual `transfers` blocks to silence `missing_cpi_for_token_context`
- Hand-edited generated files (anything other than `instructions/<name>.rs` handler bodies)
- Parser-tricking renames
- Spec-side type changes that "happen to make codegen work" but no longer describe the program
- Removing failing handlers / properties to silence the build

**Sanitization is mandatory before filing.** GitHub issues are public,
and spec fragments leak protocol identity / business logic / pubkeys.
The fail-fast script's step 3 now requires the agent to rewrite the
failing fragment as a generic reproducer first — scrub real pubkeys,
named accounts / fields / handlers that hint at protocol identity,
deal-specific constants (fee bps, tier thresholds), repo paths,
product-name comments — and step 4 requires explicit user approval
before any `gh issue create` call. Default answer is no.

Issue-template captures the SANITIZED fragment, the verbatim error
(with user-paths stripped), the generated-file role (`guards.rs` /
`Spec.lean` / etc., not the file path), and the workarounds the agent
considered and rejected.

User-authorized workarounds get a `// FIXME(qedgen-issue: <url>):`
marker so the regression is auditable; hand-edits to generated files
additionally require the issue body to note the edit will be overwritten
on the next regen.

Mirrored a short pointer at the bottom of `docs/limitations.md` so the
fail-fast policy is discoverable from both SKILL.md (the entry point)
and limitations.md (the lookup spot when triaging a specific lint).

### Codegen drift

`qedgen check --regen-drift --write` refreshed `examples/rust/{escrow,
lending, multisig}/Cargo.toml` (and the since-removed perp-dex example's) to match v2.29's codegen
output (`InitSpace` derive on multi-account wrappers, `space = 8 +
<Wrapper>::INIT_SPACE` on init handlers, the `mut`+`init` exclusion
fix). v2.29.0's first CI run failed on this gate; v2.29.1 ships clean.

## Quality gates

- 940/940 unit tests pass
- `cargo fmt --check`, `cargo clippy -- -D warnings` clean
- `qedgen check --regen-drift` clean (8 examples)
- All 7 originally-bundled examples + the new `cross-program-vault` →
  `cargo check` clean on fresh-dir codegen

## Migration from v2.29.0

Zero spec changes required. Dotted `auth` is purely additive — existing
`auth <single_ident>` continues to lower to `has_one = <name>` on Anchor.
Spec authors who manually wrote `requires foreign_acct.field ==
signer.pubkey else Unauthorized` to work around v2.29.0's missing dotted
form can simplify to `auth foreign_acct.field` if their handler has a
lone signer.
