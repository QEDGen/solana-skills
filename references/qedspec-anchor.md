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

## `#[qed]` drift loop (free-fn / inline only in v2.9)

The proc-macro `#[qed(verified, spec = ..., handler = ..., hash = ..., spec_hash = ...)]` is the seal. At compile time, `qedgen-macros`:

1. Reads the `.qedspec` at `<spec>` (resolved against `CARGO_MANIFEST_DIR`).
2. Extracts the named `handler { ... }` block via balanced-brace scan, hashes it.
3. Hashes the function body via `func.to_token_stream()` after stripping outer attributes.
4. Compares both to the pinned `hash = "..."` and `spec_hash = "..."`.
5. Mismatch → `compile_error!` with a "Expected: ... Actual: ..." diff. Match → pass-through.

`qedgen adapt --spec` precomputes both hashes via the same algorithms (`spec_hash::body_hash_for_fn` + `spec_hash::spec_hash_for_handler`) so the user just pastes the output.

### What edits trip drift

- Edit the function body (a statement, an arithmetic op, a `let` binding, even a parameter type) → body hash changes → `compile_error!`.
- Edit the spec's `handler { ... }` block (any byte inside the braces, including whitespace) → spec hash changes → `compile_error!`.
- Edit anything *outside* the braces (other handlers, type declarations, comments above the handler) → no effect.
- Add or remove an attribute on the handler (e.g. `#[inline]`) → no effect (attributes are stripped before hashing).

### Refresh after intentional edits

```
qedgen adapt --program <crate> --spec <path>
```

Re-emits all attribute lines with current hashes. Paste in the changed handlers. Build clears.

For the success path + drift demo end-to-end, see `examples/qed-drift-fixture/`. That fixture is a workspace member, so workspace `cargo test` exercises the full chain on every CI run.

### Method-shape forwarders in v2.9

`Marinade::process(...)` / `Type::method(ctx, args)` handlers come through `qedgen adapt --spec` with:

```
// === handler: deposit ===
// source: src/instructions/deposit.rs
// note: method-shape forwarder (`impl Deposit` block) — `#[qed]` annotation requires a free-fn handler in v2.9. Either refactor to a free fn or wait for v2.10's impl-method support
```

Two paths:
1. **Refactor to free-fn.** Move the body out of the impl into a `pub fn deposit(...)` at module level; the `Context<X>::deposit(...)` forwarder calls into it. Now `#[qed]` works.
2. **Wait for v2.10.** Method-shape support is on the roadmap; the proc-macro will handle `syn::ImplItemFn` alongside `syn::ItemFn`.

The scaffold path works for method shapes today; only the seal step is gated.

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

- **Method-shape `#[qed]`:** v2.10. Requires the macro to parse `syn::ImplItemFn` in addition to `syn::ItemFn`.
- **Accounts-struct in body hash:** v2.10 polish. Today the macro hashes only the handler body; constraints declared on `#[derive(Accounts)]` are validated by `qedgen check --anchor-project` rather than the proc-macro. The PRD's combined hash will fold in once we have the macro reading the accompanying struct.
- **Drift custom dispatchers:** the v2.9 adapter classifies these as `Unrecognized`. Future `--handler <name>=<rust_path>` override flag is on the roadmap.
- **Cosmetic-edit tolerance:** the spec hash is whitespace-sensitive on purpose (small reformats today, larger drift later — pinning whitespace surfaces the small ones early). v2.10+ may add a normalize-on-hash mode behind a flag.
