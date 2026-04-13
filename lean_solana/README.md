# QEDGen.Solana — Lean 4 Support Library for Solana Verification

Standalone Lean 4 library providing types, axioms, and the `qedspec` DSL for formally verifying Solana programs.

## Quick Start

```bash
lake build                         # Build library
lake env lean test_lemmas.lean     # Test axioms
lake env lean test_spec.lean       # Test qedspec DSL
```

## Modules

### QEDGen.Solana.Account

Types and axioms for Solana account modeling.

| Definition | Type | Description |
|---|---|---|
| `Pubkey` | `structure` | 4×U64 little-endian chunks, `DecidableEq` |
| `U64` | `Nat` | Unbounded Nat alias (bounds via Valid) |
| `U8` | `Nat` | Unbounded Nat alias |
| `Account` | `structure` | key, authority, balance, writable |
| `canWrite` | `Pubkey → Account → Prop` | Authority + writable check |
| `findByKey` | `List Account → Pubkey → Option Account` | Lookup by key |
| `findByAuthority` | `List Account → Pubkey → Option Account` | Lookup by authority |

**Axioms** (trusted, not proven):
- `find_map_pred_preserved` / `find_map_update_other` / `find_map_update_same` — list update properties
- `find_by_key_map_update_other` / `find_by_key_map_update_same` — key-based lookup after update

### QEDGen.Solana.Cpi

CPI (Cross-Program Invocation) envelope model — structure + verification predicates.

| Definition | Type | Description |
|---|---|---|
| `AccountMeta` | `structure` | pubkey, isSigner, isWritable |
| `CpiInstruction` | `structure` | programId, accounts, data |
| `targetsProgram` | `CpiInstruction → Pubkey → Prop` | Program ID match |
| `accountAt` | `CpiInstruction → Nat → Pubkey → Bool → Bool → Prop` | Account at index with flags |
| `hasDiscriminator` | `CpiInstruction → List Nat → Prop` | Instruction data prefix match |
| `hasNAccounts` | `CpiInstruction → Nat → Prop` | Account count check |
| `wellFormed` | `CpiInstruction → Prop` | Non-empty data + at least 1 account |

**Constants:**
- Program IDs: `TOKEN_PROGRAM_ID`, `SYSTEM_PROGRAM_ID`, `TOKEN_2022_PROGRAM_ID`, `ASSOCIATED_TOKEN_PROGRAM_ID`, `MEMO_PROGRAM_ID`, `COMPUTE_BUDGET_PROGRAM_ID`, `STAKE_PROGRAM_ID`
- SPL Token discriminators (`List Nat`): `DISC_TRANSFER`, `DISC_BURN`, `DISC_MINT_TO`, `DISC_APPROVE`, `DISC_CLOSE_ACCOUNT`, `DISC_TRANSFER_CHECKED`, etc.
- System Program discriminators (`List Nat`): `DISC_SYS_CREATE_ACCOUNT`, `DISC_SYS_TRANSFER`, `DISC_SYS_ASSIGN`, `DISC_SYS_ALLOCATE`
- ATA discriminators (`List Nat`): `DISC_ATA_CREATE`, `DISC_ATA_CREATE_IDEMPOTENT`, `DISC_ATA_RECOVER_NESTED`

All discriminators are `List Nat` — they map directly to `CpiInstruction.data`.

### QEDGen.Solana.State

Lifecycle state machine for one-shot patterns (escrow, auction, etc.).

| Definition | Type | Description |
|---|---|---|
| `Lifecycle` | `inductive` | `open \| closed` |
| `closes` | `Lifecycle → Lifecycle → Prop` | Valid close transition |
| `closed_irreversible` | `theorem` | Closed state is terminal |
| `closes_is_closed` | `theorem` | Post-state of close is closed |
| `closes_was_open` | `theorem` | Pre-state of close was open |
| `closed_cannot_close` | `theorem` | Cannot close from closed |

### QEDGen.Solana.Valid

Numeric bounds predicates for overflow/underflow safety.

| Definition | Type | Description |
|---|---|---|
| `U8_MAX` .. `U128_MAX` | `Nat` | Bit-width max values |
| `valid_u8` .. `valid_u128` | `Nat → Prop` | `n ≤ MAX` |
| `valid_u64_zero` | `theorem` | `valid_u64 0` |

### QEDGen.Solana.Spec

The `qedspec` DSL — a single declarative block that generates Lean definitions and theorem stubs.

```lean
qedspec Escrow where
  state
    maker : Pubkey
    offered : U64

  operation initialize
    who: maker
    when: Uninitialized
    then: Open

  operation exchange
    who: taker
    when: Open
    then: Complete
    calls: TOKEN_PROGRAM_ID DISC_TRANSFER(src writable, dst writable, auth signer)

  invariant conservation "total tokens preserved"
```

**Generates per-program:**
- `inductive Status` — inferred from `when`/`then` values
- `structure State` — fields + status

**Generates per-operation:**
- `<op>Transition : State → Pubkey → Option State` — signer + lifecycle guard
- `<op>.access_control` theorem stub — signer must match
- `<op>.state_machine` theorem stub — lifecycle pre/post
- `<op>.u64_bounds` theorem stub — U64 fields stay in range (if any U64 fields)

**Generates per-operation with `calls:`:**
- `<op>CpiContext` structure — account pubkeys
- `<op>_build_cpi : <op>CpiContext → CpiInstruction` — constructs the CPI
- `<op>.cpi_correct` theorem stub — program + accounts + discriminator match

**Account flags:** `readonly`, `writable`, `signer`, `signer_writable`

All theorem stubs contain `sorry` — agents fill them, `lake build` enforces completeness.

### QEDGen.Solana.SBPF

Low-level sBPF virtual machine model for binary-level verification. Submodules: ISA, Memory, Execute, Tactic, WPTactic, Pubkey, Patterns, Region. See SKILL.md for sBPF proof workflow.

## Trust Boundary

These axioms model the boundary between what we verify and what we trust:

- **Verified**: program logic (authorization, conservation, state machines, arithmetic, CPI structure)
- **Trusted**: SPL Token implementation, Solana runtime, CPI mechanics, Anchor framework

CPI verification is structural: correct program, correct accounts with correct flags, correct discriminator. Parameter serialization is trusted (SDK territory).

## Testing

```bash
lake build                         # Build all modules
lake env lean test_lemmas.lean     # Axiom smoke tests (Account, Cpi, State)
lake env lean test_spec.lean       # qedspec DSL behavioral tests
```

`test_spec.lean` proves properties of generated code — not just `#check` existence. A regression in code generation will cause a proof failure.

## Adding New Axioms

1. Add to the appropriate module in `QEDGen/Solana/`
2. Document the trust assumption with a comment
3. Export in the `QEDGen.Solana` namespace (via `QEDGen.lean` or the module's export block)
4. Add a test in `test_lemmas.lean`
5. `lake build && lake env lean test_lemmas.lean`

## Files

```
lean_solana/
├── lakefile.lean                  Build config
├── QEDGen.lean                    Root export (imports all modules)
├── QEDGen/Solana/
│   ├── Account.lean               Pubkey, Account, lookup axioms
│   ├── Cpi.lean                   CpiInstruction, predicates, constants
│   ├── State.lean                 Lifecycle state machine
│   ├── Valid.lean                 Numeric bounds predicates
│   ├── Spec.lean                  qedspec DSL macro + elaborator
│   └── SBPF/                     sBPF VM model (ISA, Memory, Execute, Tactic)
├── test_lemmas.lean               Axiom tests
└── test_spec.lean                 DSL behavioral tests
```
