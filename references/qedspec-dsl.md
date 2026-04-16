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

  property bounded :
    s.amount <= U64_MAX
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
| `aborts_if` | Reject condition with named error (v2.0) | `aborts_if amount == 0 with InvalidAmount` |
| `cover` | Reachability trace (v2.0) | `cover happy_path { trace [...] }` |
| `liveness` | Bounded leads-to (v2.0) | `liveness drain { from A leads_to B ... }` |
| `environment` | External state mutation (v2.0) | `environment oracle { mutates price : U64 }` |

## v2.0 blocks

### `aborts_if` clause

Declares a condition under which an operation must reject. Generates a negative theorem (Lean: `= none`, Kani: `assert!(!transition(...))`).

```
operation withdraw {
  guard state.C_tot >= amount
  aborts_if state.C_tot < amount with InsufficientFunds

  effect { V -= amount; C_tot -= amount }
}
```

Multiple `aborts_if` clauses per operation are allowed (e.g., different error codes for different failure modes).

**Lean output:**
```lean
theorem withdraw_aborts_if_InsufficientFunds (s : State) (signer : Pubkey) (amount : Nat)
    (h : s.C_tot < amount) : withdrawTransition s signer amount = none := sorry
```

**Kani output:**
```rust
#[kani::proof]
fn verify_withdraw_aborts_if_InsufficientFunds() {
    let mut s = State { ... kani::any() ... };
    let amount: u128 = kani::any();
    kani::assume(s.C_tot < amount);
    assert!(!withdraw(&mut s, amount));
}
```

### `cover` block (reachability)

Declares that a sequence of operations is reachable from some initial state. Generates existential proofs (Lean) and `kani::cover!` harnesses (Kani).

```
cover happy_path {
  trace [deposit, withdraw]
}

cover cancel_always_available {
  reachable cancel_proposal when state.approval_count > 0
}
```

**Lean output:**
```lean
theorem cover_happy_path : ∃ (s0 : State) (signer : Pubkey),
    ∃ (v0 : Nat), ∃ (s1 : State), depositTransition s0 signer v0 = some s1 ∧
      withdrawTransition s1 signer ≠ none := sorry
```

### `liveness` block (leads-to)

Declares that from a given lifecycle state, another state is reachable within a bounded number of steps via specified operations.

```
liveness drain_completes {
  from Draining
  leads_to Active
  via [complete_drain, reset]
  within 2
}
```

**Lean output:**
```lean
theorem liveness_drain_completes (s : State) (signer : Pubkey)
    (h : s.status = .Draining) :
    ∃ ops, ops.length ≤ 2 ∧ ∀ s', applyOps s signer ops = some s' → s'.status = .Active := sorry
```

**Kani output:** Multi-step harness with non-deterministic operation selection in a loop.

### `environment` block (external state)

Declares external state changes that can happen outside of operations (e.g., oracle price updates). Properties referencing mutated fields must still hold.

```
environment interest_rate_change {
  mutates interest_rate : U64
  constraint interest_rate > 0
}
```

**Lean output:**
```lean
theorem pool_solvency_under_interest_rate_change (s : PoolState) (new_interest_rate : Nat)
    (h_c0 : new_interest_rate > 0)
    (h_inv : pool_solvency s) :
    pool_solvency { s with interest_rate := new_interest_rate } := sorry
```

### `qedgen check --coverage` command

Prints a verification matrix showing which operations are covered by which properties.

```
$ qedgen check --spec multisig.qedspec --coverage

operation         threshold_bounded approvals_bounded
-----------------------------------------------------
create_vault              Y                 Y
propose                   Y                 Y
approve                   Y                 Y
execute                   Y                 Y
cancel_proposal           Y                 Y
remove_member             Y                 -

Coverage: 100% (6/6 operations covered by at least one property)
```

Use `--json` for machine-readable output.

### Proof decomposition (v2.0)

Properties with `preserved_by` now generate per-operation sub-lemmas instead of a monolithic theorem:

```lean
-- Per-op sub-lemma (user proves this)
theorem conservation_preserved_by_deposit (s s' : State) ...
    (h_inv : conservation s) (h : depositTransition s signer amount = some s') :
    conservation s' := sorry

-- Master theorem (auto-proven by case split)
theorem conservation_inductive ... := by
  cases op with
  | deposit amount => exact conservation_preserved_by_deposit s s' signer amount h_inv h
  | withdraw amount => exact conservation_preserved_by_withdraw s s' signer amount h_inv h
```

### Auto-overflow obligations (v2.0)

Operations with `add` effects automatically generate overflow safety obligations:

```lean
theorem deposit_overflow_safe (s s' : State) ...
    (h_valid : valid_u128 s.V ∧ valid_u128 s.C_tot ∧ valid_u128 s.I)
    (h : depositTransition s signer amount = some s') :
    valid_u128 s'.V ∧ valid_u128 s'.C_tot ∧ valid_u128 s'.I := sorry
```

Kani overflow harnesses use symbolic inputs and rely on Kani's built-in overflow detection.

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
