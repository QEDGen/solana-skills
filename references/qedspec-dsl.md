# qedspec DSL Reference

The `.qedspec` file is the single source of truth for a program's formal
specification. QEDGen parses it (chumsky parser, as of v2.3), validates it
(`qedgen check`), and generates all downstream artifacts: Quasar Rust code,
Lean proofs, Kani harnesses, proptest suites, CI workflows, and the
`#[qed(verified, spec, handler, spec_hash)]` drift attributes that tie
generated code back to the spec.

This reference covers the current (v2.3) grammar. Where the parser emits a
specific AST node shape that influences codegen (match, constructors, record
updates, `mul_div_*`), the node name is called out so you can follow the
transform into the Lean/Rust backends.

## File structure

```
spec ProgramName

// Top-level declarations (any order)
target quasar              // or: target assembly
program_id "1111...1111"
assembly "src/program.s"   // sBPF only

const MAX_MEMBERS = 32

type State
  | Uninitialized
  | Active of { authority : Pubkey, balance : U64 }
  | Closed

handler initialize ...
property conservation ...
invariant backing ...
cover happy_path [...]
liveness settles ...
environment oracle { ... }
```

Comments: `//` line comments, `///` doc comments (attached to the next item).

## Top-level declarations

### `spec`

Required header. Names the program.

```
spec Escrow
```

### `target`

Declares the compilation target. Affects which codegen backends and
sBPF-specific constructs are available.

```
target quasar       // Anchor/Quasar Rust programs (default)
target assembly     // sBPF assembly programs
```

### `program_id`

On-chain program address.

```
program_id "11111111111111111111111111111111"
```

### `assembly`

Path to the sBPF assembly source (assembly target only).

```
assembly "src/program.s"
```

### `const`

Named integer constants. Underscores allowed for readability.

```
const MAX_MEMBERS = 32
const MAX_VAULT_TVL = 10_000_000_000_000_000
```

### `pubkey`

Named pubkey as four `U64` chunks (sBPF target). Matches how an sBPF program
materializes a 32-byte pubkey in registers.

```
pubkey SYSTEM_PROGRAM_ID [0, 0, 0, 0]
pubkey RENT_SYSVAR_ID    [0x6a7d51..., 0xb8b9f5..., 0xc01b2f..., 0xb85e22...]
```

## Type system

### Records

```
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

```
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

```
type Error
  | InvalidAmount
  | Unauthorized
  | InvalidDiscriminant = 1 "Discriminant is not REGISTER_MARKET"
  | InvalidLength       = 2 "Instruction data wrong length"
```

The legacy `errors [...]` sugar (below) still works and desugars to this.

### Type aliases

```
type AccountIdx = Fin[MAX_ACCOUNTS]
type Amount     = U128
```

`Fin[N]` is a bounded natural index domain of size `N` — the canonical shape
for subscripting a `Map[N] T` field.

### Parameterised and map types

Type expressions: `Pubkey`, `U8`, `U16`, `U64`, `U128`, `I128`, `Vec U64`,
`Option Pubkey`, `Map[N] T`, `Fin[N]`.

```
accounts : Map[MAX_ACCOUNTS] Account
slots    : Map[16] (Option Pubkey)
```

### `state` (sugar)

Shorthand for a single unnamed account record. Equivalent to a one-variant
record type.

```
state {
  balance : U64
  owner   : Pubkey
}
```

### `lifecycle` (sugar)

Shorthand for declaring lifecycle variant names with no payloads.

```
lifecycle [Open, Closed, Cancelled]
```

## PDA and events

### `pda`

PDA seed derivation. Seeds can be string literals or identifiers.

```
pda escrow ["escrow", initializer]
pda market ["base_mint", "quote_mint"]
pda loan ["loan", pool, borrower]
```

### `event`

Event type with typed fields.

```
event PoolInitialized { authority : Pubkey, rate : U64 }
event Deposited       { depositor : Pubkey, amount : U64 }
```

## Error declarations (sugar)

`errors [...]` is a legacy sugar for a flat error list. Prefer
`type Error | ...`; both desugar to the same internal representation.

```
// Simple list
errors [Unauthorized, InvalidAmount, AlreadyClosed]

// Valued list (sBPF compat)
errors [
  InvalidAccountCount  = 1 "Invalid number of accounts",
  InsufficientLamports = 7 "Sender has insufficient lamports",
]
```

## Handlers

Handlers are the core building block — each one models a program instruction.
They use an ML-style signature with optional parameters and state transition.

### Syntax

```
/// Doc comment (optional, captured)
handler name (param1 : Type) (param2 : Type) : PreState -> PostState {
  // clauses
}
```

All parts of the signature are optional:

```
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
| `takes { ... }` | Parameters (sugar, prefer signature) | `takes amount : U64` |
| `on ident` | Instruction selector (sugar) | `on cancel` |
| `when ident` | Pre-state (sugar, prefer signature) | `when Open` |
| `then ident` | Post-state (sugar, prefer signature) | `then Closed` |

### `accounts` block

Declares the instruction's account context with attributes.

```
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

```
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

### `transfers` block

Token transfer declarations with source, destination, amount, and authority.

```
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

```
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

```
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

```
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

// Aggregate sum over a bounded index type
sum i : AccountIdx, state.accounts[i].capital

// Parenthesized
(amount + fee) * rate
```

### Constructors, record literals, record updates

```
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

```
requires state.accounts[i] is .Active else SlotInactive
```

### `match … with` expression

An inline `match` expression yields a value (contrast with the handler-level
`match` clause above, which dispatches entire handler bodies). Arms name the
payload binder when destructuring.

```
let authority =
  match state with
    | .Active a => a.authority
    | .Draining => 0
    | .Resetting => 0
```

### `mul_div_floor` / `mul_div_ceil` — fixed-point helpers

```
requires mul_div_floor(size_q, exec_price, POS_SCALE) <= MAX_ACCOUNT_NOTIONAL
ensures state.F == old(state.F) + mul_div_ceil(fee, numerator, denominator)
```

Integer VMs (EVM, Solana sBPF) have no native fixed-point arithmetic and
users writing `(a * b) / d` by hand routinely get the widen-before-divide
step wrong. These helpers are built-in so the spec, the generated Rust
(promoted to `u256`/`U512` locally), and the Lean proof (using Mathlib
`mul_div_cancel` / `Nat.div_add_mod` lemmas) all agree on exact semantics.

### Function application

```
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

```
left(n).key          // Field on the result of a function call
parent(n).color      // Chained
```

Bare dotted paths (`a.b.c`) still route to `Expr::Path`; `.field` on a
non-path base produces `Expr::Field`.

## Properties, invariants, cover, liveness

### `property` — quantified preservation properties

Generates per-handler sub-lemmas + a master inductive theorem. `preserved_by`
names the handler scope.

```
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

```
invariant collateral_backing :
  forall l : Loan.Active, l.collateral > 0

invariant conservation "total tokens preserved across initialize, exchange, cancel"

invariant pda_integrity "derived PDA matches provided account on initialize"
```

### `cover` — reachability

Declares that a sequence of handlers is reachable. Generates existential
proofs (Lean) and `kani::cover!` harnesses.

```
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

```
liveness escrow_settles : State.Open ~> State.Closed via [exchange, cancel] within 1

liveness drain_completes : State.Draining ~> State.Active
  via [complete_drain, reset] within 2
```

## Environment (external state)

Declares external state mutations that happen outside handlers (oracle feeds,
clock ticks, admin pokes). Properties that reference mutated fields must hold
across those mutations too.

```
environment interest_rate_change {
  mutates interest_rate : U64
  constraint interest_rate > 0
}
```

## sBPF-specific constructs

For `target assembly` specs, additional top-level blocks model sBPF program
structure. See `examples/sbpf/dropset/dropset.qedspec` for a comprehensive
example that combines most of them.

### `target assembly` and `assembly "path"`

```
target assembly
assembly "src/dropset.s"
```

### `pubkey NAME [u64, u64, u64, u64]`

32-byte pubkeys as four `U64` chunks — the form the sBPF program will compare
against in registers.

```
pubkey SYSTEM_PROGRAM_ID [0, 0, 0, 0]
pubkey RENT_SYSVAR_ID    [0x6a7d51..., 0xb8b9f5..., 0xc01b2f..., 0xb85e22...]
```

### `errors [ NAME = CODE "msg", ... ]`

Top-level error list used for exit-code reasoning in sBPF properties. Equivalent
to the Rust-side `type Error | ...`.

```
errors [
  InvalidDiscriminant        = 1  "Discriminant is not REGISTER_MARKET",
  InvalidInstructionLength   = 2  "Instruction data is not 1 byte",
  InvalidNumberOfAccounts    = 3  "Fewer than 10 accounts provided",
]
```

### `instruction NAME { ... }` block

Groups discriminant, entry point, layouts, guards, and properties for a
single sBPF instruction. Any of the sub-clauses is optional.

```
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

```
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

```
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

```
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

```
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

- **Quasar Rust skeleton** (default): program crate, `guards.rs` (always
  regenerated), `src/instructions/*.rs` (user-owned, scaffolded once),
  `src/lib.rs` (user-owned, scaffolded once), `errors.rs`, entrypoint
- **Lean proofs** (`--lean`): `Spec.lean` (always regenerated) +
  `Proofs.lean` (bootstrapped once — user-owned tactic bodies)
- **Kani harnesses** (`--kani`): BMC harnesses for each property + overflow
  detection
- **Proptest suites** (`--proptest`): randomised testing of all properties
- **Unit tests** (`--test`): Rust unit tests for handler logic
- **Integration tests** (`--integration`): QuasarSVM integration tests
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
