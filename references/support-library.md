# Support Library API Reference

After `import QEDGen.Solana` or individual modules.

## Types

| Type | Definition | Module |
|---|---|---|
| `Pubkey` | `{ c0 c1 c2 c3 : Nat }` (four LE U64 chunks) | Account |
| `Account` | `{ key authority : Pubkey, balance : Nat, writable : Bool }` | Account |
| `U64` | `Nat` | Account |
| `U128` | `Nat` | Account |
| `U8` | `Nat` | Account |
| `Lifecycle` | `open \| closed` (with DecidableEq) | State |
| `AccountMeta` | `{ pubkey : Pubkey, isSigner isWritable : Bool }` | Cpi |
| `CpiInstruction` | `{ programId : Pubkey, accounts : List AccountMeta, data : List Nat }` | Cpi |

## Constants

**Program IDs:** `SYSTEM_PROGRAM_ID`, `TOKEN_PROGRAM_ID`, `TOKEN_2022_PROGRAM_ID`, `ASSOCIATED_TOKEN_PROGRAM_ID`, `MEMO_PROGRAM_ID`, `COMPUTE_BUDGET_PROGRAM_ID`, `STAKE_PROGRAM_ID`

**SPL Token discriminators:** `DISC_TRANSFER`, `DISC_TRANSFER_CHECKED`, `DISC_MINT_TO`, `DISC_MINT_TO_CHECKED`, `DISC_BURN`, `DISC_BURN_CHECKED`, `DISC_CLOSE_ACCOUNT`, `DISC_APPROVE`, `DISC_APPROVE_CHECKED`, `DISC_REVOKE`, `DISC_SET_AUTHORITY`, `DISC_FREEZE_ACCOUNT`, `DISC_THAW_ACCOUNT`, `DISC_INITIALIZE_MINT`, `DISC_INITIALIZE_MINT2`, `DISC_INITIALIZE_ACCOUNT`, `DISC_INITIALIZE_ACCOUNT3`, `DISC_SYNC_NATIVE`

**System Program discriminators:** `DISC_SYS_CREATE_ACCOUNT`, `DISC_SYS_ASSIGN`, `DISC_SYS_TRANSFER`, `DISC_SYS_ALLOCATE`

**ATA discriminators:** `DISC_ATA_CREATE`, `DISC_ATA_CREATE_IDEMPOTENT`, `DISC_ATA_RECOVER_NESTED`

**Bounds:** `U8_MAX`, `U16_MAX`, `U32_MAX`, `U64_MAX`, `U128_MAX`

## Functions

| Function | Signature | Purpose |
|---|---|---|
| `findByKey` | `List Account -> Pubkey -> Option Account` | Find account by key |
| `findByAuthority` | `List Account -> Pubkey -> Option Account` | Find account by authority |
| `canWrite` | `Pubkey -> Account -> Prop` | Check writable + authority match |
| `targetsProgram` | `CpiInstruction -> Pubkey -> Prop` | CPI targets expected program |
| `accountAt` | `CpiInstruction -> Nat -> Pubkey -> Bool -> Bool -> Prop` | CPI account at index matches |
| `hasDiscriminator` | `CpiInstruction -> List Nat -> Prop` | CPI data starts with discriminator |
| `hasNAccounts` | `CpiInstruction -> Nat -> Prop` | CPI has expected account count |
| `wellFormed` | `CpiInstruction -> Prop` | CPI has accounts and data |
| `closes` | `Lifecycle -> Lifecycle -> Prop` | Lifecycle: open -> closed |
| `valid_u64` | `Nat -> Prop` | Value fits in u64 (and u8/u16/u32/u128) |

## Key lemmas

| Lemma | Statement |
|---|---|
| `closes_is_closed` | `closes before after -> after = closed` |
| `closes_was_open` | `closes before after -> before = open` |
| `closed_irreversible` | `lifecycle = closed -> lifecycle != open` |
| `closed_cannot_close` | `¬closes closed after` |
| `valid_u64_zero` | `valid_u64 0` |
| `Pubkey.ne_iff` | Two pubkeys differ iff at least one chunk differs |
| `Pubkey.ext'` | Four chunk equalities imply pubkey equality |
| `find_map_update_other` | Finding by different authority after update returns original |
| `find_map_update_same` | Finding by same authority after update returns updated |

## Arithmetic helpers (requires `--mathlib`)

After `import QEDGen.Solana.Arithmetic` and `open QEDGen.Solana.Arithmetic`:

### Widening multiplication

| Lemma | Signature |
|---|---|
| `mul_u64_fits_u128` | `valid_u64 a -> valid_u64 b -> valid_u128 (a * b)` |
| `mul_u32_fits_u64` | `valid_u32 a -> valid_u32 b -> valid_u64 (a * b)` |

### Widening addition

| Lemma | Signature |
|---|---|
| `add_u64_fits_u128` | `valid_u64 a -> valid_u64 b -> valid_u128 (a + b)` |
| `add_u32_fits_u64` | `valid_u32 a -> valid_u32 b -> valid_u64 (a + b)` |

### Casting

| Lemma | Signature |
|---|---|
| `u64_as_u128` | `valid_u64 n -> valid_u128 n` |
| `u32_as_u64` | `valid_u32 n -> valid_u64 n` |

### Checked arithmetic

| Lemma | Signature |
|---|---|
| `checked_add_u64` | `valid_u64 a -> valid_u64 b -> a + b <= U64_MAX -> valid_u64 (a + b)` |
| `checked_add_u128` | `valid_u128 a -> valid_u128 b -> a + b <= U128_MAX -> valid_u128 (a + b)` |
| `checked_mul_u64` | `valid_u64 a -> valid_u64 b -> a * b <= U64_MAX -> valid_u64 (a * b)` |
| `checked_mul_u128` | `valid_u128 a -> valid_u128 b -> a * b <= U128_MAX -> valid_u128 (a * b)` |

### Division

| Lemma | Signature |
|---|---|
| `div_preserves_u128` | `valid_u128 n -> d > 0 -> valid_u128 (n / d)` |
| `div_preserves_u64` | `valid_u64 n -> d > 0 -> valid_u64 (n / d)` |

### Fixed-point (DeFi patterns)

| Lemma | Signature |
|---|---|
| `fixed_point_u128` | `valid_u64 price -> valid_u64 amount -> decimals > 0 -> valid_u128 (price * amount / decimals)` |
| `fixed_point_two_step` | Two-step: `(a*b/d1)*c/d2` with intermediate bound proof |

### Accumulators

| Lemma | Signature |
|---|---|
| `accum_u128_add_u64` | `valid_u128 total -> valid_u64 delta -> total + delta <= U128_MAX -> valid_u128 (total + delta)` |
| `accum_u128_sub` | `valid_u128 total -> delta <= total -> valid_u128 (total - delta)` |

### Generic bounded multiplication

| Lemma | Signature |
|---|---|
| `mul_bounded_u128` | `a <= bound_a -> b <= bound_b -> bound_a * bound_b <= U128_MAX -> valid_u128 (a * b)` |
| `mul_u64_bounded_u128` | `valid_u64 a -> b <= bound_b -> U64_MAX * bound_b <= U128_MAX -> valid_u128 (a * b)` |
