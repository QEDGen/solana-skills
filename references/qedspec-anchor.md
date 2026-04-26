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

The classifier in `anchor_resolver` walks each handler's tail expression. Production Anchor programs split across five conventions (the in-the-wild survey driving the taxonomy is internal):

| Shape                | Tail expression                                                          | Adapter behavior |
|----------------------|--------------------------------------------------------------------------|------------------|
| Inline               | multi-stmt body in the program mod fn (incl. `let`-bindings, `require!`) | program mod fn IS the handler |
| Free-fn forwarder    | `module::function(args)` (also `<call>?` and `<call>?; Ok(())`)          | walks `src/` to `pub fn function` |
| Type-associated      | `Type::method(ctx, args)` (PascalCase prefix)                            | walks for `impl Type { pub fn method }` |
| Accounts-method      | `ctx.accounts.method(args)`                                              | reads `Context<X>`, walks for `impl X { pub fn method }` |
| Unrecognized         | custom dispatcher / closure / non-path call                              | scaffolded with a `// TODO: classify manually` note + `--handler` override |

File-to-module mapping (`src/foo/bar.rs` → `["foo", "bar"]`) seeds the resolver so a forwarder like `instructions::buy::handler` resolves against `src/instructions/buy.rs` even when the file's items aren't syntactically wrapped in `pub mod instructions { pub mod buy { ... } }`.

See the worked examples:
- `examples/anchor-brownfield-demo/` — free-fn forwarders (full before+after walkthrough)
- `examples/regressions/anchor-adapter-shapes/marinade-style/` — accounts-method forwarders (`ctx.accounts.<method>(...)`); adapter-output snapshot fixture
- `examples/regressions/anchor-adapter-shapes/squads-style/` — type-associated forwarders (`<Type>::<method>(ctx, ...)`); adapter-output snapshot fixture
- `examples/regressions/anchor-forwarder-multistmt/` — `<call>?; Ok(())` two-stmt forwarders + Inline

## `#[qed]` drift loop

The proc-macro `#[qed(verified, spec = ..., handler = ..., hash = ..., spec_hash = ..., [accounts = ..., accounts_file = ..., accounts_hash = ...])]` is the seal. Three legs, two required, one optional:

- **Required** — `hash`: SHA-256-hex16 of the function body's canonical token stream after outer-attribute stripping. Works on free fns (`syn::ItemFn`) and impl methods (`syn::ImplItemFn`) alike via the `FnLike` shim, so accounts-method (`ctx.accounts.process(...)`) and type-associated (`Type::method(ctx, args)`) handlers seal end-to-end.
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

Accounts-method (`ctx.accounts.process(...)`) and type-associated (`Type::method(ctx, args)`) handlers seal end-to-end, the same as free-fn shapes. Place `#[qed]` directly on the impl method:

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

## Custom dispatcher override

Handlers whose program-mod fn body uses a custom dispatcher table
(runtime lookup, function pointer indirection, closure-call shape)
can't be followed by the classifier. Use the `--handler` flag to
point the adapter at the actual implementation:

```
qedgen adapt --program ./programs/dispatcher \
  --handler dispatch=instructions::dispatch::handler \
  --handler ix2=instructions::ix2::run
```

Each `--handler <name>=<rust_path>` is repeatable. The path uses
`module::sub::function` syntax (or just `function` for a top-level
free fn). Override paths are treated like hand-supplied free-fn
forwarders: the resolver walks `src/` for `pub fn <function>` at the
named module path. Both scaffold mode and attribute mode honor
overrides.

## Effect coverage gate

`qedgen check --anchor-project <path>` also runs an effect-coverage
lint: for each spec handler with an `effect { ... }` block, it
verifies the resolved Rust handler body contains at least one
assignment-like mutation whose LHS leaf matches each declared
effect's field name. Catches the "I added an effect to the spec but
forgot to wire it in code" footgun. Heuristic — not a proof of
semantic equivalence — but cheap and bounded.

```
$ qedgen check --spec my_program.qedspec \
    --anchor-project ./programs/my_program

Anchor cross-check (`./programs/my_program`) — spec and program handler sets agree.
Effect coverage — 1 unimplemented effect(s):
  ! handler `withdraw`: spec effect on `balance` has no matching mutation in the Rust body — either implement it (assign to a path ending in `.balance`) or remove the effect from the spec
```

## Cosmetic-edit tolerance for spec hash

Spec-hash computation runs the extracted `handler { ... }` block
through a normalizer before hashing. Cosmetic edits don't fire drift:

  - Runs of whitespace outside string literals collapse to one space.
  - `// ...` line comments and `/* ... */` block comments are stripped.
  - Leading/trailing whitespace is trimmed.
  - String literal contents (including escape sequences) pass through
    verbatim — `"hello   world"` stays `"hello   world"` because the
    spaces inside the literal are semantically meaningful.

Semantic edits — operator changes, identifier changes, added/removed
clauses — still trip drift, since the canonical bytes change.

## Limitations + roadmap

- **Effect coverage is heuristic.** The lint checks that *some*
  mutation targets each effect's field; it doesn't verify the RHS
  matches the spec's expression or that the operator (`=` vs `+=`)
  agrees. A handler with `state.balance = 0;` "covers" a spec effect
  `balance += amount`. Future passes (v3.0+) can tighten this once
  we have signal on which precision level pays.
- **Override path syntax.** `--handler <name>=<path>` resolves a free
  fn. Method-shape overrides aren't yet supported via this flag —
  refactor the dispatcher target to a free fn or use the existing
  scaffold-then-paste loop.
