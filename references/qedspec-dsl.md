# qedspec DSL Reference

## qedspec macro

```lean
import QEDGen.Solana.Spec
open QEDGen.Solana.SpecDSL

qedspec Escrow where
  program_id: "11111111111111111111111111111111"

  state
    maker         : Pubkey
    src_mint      : Pubkey
    dst_mint      : Pubkey
    amount        : U64
    taker_amount  : U64

  event ExchangeEvent { maker : Pubkey, taker : Pubkey, amount : U64 }

  errors: Unauthorized, InvalidAmount, AlreadyCompleted

  operation cancel
    doc: "Cancel the escrow and return tokens to maker"
    who: maker
    when: Open
    then: Cancelled
    effect: transfer escrow_token -> maker
    context: {
      maker : Signer, mut
      escrow_token : TokenAccount, mut
    }

  operation exchange
    doc: "Execute the exchange"
    who: taker
    when: Open
    then: Completed
    takes: taker_amount U64
    guard: "taker_amount >= s.taker_amount"
    effect: transfer escrow_token -> taker, transfer taker_token -> maker
    emits: ExchangeEvent
    calls: TOKEN_PROGRAM_ID DISC_TRANSFER(escrow_token writable, dest writable, authority signer)
    context: {
      taker : Signer, mut
      escrow_token : TokenAccount, mut
      taker_token : TokenAccount, mut
      system_program : Program, System
    }

  invariant token_conservation
    over: [escrow_token, maker_token, taker_token]
    sum amount is constant across exchange

  invariant lifecycle
    Open -> Completed | Cancelled
    terminal: Completed, Cancelled

  property bounded "s.amount <= U64_MAX"
    preserved_by: exchange

  trust
    spl_token, solana_runtime, anchor_framework
```

## What the macro generates

- `State` structure with `DecidableEq`
- `Status` enum from `when`/`then` values (with `DecidableEq`)
- Transition functions (`Option State`) per operation
- `Operation` inductive + `applyOp` dispatcher
- Theorem **signatures** with `sorry` bodies — one per operation x property, one per invariant
- CPI correctness theorem stubs

**What the macro does NOT generate**: proof bodies. You fill those in the proof-writing step.

## DSL vocabulary

| Clause | Purpose | Example |
|---|---|---|
| `state` | Declare program state fields | `amount : U64` |
| `operation` | Define an operation | `operation cancel` |
| `who:` | Access control (signer must match field) | `who: maker` |
| `when:` / `then:` | Lifecycle transition | `when: Open then: Cancelled` |
| `takes:` | Operation parameters | `takes: amount U64` |
| `guard:` | Domain constraint (generates proof obligation) | `guard: "amount > 0"` |
| `effect:` | State mutation | `effect: balance add amount` |
| `calls:` | CPI declaration | `calls: TOKEN_PROGRAM_ID DISC_TRANSFER(...)` |
| `emits:` | Event emission | `emits: ExchangeEvent` |
| `context:` | Account context (for codegen) | `authority : Signer, mut` |
| `property` | Named property with `preserved_by` | `property bounded "expr"` |
| `invariant` | Global invariant | `invariant conservation` |
| `trust` | Trust boundary | `trust spl_token, solana_runtime` |
| `event` | Event type declaration | `event E { field : Type }` |
| `errors:` | Error enum for codegen | `errors: Unauthorized, InvalidAmount` |
| `program_id:` | On-chain program ID | `program_id: "111..."` |
| `doc:` | Documentation string | `doc: "Cancel the escrow"` |

## qedguards DSL (for sBPF programs)

For sBPF assembly programs, use `qedguards` instead of `qedspec`:

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

## qedbridge DSL (refinement bridge)

Connects qedspec (abstract state) to sBPF bytecode (concrete memory):

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
