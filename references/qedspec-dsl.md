# qedspec DSL Reference

The `.qedspec` file is the single source of truth for a program's formal specification.
QEDGen parses it (pest PEG grammar), validates it (`qedgen check`), and generates all
downstream artifacts: Quasar Rust code, Lean proofs, Kani harnesses, proptest suites,
and CI workflows.

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

Declares the compilation target. Affects which codegen backends and sBPF-specific
constructs are available.

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

Named pubkey as a byte array.

```
pubkey SYSTEM_PROGRAM_ID [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                          0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6]
```

## Type system

### `type` (algebraic data types)

ML-style sum types with optional fields, error codes, and descriptions.

```
// State ADT — variants with optional fields
type Pool
  | Uninitialized
  | Active of {
      authority      : Pubkey,
      total_deposits : U64,
      total_borrows  : U64,
      interest_rate  : U64,
    }
  | Paused

// Multiple types per spec
type Loan
  | Empty
  | Active of { borrower : Pubkey, amount : U64, collateral : U64 }
  | Liquidated

// Error type — variants with optional code + description
type Error
  | InvalidAmount
  | Unauthorized
  | InvalidDiscriminant = 1 "Discriminant is not REGISTER_MARKET"
  | InvalidLength       = 2 "Instruction data wrong length"
```

Type expressions: `Pubkey`, `U8`, `U16`, `U64`, `U128`, `Vec U64`, `Option Pubkey`.

### `state` (sugar)

Shorthand for a single unnamed account type. Equivalent to a `type` with one variant.

```
state {
  balance : U64
  owner   : Pubkey
}
```

### `lifecycle` (sugar)

Shorthand for declaring lifecycle variant names.

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

## Error declarations

### `errors` (sugar)

Simple error list or valued error list. Prefer `type Error | ...` for new specs.

```
// Simple list
errors [Unauthorized, InvalidAmount, AlreadyClosed]

// Valued list (sBPF compat)
errors [
  InvalidAccountCount = 1 "Invalid number of accounts",
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
  interest_rate  := rate
  total_deposits += amount
  balance        -= fee
  counter        += 1
}
```

Values can be integers or qualified identifiers (e.g., `deposit_amount`, `state.balance`).

### `transfers` block

Token transfer declarations with source, destination, amount, and authority.

```
transfers {
  from initializer_ta to escrow_ta amount deposit_amount authority initializer
  from escrow_ta to taker_ta amount initializer_amount authority escrow
}
```

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

## Properties

Quantified properties with a preservation clause. Generates per-handler sub-lemmas.

```
// Preserved by all handlers
property conservation :
  state.V >= state.C_tot + state.I
  preserved_by all

// Preserved by specific handlers
property vault_bounded :
  state.V <= MAX_VAULT_TVL
  preserved_by [deposit, top_up_insurance]

// Quantified over a type
property pool_solvency :
  forall s : Pool.Active, s.total_deposits >= s.total_borrows
  preserved_by all

// Simple bound
property threshold_bounded :
  state.threshold <= state.member_count and state.threshold > 0
  preserved_by all
```

**Generated Lean output** — per-handler sub-lemmas + master inductive theorem:

```lean
-- Per-handler sub-lemma (user proves this)
theorem conservation_preserved_by_deposit (s s' : State) ...
    (h_inv : conservation s) (h : depositTransition s signer amount = some s') :
    conservation s' := sorry

-- Master theorem (auto-proven by case split)
theorem conservation_inductive ... := by
  cases op with
  | deposit amount => exact conservation_preserved_by_deposit s s' signer amount h_inv h
  | withdraw amount => exact conservation_preserved_by_withdraw s s' signer amount h_inv h
```

Operations with `+=` effects also generate **auto-overflow obligations**:

```lean
theorem deposit_overflow_safe (s s' : State) ...
    (h_valid : valid_u128 s.V ∧ valid_u128 s.C_tot ∧ valid_u128 s.I)
    (h : depositTransition s signer amount = some s') :
    valid_u128 s'.V ∧ valid_u128 s'.C_tot ∧ valid_u128 s'.I := sorry
```

## Invariants

Named invariants — either a quantified expression or a string description.

```
// Quantified
invariant collateral_backing :
  forall l : Loan.Active, l.collateral > 0

// String description (for documentation / Lean comment)
invariant conservation "total tokens preserved across initialize, exchange, cancel"

invariant pda_integrity "derived PDA matches provided account on initialize"
```

## Cover (reachability)

Declares that a sequence of handlers is reachable. Generates existential proofs (Lean) and `kani::cover!` harnesses.

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

## Liveness (bounded leads-to)

One-liner declaring that from state A, state B is reachable within N steps via specified handlers.

```
liveness escrow_settles : State.Open ~> State.Closed via [exchange, cancel] within 1

liveness drain_completes : State.Draining ~> State.Active via [complete_drain, reset] within 2

liveness proposal_resolves : State.HasProposal ~> State.Active via [execute, cancel_proposal] within 1
```

## Environment (external state)

Declares external state changes that happen outside of handlers (e.g., oracle price updates). Properties referencing mutated fields must still hold.

```
environment interest_rate_change {
  mutates interest_rate : U64
  constraint interest_rate > 0
}
```

## Guard expressions

Guards are the expression language used in `requires`, `ensures`, `property`, `invariant`, and other clauses.

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
| 8 | atoms: integers, identifiers, `old(...)`, quantifiers, parenthesized |

### Atoms

```
// Integers (underscores allowed)
42
10_000_000

// Qualified identifiers
amount
state.balance
Pool.Active
state.approval_count

// Pre-state reference (only inside ensures)
old(state.balance)

// Quantifiers
forall s : Pool.Active, s.total_deposits >= s.total_borrows
exists l : Loan.Active, l.collateral > 0

// Parenthesized
(amount + fee) * rate
```

### Examples

```
// Simple comparison
amount > 0

// Compound with logical operators
threshold > 0 and threshold <= member_count

// ML-style logical operators
amount > 0 /\ collateral > 0

// Arithmetic
state.V + amount <= MAX_VAULT_TVL
state.approval_count + state.rejection_count < state.member_count

// Implication
sender.lamports < amount implies exit_code == 7

// Negation
not (state.is_closed)

// Quantified
forall s : Pool.Active, s.total_deposits >= s.total_borrows
```

## sBPF-specific constructs

For `target assembly` specs, additional constructs model sBPF program structure.

### `instruction` block

Groups discriminant, entry point, layouts, guards, and properties for a single sBPF instruction.

```
instruction register_market {
  discriminant 0
  entry 0

  const QUOTE_MINT_OFFSET = 32

  errors [InvalidDiscriminant = 1, InvalidLength = 2]

  input_layout {
    discriminant : U8  @0 "Instruction discriminant"
    base_mint    : Pubkey @1
    quote_mint   : Pubkey @33
  }

  insn_layout {
    opcode : U8 @0
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

### sBPF `property` block (top-level or inside `instruction`)

Properties for sBPF programs support additional clauses for low-level verification.

```
property all_guards_reject_invalid {
  expr forall i : Error, register_market rejects with error i when guard i fails
  preserved_by all
}

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

sBPF property clauses:

| Clause | Purpose | Example |
|---|---|---|
| `expr` | Property expression | `expr amount > 0` |
| `preserved_by` | Handler scope | `preserved_by all` or `preserved_by [h1, h2]` |
| `scope guards` | Scope to all guard blocks | `scope guards` |
| `scope [names]` | Scope to specific guards/instructions | `scope [check_disc, check_len]` |
| `flow name from seeds [...]` | Data flow from PDA seeds | `flow market from seeds [base_mint, quote_mint]` |
| `flow name through [...]` | Data flow through registers | `flow amount through [r2, r3]` |
| `cpi program target { ... }` | CPI correctness | see below |
| `after all guards` | Property holds after all guards pass | `after all guards` |
| `exit code` | Expected exit code | `exit 0` |

### CPI block (inside sBPF property)

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

### Guard block (inside `instruction`)

```
guard check_discriminant {
  checks discriminant == 0
  error InvalidDiscriminant
  fuel 8
}
```

## `qedgen check` coverage

Prints a verification matrix showing which handlers are covered by which properties.

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

- **Lean proofs** (`--lean`): State structures, transition functions, theorem stubs with `sorry`
- **Kani harnesses** (`--kani`): BMC harnesses for each property + overflow detection
- **Proptest suites** (`--proptest`): Randomized testing of all properties
- **Quasar Rust code** (`--rust`): Program skeleton with Anchor-compatible handlers
- **Unit tests** (`--unit-test`): Rust unit tests for handler logic
- **Integration tests** (`--integration-test`): QuasarSVM integration tests
- **CI workflows** (`--ci`): GitHub Actions workflow for the verification waterfall

`qedgen codegen --spec program.qedspec --all` generates everything.

## qedguards Lean macro

For direct Lean proof authoring on sBPF programs, the `qedguards` macro generates guard
chain infrastructure. This is the Lean-side companion to `.qedspec` instruction blocks.

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
| `proof auto` | Auto-generate wp_exec proof |
| `proof phased [...]` | Phase decomposition with fuel per phase |
| `proof sorry` | Stub only (default) |

### What qedguards generates

- Offset constants + `@[simp] theorem ea_NAME` lemmas
- Error code abbreviations
- `Spec` structure with rejection theorem types
- For `proof auto`: full wp_exec proofs with hypothesis lifting
- For `proof phased`: main composition theorem + phase sorry stubs

## qedbridge Lean macro

Refinement bridge connecting qedspec (abstract state) to sBPF bytecode (concrete memory).

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
  - `OpName.refines`: if abstract transition succeeds, execution exits 0 and encodes new state
  - `OpName.rejects`: if abstract transition fails, execution exits non-zero
