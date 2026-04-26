# qedspec DSL Reference

The `.qedspec` file is the single source of truth for a program's formal
specification. QEDGen parses it (chumsky parser), validates it (`qedgen
check`), and generates all downstream artifacts: Anchor-compatible Rust
handlers, Lean proofs, Kani harnesses, proptest suites, CI workflows, and the
`#[qed(verified, spec, handler, spec_hash)]` drift attributes that tie
generated code back to the spec.

This reference covers the current (v2.7) grammar. Where the parser emits a
specific AST node shape that influences codegen (match, constructors, record
updates, `mul_div_*`), the node name is called out so you can follow the
transform into the Lean/Rust backends.

## What changed in v2.7

- **`+=` / `-=` default to checked semantics in Anchor handler bodies too**
  (v2.6 already did this for the Kani model). `pool += net` lowers to
  `self.pool = self.pool.checked_add(net).ok_or(ErrorCode::MathOverflow)?`
  — the pattern deployed programs use. Pre-v2.7 this lowered to
  `wrapping_add` which didn't match production.
- **Explicit per-effect arithmetic modifiers.** `+=! ` (saturating) and `+=?`
  (wrapping) opt out of the checked default when the handler *deliberately*
  wants those semantics. Same three tiers for `-=!` / `-=?`. See
  [Effect arithmetic](#effect-arithmetic) below.
- **`permissionless` handler marker.** New body clause that opts the handler
  out of the `no_access_control` P1 lint — for deliberately-unauthenticated
  handlers like `deposit_collateral`, `init_user`, donations. Declaring both
  `auth X` and `permissionless` on one handler fires a `contradictory_auth`
  P1. See [Handler clauses](#handler-clauses).
- **proptest `Arbitrary` strategies for records + unit-variant sums.** Each
  record type emits a `prop_compose! { fn arb_<Name>(…) -> <Name> { … } }`
  block; each unit-variant sum emits a `prop_oneof!` strategy. `arb_state`
  now correctly composes these instead of bailing to `0u64..=u64::MAX` for
  user-typed fields.
- **`Map[N] T` strategies use strict-length Vec + `TryInto`.** Works for any
  N; proptest's `prop::array::uniform*` combinators cap at 32 which wasn't
  enough for `Map[1024] Account`.
- **`--spec <dir>` accepts directories** across every CLI subcommand
  (`check`, `codegen`, `verify`, `spec`, `reconcile`). Multi-file specs
  already worked at the parser level; v2.7 locks it in as CLI-level
  contract with clearer errors on missing paths.

## What changed in v2.6

- **`+=` / `-=` default to checked arithmetic** in the generated Kani model.
  On overflow the transition returns `false` — mirroring the
  `checked_add(..).ok_or(MathOverflow)?` pattern deployed Anchor programs
  use. Proptest mode keeps wrapping arithmetic for bounded exploration.
  Before v2.6 the Kani model emitted bare `+=`, which flagged overflow on
  every unbounded pre-state (a spec-model artifact that didn't match real
  programs).
- **`state { fields }` sugar parses** as documented. Accepts
  comma- or newline-separated fields; desugars to `type State = { ... }`.
- **Single-line `accounts { a : signer, b : writable, ... }` parses.**
  Attribute commas and descriptor commas disambiguate via lookahead for
  `<ident> :`, so multi-line and compact forms both work.
- **`implies` lowers to valid Rust** (`(!a) || (b)`) in generated Kani and
  proptest property bodies. `forall` / `exists` in property bodies emit a
  `/* QEDGEN_UNSUPPORTED_QUANTIFIER */` marker — the property fn returns
  `true` (non-blocking) and the body should be lifted to harness-level
  `kani::any()` scaffolding instead.
- **Multi-variant ADTs with shared field names deduplicate** when flattened
  into the state model. First-variant wins on collisions. Proper
  enum+match codegen remains on the roadmap.
- **Default output layout changed.** `qedgen codegen --kani` / `--proptest`
  write to `./programs/tests/kani.rs` / `./programs/tests/proptest.rs` so
  `cargo kani --tests` and `cargo test --test proptest` resolve the
  program's `Cargo.toml` directly. `qedgen verify` matches. Pass
  `--kani-output` / `--proptest-output` to override.
- **`qedgen init` refuses to nest `formal_verification/formal_verification/`.**
  If run from inside an existing `formal_verification/` it bails with a
  fix hint (`--output-dir .`).
- **Generated `Cargo.toml` points at real deps.** `anchor-lang` +
  `anchor-spl` are the target framework; `qedgen-macros` is a git-dep
  at the release tag. Dependency lines start commented with a clear TODO
  so `cargo build` surfaces the contract instead of pretending the
  scaffold is complete.
- **Vacuous `verify_*_rejects_invalid` Kani harnesses are gone.** The
  `kani::assume(!(true))` footgun is fixed — harnesses now negate the
  full `guard + requires` conjunction, or skip entirely when the handler
  has no preconditions.
- **`verify_*_effects` is split per-field.** One `verify_X_effect_{field}`
  harness per effect, with `kani::solver(minisat)` when the RHS contains
  `*` or `/`. No more 20-minute cadical wedges on chained fee arithmetic.

## What changed in v2.5

- **`pragma <name> { ... }`** — platform-specific namespace. sBPF-specific
  constructs (`instruction`, `pubkey`, top-level `errors [...]`) now live
  only inside `pragma sbpf { ... }`. See *Pragmas* below.
- **`target` keyword removed.** Target is inferred from pragma presence —
  `pragma sbpf` → assembly target, absent → framework-flavored Rust.
  Framework selection happens at codegen time via `qedgen init
  --target {anchor,quasar,pinocchio}` (anchor and quasar ship in v2.9;
  pinocchio in v2.10+).
- **`assembly "..."` keyword removed.** Assembly source path is tooling
  config, not spec intent — pass `qedgen asm2lean --input <path>` or use
  the convention of `src/program.s` next to the spec.
- **`interface Name { ... }` + `call Target.handler(...)`** — declarative
  CPI contracts. See *Interface declarations*.
- **Multi-file specs.** `parse_spec_file` accepts a directory of `.qedspec`
  fragments (all declaring the same `spec Name`); fragments are merged
  deterministically in sorted-path order.
- **`let x = v in body` in expressions** — ML-style binding inside
  `ensures` / `requires` / effect RHS.

## File structure

```fsharp
spec ProgramName

// Top-level declarations (any order)
program_id "1111...1111"

const MAX_MEMBERS = 32

type State
  | Uninitialized
  | Active of { authority : Pubkey, balance : U64 }
  | Closed

interface Token { ... }     // callee contracts for CPI
pragma sbpf { ... }         // platform-specific namespace (opt-in; selects sBPF target)

handler initialize ...
property conservation ...
invariant backing ...
cover happy_path [...]
liveness settles ...
environment oracle { ... }
```

Comments: `//` line comments, `///` doc comments (attached to the next item).

## Project pinning — `.qed/config.json`

`qedgen init --name <name> --spec <path>` records the authored spec's
location in `.qed/config.json`:

```json
{
  "name": "escrow",
  "spec": "escrow.qedspec",           // path, relative to the project root
  "interfaces_dir": ".qed/interfaces" // vendored library interfaces
}
```

`qedgen check` / `qedgen codegen` with no `--spec` argument walk upward from
the current directory, find the nearest `.qed/config.json`, and resolve the
spec via the `spec` field. Explicit `--spec <path>` still overrides the
config — useful for scripts or one-off checks against a different spec.

Authored `.qedspec` files stay at the program root (visible, committed,
user-edited). Tool-managed library interfaces live under
`.qed/interfaces/` and are dropped there by `qedgen interface --idl
<path> --vendor`.

## Multi-file specs

`qedgen check --spec <path>` accepts either a single `.qedspec` file or a
directory of fragments. In directory mode, every `*.qedspec` under the path
(recursively) is parsed, and all fragments must declare the same `spec
Name`. Top items are merged in alphabetically-sorted source-path order —
both the merged `ParsedSpec` and every downstream artifact are
deterministic.

Convention-based layout (no new grammar required):

```fsharp
my-program/
  escrow.qedspec            # spec header + types + events + errors
  handlers/
    initialize.qedspec      # spec Escrow + handler initialize { ... }
    exchange.qedspec        # spec Escrow + handler exchange { ... }
    cancel.qedspec          # spec Escrow + handler cancel { ... }
  properties.qedspec        # spec Escrow + invariants + covers + liveness
  interfaces/               # scoped copies of library interfaces
    token.qedspec           # spec Escrow + interface Token { ... }
```

See `examples/rust/escrow-split/` for a concrete demo.

## Top-level declarations

### `spec`

Required header. Names the program.

```fsharp
spec Escrow
```

### `program_id`

On-chain program address.

```fsharp
program_id "11111111111111111111111111111111"
```

### `const`

Named integer constants. Underscores allowed for readability.

```fsharp
const MAX_MEMBERS = 32
const MAX_VAULT_TVL = 10_000_000_000_000_000
```

## Type system

### Records

```fsharp
// Flat record — no sum tag
type Account = {
  active        : U8,
  capital       : U128,
  reserved_pnl  : U128,
  pnl           : I128,
  fee_credits   : U128,
}
```

### Sum types (ADTs)

ML-style sum types with optional payloads. Variants without payload are bare
idents; payload variants use `of { ... }`.

```fsharp
// State ADT — variants with optional payloads
type State
  | Uninitialized
  | Active of {
      authority : Pubkey,
      V         : U128,
      I         : U128,
      F         : U128,
      accounts  : Map[MAX_ACCOUNTS] Account,
    }
  | Draining
  | Resetting
```

Sum types used as `Map` values are emitted as proper Lean `inductive`
declarations; state ADTs flatten for downstream transition codegen.

### Error types

`type Error | ...` is a flat enum with optional numeric code + description.

```fsharp
type Error
  | InvalidAmount
  | Unauthorized
  | InvalidDiscriminant = 1 "Discriminant is not REGISTER_MARKET"
  | InvalidLength       = 2 "Instruction data wrong length"
```

The legacy `errors [...]` sugar (below) still works and desugars to this.

### Type aliases

```fsharp
type AccountIdx = Fin[MAX_ACCOUNTS]
type Amount     = U128
```

`Fin[N]` is a bounded natural index domain of size `N` — the canonical shape
for subscripting a `Map[N] T` field.

### Parameterised and map types

Type expressions: `Pubkey`, `U8`, `U16`, `U64`, `U128`, `I128`, `Vec U64`,
`Option Pubkey`, `Map[N] T`, `Fin[N]`.

```fsharp
accounts : Map[MAX_ACCOUNTS] Account
slots    : Map[16] (Option Pubkey)
```

### `state` (sugar)

Shorthand for a single unnamed account record. Equivalent to a one-variant
record type.

```fsharp
state {
  balance : U64
  owner   : Pubkey
}
```

## PDA and events

### `pda`

PDA seed derivation. Seeds can be string literals or identifiers.

```fsharp
pda escrow ["escrow", initializer]
pda market ["base_mint", "quote_mint"]
pda loan ["loan", pool, borrower]
```

### `event`

Event type with typed fields.

```fsharp
event PoolInitialized { authority : Pubkey, rate : U64 }
event Deposited       { depositor : Pubkey, amount : U64 }
```

## Error declarations

At the top level, declare errors with `type Error | Variant | ...`:

```fsharp
type Error
  | Unauthorized
  | InvalidAmount
  | AlreadyClosed
```

Valued form (with codes + descriptions) uses the same ADT syntax:

```fsharp
type Error
  | InvalidAccountCount  = 1 "Invalid number of accounts"
  | InsufficientLamports = 7 "Sender has insufficient lamports"
```

The `errors [ ... ]` list-sugar is v2.5-restricted to inside `pragma sbpf
{ ... }` — see the sBPF section.

## Handlers

Handlers are the core building block — each one models a program instruction.
They use an ML-style signature with optional parameters and state transition.

### Syntax

```fsharp
/// Doc comment (optional, captured)
handler name (param1 : Type) (param2 : Type) : PreState -> PostState {
  // clauses
}
```

All parts of the signature are optional:

```fsharp
// Full signature
handler initialize (amount : U64) : State.Uninitialized -> State.Active { ... }

// No params
handler cancel : State.Open -> State.Closed { ... }

// No transition (pure guard program)
handler check_slippage { ... }

// No params, no transition
handler transfer_sol { ... }
```

### Handler clauses

| Clause | Purpose | Example |
|---|---|---|
| `auth` | Access control (signer must match field) | `auth authority` |
| `accounts { ... }` | Account descriptors | see below |
| `requires expr else Error` | Guard with error code | `requires amount > 0 else InvalidAmount` |
| `requires expr` | Guard without error code | `requires state.member_count > state.threshold` |
| `ensures expr` | Postcondition | `ensures state.balance >= 0` |
| `modifies [fields]` | Modification set | `modifies [balance, counter]` |
| `let name = expr` | Local binding | `let fee = amount * 3 / 100` |
| `effect { ... }` | State mutations | see below |
| `transfers { ... }` | Token transfer declarations | see below |
| `emits Event` | Event emission | `emits PoolInitialized` |
| `match { ... }` | Guarded branching | see below |
| `aborts_total` | Handler must reject on all guard failures | `aborts_total` |
| `invariant name` | Reference a global invariant | `invariant conservation` |
| `include schema` | Include a schema's clauses | `include base_validation` |
| `permissionless` | Opt out of the `no_access_control` lint (v2.7) | see below |
| `takes { ... }` | Parameters (sugar, prefer signature) | `takes amount : U64` |
| `on ident` | Instruction selector (sugar) | `on cancel` |
| `when ident` | Pre-state (sugar, prefer signature) | `when Open` |
| `then ident` | Post-state (sugar, prefer signature) | `then Closed` |

### `accounts` block

Declares the instruction's account context with attributes.

```fsharp
accounts {
  authority      : signer, writable
  vault          : writable, pda ["vault", authority]
  pool_vault     : writable, token, authority pool
  depositor_ta   : writable, type token
  mint           : readonly
  token_program  : program
  system_program : program
}
```

Account attributes:
- `signer` — must sign the transaction
- `writable` — mutable account
- `readonly` — immutable account
- `program` — program account
- `token` — SPL token account (shorthand)
- `type ident` — explicit account type
- `authority ident` — token authority reference
- `pda [seeds]` — PDA derivation inline

### `effect` block

State mutations using `:=` (assignment), `+=` (increment), `-=` (decrement).

```fsharp
effect {
  interest_rate       := rate
  total_deposits      += amount
  balance             -= fee
  counter             += 1
  accounts[i].capital += amount    // indexed LHS
  state               := .Active { authority, V := 0, I := 0, F := 0,
                                   accounts := empty_map }   // constructor RHS
}
```

Values on the RHS may be integer literals, qualified paths, arithmetic
expressions, constructor applications (`.Variant payload`), record literals,
record updates, `match … with`, or built-in helpers like `mul_div_floor`.

#### Effect arithmetic

As of v2.7, `+=` / `-=` default to **checked** semantics *in both the
generated Anchor handler bodies AND the Kani model*. On overflow the
transition returns `false` (Kani model) or `.ok_or(ErrorCode::MathOverflow)?`
(Anchor body) — mirroring what deployed Anchor programs do. Pre-v2.7 the
Anchor handler body lowered to `wrapping_add` which didn't match production.

Two explicit modifiers opt into alternative semantics when the handler
deliberately wants them:

| Operator | Semantics | Kani transition | Anchor handler body |
|---|---|---|---|
| `+=`  | checked (default)   | `checked_add(..)` — `return false` on overflow | `.checked_add(..).ok_or(MathOverflow)?` |
| `+=!` | saturating          | `saturating_add(..)` | `.saturating_add(..)` |
| `+=?` | wrapping            | `wrapping_add(..)`   | `.wrapping_add(..)` |

(`-=` / `-=!` / `-=?` follow the same pattern.) Proptest harness keeps
`wrapping_*` for default `+=` in the "explore the full state space" mode;
`+=!` / `+=?` are honored verbatim regardless of harness.

Example:

```fsharp
effect {
  // default checked — reverts on overflow, matches deployed checked_add
  pool += amount

  // explicit saturating — clamps to u64::MAX instead of erroring
  fees_collected +=! delta

  // explicit wrapping — cursor/index field where modular arithmetic is intended
  nonce +=? 1
}
```

### `permissionless` clause

Marks a handler as deliberately unauthenticated. Opts out of the P1
`no_access_control` lint that otherwise fires for any handler missing
`auth X`. Common on DeFi primitives where *anyone* is supposed to call
the handler — user slot inits, deposits, donations.

```fsharp
handler init_user (user : Pubkey) {
  permissionless
  effect { balance := 0 }
}

handler deposit_collateral (i : AccountIdx) (amount : U128)
    : State.Active -> State.Active {
  permissionless
  effect { accounts[i].capital += amount }
}
```

Declaring both `auth X` AND `permissionless` on the same handler fires
a `contradictory_auth` P1 — pick one.

### `transfers` block

Token transfer declarations with source, destination, amount, and authority.

```fsharp
transfers {
  from initializer_ta to escrow_ta amount deposit_amount authority initializer
  from escrow_ta to taker_ta amount initializer_amount authority escrow
}
```

### `match` clause (guarded branches)

A handler can end in a `match { | cond => outcome | ... }` clause that
desugars to multiple synthetic handlers, one per arm. Arms dispatch on the
first matching boolean condition. Outcomes are `abort ErrorName`,
`effect { ... }`, or an empty body (no-op / state unchanged). The final arm
is typically `_ => ...` as a catch-all.

```fsharp
handler liquidate (i : AccountIdx) : State.Active -> State.Active {
  auth authority
  accounts { authority : signer, vault : writable }

  requires state.accounts[i].active == 1 else SlotInactive

  match
    | state.accounts[i].capital + state.accounts[i].pnl >= 0 =>
        abort AccountHealthy
    | state.accounts[i].capital + state.accounts[i].pnl + state.I >= 0 =>
        effect { accounts[i].active := 0 }
    | _ =>
        abort BankruptPosition
}
```

Each arm becomes its own case in the generated transition function and its
own preservation obligation per property — vacuous cases close trivially,
the real cases need proofs.

### `schema` block

Reusable clause fragments. Handlers include them with `include`.

```fsharp
schema base_validation {
  requires accounts.count >= 3 else InvalidAccountCount
  requires user.data_len == 0 else UserDataLen
}

handler initialize : State.Uninitialized -> State.Active {
  include base_validation
  // additional clauses...
}
```

## Expressions

Guard expressions appear in `requires`, `ensures`, `property`, `invariant`,
`match` arms, and effect RHS positions. The full set of nodes parsed by the
chumsky grammar:

### Precedence (lowest to highest)

| Level | Operators |
|---|---|
| 1 | `or`, `\/` |
| 2 | `implies` |
| 3 | `and`, `/\` |
| 4 | `not` |
| 5 | `<=`, `>=`, `!=`, `<`, `>`, `==` |
| 6 | `+`, `-` |
| 7 | `*`, `/`, `%` |
| 8 | postfix: `.field`, `is .Variant` |
| 9 | atoms: literals, paths, calls, `old(...)`, quantifiers, `match`, constructors, record literals, parenthesized |

### Atoms

```fsharp
// Integers (underscores allowed)
42
10_000_000

// Booleans (used in propositional positions)
true
false

// Qualified paths with optional subscripts
amount
state.balance
Pool.Active
state.approval_count
state.accounts[i].capital

// Pre-state reference (only inside ensures)
old(state.balance)
old(state.accounts[i].pnl)

// Quantifiers — single binder
forall s : Pool.Active, s.total_deposits >= s.total_borrows
exists l : Loan.Active, l.collateral > 0

// Quantifiers — multi-binder (desugars to nested single-binder forms)
forall p1 p2 : Path, black_count(p1) == black_count(p2)

// Lowering notes (v2.6):
//   - `implies`  Lean: → ; Rust: `(!a) || (b)` — valid in property fn bodies.
//   - `forall` / `exists` in a `property` body emit a
//     `/* QEDGEN_UNSUPPORTED_QUANTIFIER */` marker in Rust and the
//     property fn returns `true`. Lift quantified universals to
//     harness-level `kani::any()` / proptest generators instead.

// Aggregate sum over a bounded index type
sum i : AccountIdx, state.accounts[i].capital

// The body of a `sum` is a plain expression (no conditionals). To count
// active entries when `active` is a 0/1 discriminator, sum the field
// directly — `if state.accounts[i].active == 1 then 1 else 0` is not
// grammatical. Example:
//   num_active == sum i : AccountIdx, state.accounts[i].active

// Parenthesized
(amount + fee) * rate

// Let-in binding — derive a value once, reference it by name.
// Lowers to Lean's `let x := v; body`, Rust `{ let x = v; body }`.
// Useful in ensures to name the quantity you're asserting about:
ensures let delta = old(state.balance) - state.balance in delta == amount
```

### Constructors, record literals, record updates

```fsharp
// Bare constructor — variant without payload
.Uninitialized

// Constructor with record-literal payload
.Active { authority, V := deposit_amount, I := 0, F := 0 }

// Record literal — useful as a Map-value RHS
{ active := 1, capital := amount, reserved_pnl := 0, pnl := 0, fee_credits := 0 }

// Record update — `{ base with f := v, ... }`
{ state.accounts[i] with capital := state.accounts[i].capital + amount }
```

Record updates are the compact form for touching a few fields of a sum-typed
record without restating the rest. Generated Lean renders this to native
`{ base with ... }` syntax so Mathlib's update lemmas apply.

### `is .Variant` — constructor test

Postfix `is .Variant` yields a `Prop` that's true when the LHS was built with
the given variant. Preferred over a full `match` when you only need the
discriminator check.

```fsharp
requires state.accounts[i] is .Active else SlotInactive
```

### `match … with` expression

An inline `match` expression yields a value (contrast with the handler-level
`match` clause above, which dispatches entire handler bodies). Arms name the
payload binder when destructuring.

```fsharp
let authority =
  match state with
    | .Active a => a.authority
    | .Draining => 0
    | .Resetting => 0
```

### `mul_div_floor` / `mul_div_ceil` — fixed-point helpers

```fsharp
requires mul_div_floor(size_q, exec_price, POS_SCALE) <= MAX_ACCOUNT_NOTIONAL
ensures state.F == old(state.F) + mul_div_ceil(fee, numerator, denominator)
```

Integer VMs (EVM, Solana sBPF) have no native fixed-point arithmetic and
users writing `(a * b) / d` by hand routinely get the widen-before-divide
step wrong. These helpers are built-in so the spec, the generated Rust
(promoted to `u256`/`U512` locally), and the Lean proof (using Mathlib
`mul_div_cancel` / `Nat.div_add_mod` lemmas) all agree on exact semantics.

### Function application

```fsharp
forall n : Node, left(n).key < n.key and n.key < right(n).key
forall n : Node, left(parent(n)) == n or right(parent(n)) == n
```

`f(a, b, ...)` parses as `Expr::App` with the function name left abstract.
Spec-level helpers (`parent`, `left`, `right`, `black_count`, …) are
declared as uninterpreted symbols in the generated Lean support module —
users can then prove properties about them with hand-written lemmas. Zero-arg
calls are rejected; bare identifiers parse as paths.

### Postfix `.field`

`.field` applies to any expression, not just bare paths:

```fsharp
left(n).key          // Field on the result of a function call
parent(n).color      // Chained
```

Bare dotted paths (`a.b.c`) still route to `Expr::Path`; `.field` on a
non-path base produces `Expr::Field`.

## Properties, invariants, cover, liveness

### `property` — quantified preservation properties

Generates per-handler sub-lemmas + a master inductive theorem. `preserved_by`
names the handler scope.

```fsharp
// Preserved by all handlers
property conservation :
  state.V >= (sum i : AccountIdx, state.accounts[i].capital)
           + (sum i : AccountIdx, state.accounts[i].reserved_pnl)
           + state.I + state.F
  preserved_by all

// Preserved by specific handlers
property vault_bounded :
  state.V <= MAX_VAULT_TVL
  preserved_by [deposit, top_up_insurance, deposit_fee_credits]

// Quantified over a type
property account_solvent :
  forall i : AccountIdx,
    state.accounts[i].active == 1
      implies state.accounts[i].capital + state.accounts[i].pnl >= 0
  preserved_by all
```

### `invariant` — named state invariants

Either a quantified expression (emitted as a proof obligation) or a string
description (kept as documentation for Lean and generated reports).

```fsharp
invariant collateral_backing :
  forall l : Loan.Active, l.collateral > 0

invariant conservation "total tokens preserved across initialize, exchange, cancel"

invariant pda_integrity "derived PDA matches provided account on initialize"
```

### `cover` — reachability

Declares that a sequence of handlers is reachable. Generates existential
proofs (Lean) and `kani::cover!` harnesses.

```fsharp
// One-liner trace
cover happy_path [initialize, exchange]
cover cancel_path [initialize, cancel]
cover bulk_insert [initialize, insert, insert, insert]

// Block form with trace and/or reachable clauses
cover cancel_available {
  trace [create_vault, propose, reject, cancel_proposal]
  reachable cancel_proposal when state.approval_count > 0
}
```

### `liveness` — bounded leads-to

From state A, state B is reachable within N steps via specified handlers.

```fsharp
liveness escrow_settles : State.Open ~> State.Closed via [exchange, cancel] within 1

liveness drain_completes : State.Draining ~> State.Active
  via [complete_drain, reset] within 2
```

## Environment (external state)

Declares external state mutations that happen outside handlers (oracle feeds,
clock ticks, admin pokes). Properties that reference mutated fields must hold
across those mutations too.

```fsharp
environment interest_rate_change {
  mutates interest_rate : U64
  constraint interest_rate > 0
}
```

## Pragmas

`pragma <name> { <items> }` wraps platform-specific declarations in a named
namespace. Pragmas keep the core DSL platform-agnostic: constructs that only
make sense for one target live inside their pragma block, not at the top
level.

```fsharp
pragma sbpf { ... }    // sBPF assembly programs
```

The presence of `pragma sbpf` also selects the assembly target — no explicit
`target` keyword needed. Absent → framework-flavored Rust (the
default qedspec target). Framework selection happens at codegen
time via `qedgen init --target {anchor,quasar,...}`, not in the
spec — the same `.qedspec` drives every framework target.

Body whitelist for `pragma sbpf`: `const`, `pubkey`, `instruction`, `errors`.
Core DSL items (`handler`, `type`, `property`, `invariant`, `interface`, …)
stay at the top level.

## Interface declarations

Contracts for programs you CPI into. Uniform surface across three tiers:

- **Tier 0** — shape only: `program_id`, handler discriminant, accounts, args.
  Generated by `qedgen interface --idl target/idl/program.json`.
- **Tier 1** — hand-authored `requires` / `ensures` on handlers. The
  caller gets real hypotheses at each `call` site, bound into
  proptest and Kani obligations from the same source. (When the caller
  also has a Lean side, Tier-1 clauses become Lean hypotheses there
  too — but Lean is optional for CPI contracts.)
- **Tier 2** — the interface is a real imported `.qedspec` (v2.6+). No
  `interface` keyword needed — every handler in the imported spec is public.

**Drift caution on Tier-1 growth.** Keep `requires`/`ensures` clauses
**semantic** — invariants that describe program-visible behavior —
rather than **implementation-tracking** — clauses that pin specific fee
math, byte layouts, or error codes that the upstream may change. An
`ensures amount > 0` survives upstream patches. An `ensures fee ==
amount * 30 / 10_000` rots the first time the upstream bumps a fee
parameter. When in doubt about whether a clause is durable, prefer
fewer clauses plus a Kani harness against the deployed binary over
more clauses that need rewriting with every upstream release.

```fsharp
interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"

  upstream {
    package       "spl-token"
    version       "4.0.3"
    binary_hash   "sha256:..."      // deployed .so, authoritative
    verified_with ["proptest", "kani"]  // honest — "lean" only when proven
    verified_at   "2026-04-18"
  }

  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable, type token
      to        : writable, type token
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}
```

See `docs/design/spec-composition.md` §2 for the tier model and
`interfaces/spl_token.qedspec` for the canonical SPL Token interface.

### `call Target.handler(name = expr, ...)` clause

Inside a handler body, a `call` is a terminal statement — the uniform CPI
surface. Not an expression, not nestable.

```fsharp
handler exchange : State.Open -> State.Closed {
  call Token.transfer(
    from      = taker_ta,
    to        = initializer_ta,
    amount    = taker_amount,
    authority = taker,
  )
  emits EscrowExchanged
}
```

`qedgen check` emits `[shape_only_cpi]` on calls whose target declares no
`ensures` — the visible gap between "my Rust compiles" and "my program is
verified."

## sBPF-specific constructs (inside `pragma sbpf { ... }`)

Everything in this section lives inside `pragma sbpf { ... }`. The pragma
wrapping is mandatory in v2.5; the grammar rejects these items at the top
level. See `examples/sbpf/dropset/dropset.qedspec` for a full example.

```fsharp
pragma sbpf {
  pubkey SYSTEM_PROGRAM_ID [0, 0, 0, 0]
  pubkey RENT_SYSVAR_ID    [0x6a7d51..., 0xb8b9f5..., 0xc01b2f..., 0xb85e22...]

  errors [
    InvalidDiscriminant        = 1  "Discriminant is not REGISTER_MARKET",
    InvalidInstructionLength   = 2  "Instruction data is not 1 byte",
  ]

  instruction register_market { ... }
}
```

### `pubkey NAME [u64, u64, u64, u64]`

32-byte pubkeys as four `U64` chunks — the form the sBPF program will compare
against in registers.

### `errors [ NAME = CODE "msg", ... ]`

Error list used for exit-code reasoning in sBPF properties. Anchor-style
programs use `type Error | ...` at the top level instead — this sugar is
sBPF-only.

### `instruction NAME { ... }` block

Groups discriminant, entry point, layouts, guards, and properties for a
single sBPF instruction. Any of the sub-clauses is optional.

```fsharp
instruction register_market {
  discriminant 0
  entry 0

  const QUOTE_MINT_OFFSET = 32

  errors [InvalidDiscriminant = 1, InvalidLength = 2]

  input_layout {
    discriminant : U8     @0  "Instruction discriminant"
    base_mint    : Pubkey @1
    quote_mint   : Pubkey @33
  }

  insn_layout {
    opcode : U8  @0
    amount : U64 @1
  }

  guard check_discriminant {
    checks discriminant == 0
    error InvalidDiscriminant
    fuel 8
  }

  guard check_length {
    checks instruction_data_len == 1
    error InvalidLength
    fuel 4
  }

  property rejects_wrong_discriminant {
    expr discriminant != 0 implies exit_code == 1
    scope guards
    exit 1
  }
}
```

### `input_layout { ... }` and `insn_layout { ... }`

Field declarations of the form `name : Type @ offset "doc"` (description
optional). `input_layout` describes the input buffer; `insn_layout` describes
the instruction-data register's memory layout.

### `guard NAME { ... }` block

A single validation check. `checks` is the guard predicate, `error` names the
failure code, `fuel` bounds the sBPF execution steps needed to close the
goal.

```fsharp
guard check_discriminant {
  checks discriminant == 0
  error InvalidDiscriminant
  fuel 8
}
```

### sBPF `property NAME { ... }` block

sBPF property blocks can carry additional clauses that drive the sBPF
WP-based proof backend:

| Clause | Purpose | Example |
|---|---|---|
| `expr` | Property expression | `expr amount > 0` |
| `preserved_by` | Handler scope | `preserved_by all` or `preserved_by [h1, h2]` |
| `scope guards` | Scope to all guard blocks | `scope guards` |
| `scope [names]` | Scope to specific guards/instructions | `scope [check_disc, check_len]` |
| `flow name from seeds [...]` | Data flow from PDA seeds | `flow market from seeds [base_mint, quote_mint]` |
| `flow name through [...]` | Data flow through registers | `flow amount through [r2, r3]` |
| `cpi program target { ... }` | Expected CPI envelope | see below |
| `after all guards` | Property asserted after all guards pass | `after all guards` |
| `exit N` | Expected exit code | `exit 0` |

```fsharp
property rejects_wrong_account_count {
  expr accounts.count != 3 implies exit_code == 1
  scope guards
  exit 1
}

property accepts_valid_transfer {
  expr all_guards_pass implies exit_code == 0
  scope [transfer_sol]
  after all guards
  exit 0
}
```

### CPI envelope block (inside sBPF `property`)

```fsharp
property transfer_cpi_correct {
  cpi system_program transfer {
    accounts [sender, recipient, system_program]
    data amount
  }
  after all guards
  exit 0
}
```

## `#[qed(verified, ...)]` drift attribute

QEDGen codegen stamps each generated Rust handler with a `#[qed]` attribute
that binds it to its spec contract. At compile time the proc macro reads the
referenced spec, re-hashes the handler block, and emits `compile_error!` on
mismatch.

```rust
#[qed(verified,
      spec      = "../../percolator.qedspec",
      handler   = "deposit",
      hash      = "3f2c9a81b0d5e4f7",   // body content hash
      spec_hash = "7e1a48d93b2c0f65")]  // spec-handler content hash
pub fn deposit(ctx: Context<Deposit>, i: u64, amount: u128) -> Result<()> {
    // ... user-filled body
}
```

Args:
- `spec` — path (relative to the `.rs` file) to the `.qedspec` source
- `handler` — handler name inside the spec
- `hash` — SHA-256-hex16 of the function signature + body (set by
  `qedgen check --drift --update-hashes`)
- `spec_hash` — SHA-256-hex16 of the spec-side `handler <name> { ... }`
  block text (set by codegen and by `qedgen reconcile --update-hashes`)

See SKILL.md **Step 4d — drift reconciliation** for the full agent workflow
and `references/cli.md` for `qedgen reconcile` / `qedgen check --drift`.

## `qedgen check` coverage

Prints a verification matrix showing which handlers are covered by which
properties.

```fsharp
$ qedgen check --spec multisig.qedspec --coverage

handler           threshold_bounded votes_bounded
-------------------------------------------------
create_vault              Y               Y
propose                   Y               Y
approve                   Y               Y
reject                    Y               Y
execute                   Y               Y
cancel_proposal           Y               Y
remove_member             Y               -

Coverage: 100% (7/7 handlers covered by at least one property)
```

Use `--json` for machine-readable output.

## What `qedgen codegen` generates

From a `.qedspec`, codegen produces:

- **Rust handler skeleton** (default, Anchor-compatible): program crate,
  `guards.rs` (always regenerated), `src/instructions/*.rs` (user-owned,
  scaffolded once), `src/lib.rs` (user-owned, scaffolded once),
  `errors.rs`, entrypoint. Generated `Cargo.toml` includes
  TODO-commented `anchor-lang` / `anchor-spl` / `qedgen-macros` deps —
  uncomment after reviewing.
- **Lean proofs** (`--lean`): `Spec.lean` (always regenerated) +
  `Proofs.lean` (bootstrapped once — user-owned tactic bodies)
- **Kani harnesses** (`--kani`): BMC harnesses for each property + overflow
  detection
- **Proptest suites** (`--proptest`): randomised testing of all properties
- **Unit tests** (`--test`): Rust unit tests for handler logic
- **Integration tests** (`--integration`): in-process SVM integration tests
- **CI workflows** (`--ci`): GitHub Actions workflow for the verification
  waterfall

`qedgen codegen --spec program.qedspec --all` generates everything. See
`references/cli.md` for the scaffold-once policy, drift attributes, and the
require-git guard.

## qedguards Lean macro

For direct Lean proof authoring on sBPF programs, the `qedguards` macro
generates guard-chain infrastructure. This is the Lean-side companion to
`.qedspec` `instruction` blocks.

```lean
import QEDGen.Solana.Guards

qedguards Dropset where
  prog: progAt
  chunks progAt_0 progAt_1 progAt_2

  errors
    E_DISCRIMINANT 100
    E_QUOTE_MINT   200

  offsets
    DISCRIMINANT_OFFSET "0"
    QUOTE_MINT_OFFSET   "0x20"

  guard P1 "wrong discriminant"
    offset: DISCRIMINANT_OFFSET
    expected: DISCRIMINANT_REGISTER_MARKET
    fuel 8
    error E_DISCRIMINANT
    proof auto

  guard P9 "quote mint mismatch chunk 0"
    offset: QUOTE_MINT_C0_OFFSET
    expected_reg: EXPECTED_QUOTE_MINT_C0_OFFSET
    fuel 12
    error E_QUOTE_MINT
    proof phased [phase1_prefix 4, phase2_ptr_arith 3, phase3_read 5]
```

### qedguards clauses

| Clause | Purpose |
|---|---|
| `prog:` | Program definition or fetch function |
| `chunks` | Sub-program chunk defs for dsimp |
| `entry:` | Entry PC (optional, for non-zero entrypoints) |
| `r1:` / `r2:` | Register bindings (optional) |
| `errors` | Error code constants (`NAME value`) |
| `offsets` | Offset constants (`NAME "intValue"`) |
| `guard NAME "description"` | Guard declaration |
| `fuel N` | Execution fuel for this guard |
| `error NAME` | Error code on failure |
| `proof auto` | Auto-generate `wp_exec` proof |
| `proof phased [...]` | Phase decomposition with fuel per phase |
| `proof sorry` | Stub only (default) |

### What qedguards generates

- Offset constants + `@[simp] theorem ea_NAME` lemmas
- Error-code abbreviations
- `Spec` structure with rejection theorem types
- For `proof auto`: full `wp_exec` proofs with hypothesis lifting
- For `proof phased`: main composition theorem + phase `sorry` stubs

## qedbridge Lean macro

Refinement bridge connecting qedspec (abstract state) to sBPF bytecode
(concrete memory).

```lean
import QEDGen.Solana.Bridge

qedbridge Escrow where
  input: r1
  insn: r2        -- optional: instruction data register
  entry: 0        -- optional: entry PC
  fuel: 100

  layout
    maker     Pubkey at 0
    amount    U64   at 32
    status    U8    at 40

  status_encoding at 40
    Open      0
    Completed 1
    Cancelled 2

  operations
    cancel    0x01
    exchange  0x02 takes: taker_amount U64
```

### What qedbridge generates

- Memory layout constants (byte offsets)
- Status encoding/decoding functions
- `encodeState : State -> Nat -> Mem -> Prop` (state-memory correspondence)
- `decodeState : Nat -> Mem -> State` (functional read)
- Per-operation refinement theorems (sorry stubs):
  - `OpName.refines`: if abstract transition succeeds, execution exits 0 and
    encodes the new state
  - `OpName.rejects`: if abstract transition fails, execution exits non-zero
