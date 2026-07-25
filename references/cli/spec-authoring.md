# CLI: spec authoring

`interface`, `spec`, `adapt`, `stamp` — producing and stamping a `.qedspec`.

Part of the [CLI Reference](../cli.md).

## Spec and validation

### `interface`
Generate a Tier-0 interface `.qedspec` from an Anchor IDL. Shape only —
program ID, discriminator, accounts, argument types. No `requires`/
`ensures`/`effect` (those require semantic understanding the IDL does not
carry). The `upstream` block is left as a TODO stub for the author to fill
in after running QEDGen harnesses against the deployed program.

See `docs/design/spec-composition.md` §2 for the CPI tier model.

```bash
# Print to stdout
$QEDGEN interface --idl target/idl/jupiter.json

# Write to an explicit path
$QEDGEN interface --idl target/idl/jupiter.json --out interfaces/jupiter.qedspec

# Vendor into .qed/interfaces/<program>.qedspec (canonical library location,
# resolved via the nearest .qed/config.json)
$QEDGEN interface --idl target/idl/jupiter.json --vendor
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--idl` | Path | required | Anchor IDL JSON file |
| `--out` | Path | - | Output path (default: stdout). Conflicts with `--vendor`. |
| `--vendor` | bool | false | Drop into `.qed/interfaces/<program>.qedspec`. Requires a discoverable `.qed/` ancestor. |

### `spec`
**DEPRECATED (slated for v3.0 removal).** The IDL is now an evidence
*source* for `qedgen probe` — the hypothesizer consumes IDL signer flags
and `has_one` relations directly and offers confirmable clauses instead
of a TODO shell. Remains functional in v2.x with a runtime warning.

Scaffold a `.qedspec` from an Anchor IDL JSON. (For Tier-0 interface
scaffolding from an IDL — program ID + handler signatures only — prefer
`interface`, which is more focused.) v2.10 dropped the SPEC.md
generators that previously lived behind `--from-spec` and the default
`--format md` path; `.qedspec` is QEDGen's front-door artifact and
parallel Markdown duplicates were drifting in practice.

```bash
$QEDGEN spec --idl target/idl/program.json
$QEDGEN spec --idl target/idl/program.json --output-dir ./formal_verification
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--idl` | Path | required | Anchor IDL JSON file |
| `--output-dir` | Path | `./formal_verification` | Output directory; `<idl-stem>.qedspec` is written inside |

### `adapt`
**DEPRECATED (slated for v3.0 removal).** `adapt` bundled two unrelated
jobs and both now have honest homes: scaffold mode is subsumed by
`qedgen probe --emit-spec-candidates --audit-dir` (elicitation-first —
confirmable, evidence-anchored hypotheses instead of TODO stubs, and the
same skeleton written as a byproduct), and attribute mode is
[`stamp`](#stamp) (same emission plus the recorded-verification gate).
Both modes remain functional in v2.x with a runtime warning.

Brownfield adapter for existing Anchor programs. Two modes:

- **Scaffold mode** (`--program <c>` only): parses `<c>/src/lib.rs`, finds
  the `#[program]` mod, walks each instruction to its handler body via
  forwarder classification, and emits a parseable `.qedspec` skeleton with
  TODO markers for state machine / requires / effect bodies.
- **Attribute mode** (`--program <c> --spec <s>`): given a filled-in spec,
  emits one `#[qed(verified, spec = ..., handler = ..., hash = ...,
  spec_hash = ...[, accounts = ..., accounts_file = ..., accounts_hash = ...])]`
  line per handler. Paste each above its handler `pub fn`; future body or
  spec edits trip `compile_error!` until you re-run `adapt --spec`.

Forwarder shapes the classifier handles end-to-end: Inline, free-fn
(`module::fn(args)` plus the two-stmt `<call>?; Ok(())` and `?`-tail
shapes), type-associated (`Type::method(ctx, args)` PascalCase prefix),
accounts-method (`ctx.accounts.method(args)`). Custom dispatcher patterns
fall through to `Unrecognized` — use `--handler` to point them at the real
implementation.

```bash
# Scaffold a starter spec from existing Anchor source
$QEDGEN adapt --program ./programs/my_program

# Write to disk instead of stdout
$QEDGEN adapt --program ./programs/my_program --out my_program.qedspec

# Emit #[qed] attributes for an existing spec
$QEDGEN adapt --program ./programs/my_program --spec my_program.qedspec

# Custom dispatcher handlers — point each at its actual implementation
$QEDGEN adapt --program ./programs/my_program \
  --handler dispatch=instructions::dispatch::handler \
  --handler ix2=instructions::ix2::run
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--program` | Path | required | Program crate (directory holding `Cargo.toml`, with `src/lib.rs` inside) |
| `--spec` | Path | - | Existing `.qedspec`. Switches to attribute-emit mode |
| `--out` | Path | stdout | Output path. In scaffold mode writes a `.qedspec`; in attribute mode writes a `// === handler … ===` report |
| `--handler` | `NAME=PATH` | - | Manually point an unrecognized handler at its actual implementation. Format: `<handler>=<rust_path>` where path is `module::sub::function` or just `function`. Repeatable. Wins over the classifier's choice for any outcome (Inline / FreeFn / Method / Unrecognized) |

### `stamp`
v2.44 — stamp `#[qed(verified, …)]` drift attributes for an
already-verified spec: the post-verification half of the old `adapt`,
under a name that says what it does. Emits one attribute per handler
(body hash + spec-block hash, plus the Accounts-struct seal when the
`Context<X>` struct is found) to paste above each `pub fn`; the
`qedgen-macros` proc macro recomputes both at compile time and fires
`compile_error!` on drift. Anchor-only (it round-trips through the
Anchor project parser to locate each handler body).

`stamp` runs **after** verification and proves nothing itself. Its one
new behavior over `adapt --spec` is the gate: it requires recorded
implementation-verified evidence — `.qed/verify-evidence.json`, written
by every `qedgen verify` run — with (a) a `spec_hash` matching the spec
being stamped byte-for-byte, (b) a `program_hash` matching the current
program source tree, and (c) at least one passing **implementation-bound**
backend (miri or a `kani_impl*.rs` harness). Checking/model-tested results
and bug-oriented `--probe-repros` are not eligible; an edited spec or
program invalidates the evidence until re-verified. So
the division of labor is: a source-bound backend establishes the
implementation claim, `stamp` freezes it, the compiler guards the
freeze.

```bash
# 1. verify with an implementation-bound backend (records evidence)
$QEDGEN verify --spec my_program.qedspec --program programs/my_program --kani --kani-path programs/my_program/src/kani_impl.rs

# 2. stamp — refuses unless the recorded evidence matches and is impl-bound
$QEDGEN stamp --program programs/my_program --spec my_program.qedspec
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--program` | Path | required | Program crate (directory holding `Cargo.toml`, with `src/lib.rs` inside) |
| `--spec` | Path | required | The verified `.qedspec` to stamp against |
| `--out` | Path | stdout | Output path for the `// === handler … ===` attribute report |
| `--handler` | `NAME=PATH` | - | Manually point an unrecognized handler at its implementation (same semantics as `adapt`) |
| `--evidence` | Path | `<spec_dir>/.qed/verify-evidence.json` | Override the verification-evidence path |

