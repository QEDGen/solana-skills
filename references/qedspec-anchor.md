# Anchor brownfield workflow

v2.9 makes `qedgen` work natively against existing Anchor programs.
Three pieces:

1. `qedgen adapt --program <crate>` — discover handlers from source, emit a `.qedspec` skeleton.
2. `qedgen adapt --program <crate> --spec <path>` — emit `#[qed]` attribute lines pinning each handler's body + spec hash.
3. `qedgen check --spec <path> --anchor-project <crate>` — CI gate that asserts the spec's handler set matches the program's `#[program]` mod.

This document covers all three end-to-end. For the spec language itself, see `qedspec-dsl.md`. For spec composition (imports, `qed.toml`), see `qedspec-imports.md`.

## What `qedgen adapt` carries forward

| From the Rust source                                  | Into the `.qedspec` |
|-------------------------------------------------------|--------------------|
| `#[program] pub mod <name>`                           | `spec <PascalName>` |
| each `pub fn` in the program mod                      | one `handler <name> { ... }` block |
| typed arguments after `Context<X>`                    | `(arg_name : Type)` per primitive; user-defined types pass through; generics fall back to `U64` placeholder + body comment |
| `Context<X>` type                                     | `// accounts struct: \`X\`` comment |
| handler body location (free-fn / inline / method)     | `// discovered at: <path>` breadcrumb |
| `#[error_code] pub enum X { Variant1, ... }`          | `type Error \| Variant1 \| ...` (enum name surfaces in a comment) |

What stays as `// TODO:` is everything that needs *semantic* judgment: lifecycle states, `auth`, `accounts {}` block, `requires` and `effect` bodies, transfers, events. That's the work an LLM-with-source-in-hand or a human will do with the scaffold as a starting point.

## Forwarder shapes the adapter handles

The classifier in `anchor_resolver` walks each handler's tail expression. Production Anchor programs split across these conventions (per `reference_anchor_patterns.md`):

| Shape                | Tail expression                              | Real-world program     | Adapter behavior |
|----------------------|----------------------------------------------|------------------------|------------------|
| Inline               | multi-stmt body in the program mod fn        | Jito tip-distribution  | program mod fn IS the handler |
| Free-fn forwarder    | `module::function(args)`                     | Anchor scaffold, Raydium | walks `src/` to `pub fn function` |
| Type-associated      | `Type::method(ctx, args)` (PascalCase)       | Squads V4              | walks for `impl Type { pub fn method }` |
| Accounts-method      | `ctx.accounts.method(args)`                  | Marinade               | reads `Context<X>`, walks for `impl X { pub fn method }` |
| Unrecognized         | custom dispatcher / closure / non-path call  | Drift                  | scaffolded with a `// TODO: classify manually` note |

File-to-module mapping (`src/foo/bar.rs` → `["foo", "bar"]`) seeds the resolver so a forwarder like `instructions::buy::handler` resolves against `src/instructions/buy.rs` even when the file's items aren't syntactically wrapped in `pub mod instructions { pub mod buy { ... } }`.

See the worked examples:
- `examples/anchor-brownfield-demo/` — Anchor scaffold (free-fn forwarders)
- `examples/anchor-marinade-style-demo/` — accounts-method forwarders
- `examples/anchor-squads-style-demo/` — type-associated forwarders

## `#[qed]` drift loop

The proc-macro `#[qed(verified, spec = ..., handler = ..., hash = ..., spec_hash = ..., [accounts = ..., accounts_file = ..., accounts_hash = ...])]` is the seal. Three legs, two required, one optional:

- **Required** — `hash`: SHA-256-hex16 of the function body's canonical token stream after outer-attribute stripping. Works on free fns (`syn::ItemFn`) and impl methods (`syn::ImplItemFn`) alike via the `FnLike` shim, so Marinade-style `ctx.accounts.process(...)` and Squads-style `Type::method(ctx, args)` handlers seal end-to-end.
- **Required** — `spec_hash`: SHA-256-hex16 of the `handler <name> { ... }` block's raw text (braces included), whitespace-sensitive.
- **Optional** — `accounts` / `accounts_file` / `accounts_hash`: when present, the macro reads the file at `accounts_file` (resolved against `CARGO_MANIFEST_DIR`), finds `pub struct <accounts>`, hashes its tokens after outer-attr stripping, and compares to `accounts_hash`. Edits to fields, types, or `#[account(...)]` constraints fire drift.

Mismatch in any leg → `compile_error!` with an "Expected: … Actual: …" diff. All match → pass-through.

`qedgen adapt --spec` precomputes every leg via the same algorithms (`spec_hash::body_hash_for_fn`, `spec_hash::body_hash_for_impl_fn`, `spec_hash::spec_hash_for_handler`, `spec_hash::accounts_struct_hash`) so the user just pastes the output. The accounts triplet is auto-included whenever the adapter can find the `Context<X>` struct in source.

### What edits trip drift

- Edit the function body (a statement, an arithmetic op, a `let` binding, even a parameter type) → body hash changes → `compile_error!`.
- Edit the spec's `handler { ... }` block (any byte inside the braces, including whitespace) → spec hash changes → `compile_error!`.
- Edit a field, type, or inner `#[account(...)]` attribute on the `#[derive(Accounts)]` struct (when sealed via the optional triplet) → accounts hash changes → `compile_error!`.
- Edit anything *outside* those scopes (other handlers, unrelated type declarations, comments above the handler) → no effect.
- Add or remove an outer attribute on the handler or the accounts struct (e.g. `#[inline]`, `#[derive(Debug)]`) → no effect (outer attributes are stripped before hashing).

### Refresh after intentional edits

```
qedgen adapt --program <crate> --spec <path>
```

Re-emits all attribute lines with current hashes. Paste in the changed handlers. Build clears.

For the success path + drift demo end-to-end, see `examples/qed-drift-fixture/`. That fixture is a workspace member exercising all three legs (free-fn body, impl-method body, accounts struct), so workspace `cargo test` proves every leg of the drift loop on every CI run.

### Method-shape forwarders

Marinade-style (`ctx.accounts.process(...)`) and Squads-style (`Type::method(ctx, args)`) handlers seal end-to-end, the same as free-fn shapes. Place `#[qed]` directly on the impl method:

```rust
impl<'info> Deposit<'info> {
    #[qed(verified, spec = "stake.qedspec", handler = "deposit",
          hash = "...", spec_hash = "...",
          accounts = "Deposit", accounts_file = "src/lib.rs", accounts_hash = "...")]
    pub fn process(&mut self, lamports: u64) -> Result<()> {
        // ...
    }
}
```

The proc-macro tries `syn::ItemFn` first and falls back to `syn::ImplItemFn`, so the same attribute syntax works in either position. `qedgen adapt --spec` emits the right line whether the resolver classifies the handler as `Inline`, `FreeFn`, or `Method`.

## CI integration

`qedgen check --spec <path> --anchor-project <crate>` is the production gate. Two findings types:

- **Spec handler not in program.** A `handler X` block in the spec but no `pub fn X` in the `#[program]` mod. The user renamed in code and forgot to update the spec, or the spec was authored against a different version.
- **Program instruction not in spec.** A `pub fn X` in the program mod with no spec coverage. Verification has nothing to say about it.

Pair with `--frozen` (errors on stale `qed.lock`) for the full freeze gate:

```
qedgen check --spec my_program.qedspec \
  --anchor-project ./programs/my_program \
  --frozen
```

Output is plain stderr by default, JSON via `--json` for tools.

## Limitations + roadmap

- **Drift custom dispatchers:** the adapter classifies these as `Unrecognized`. The planned `--handler <name>=<rust_path>` override flag is on the roadmap.
- **Nested accounts struct:** `accounts_struct_hash` walks top-level items only. If `#[derive(Accounts)] pub struct Buy` lives inside `pub mod accounts { ... }`, the macro won't find it. Move it to the file's top level or use a sibling file (see `accounts_file` in the attribute).
- **Cosmetic-edit tolerance:** the spec hash is whitespace-sensitive on purpose (small reformats today, larger drift later — pinning whitespace surfaces the small ones early). v3.0+ may add a normalize-on-hash mode behind a flag.
