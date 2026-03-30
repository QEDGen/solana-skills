# Dropset RegisterMarket Verification Spec v1.0

Dropset is a Solana on-chain order book (sBPF assembly). RegisterMarket creates a new
market account as a PDA derived from `[base_mint_pubkey, quote_mint_pubkey]`, ensuring
each token pair maps to exactly one market.

Reference: https://docs.dropset.io/program/markets.html#registration

## 0. Security Goals

1. **Input validation**: The program MUST reject malformed inputs (wrong discriminant,
   wrong account count, wrong instruction length) with distinct error codes.
2. **Account uniqueness**: Every account MUST be checked for a `NON_DUP_MARKER` (255)
   to prevent account aliasing attacks.
3. **Account freshness**: User and market accounts MUST have `data_len == 0` to prevent
   re-initialization.
4. **PDA integrity**: The provided market pubkey MUST match the PDA derived from
   `[base_mint, quote_mint]` seeds.
5. **System program identity**: The System Program and Rent sysvar accounts MUST match
   their well-known addresses.

## 1. State Model

The program operates on raw sBPF memory. Key regions:

- **Input buffer** (`r1`): Serialized account infos (user at offset 0, market at ~10344, etc.)
- **Instruction data** (`r2`): 1-byte discriminant; `r2-8` holds instruction data length
- **Stack frame** (`r10-664..r10`): PDA seeds, CPI data, account metas

### Account Layout (per account in input buffer)

| Offset | Field         | Size  |
|--------|--------------|-------|
| 0      | duplicate    | 1     |
| 1      | is_signer    | 1     |
| 2      | is_writable  | 1     |
| 8      | address      | 32    |
| 40     | owner        | 32    |
| 80     | data_len     | 8     |
| 88     | data         | var   |

## 2. Operations

### 2.1 RegisterMarket

**Discriminant**: 0

**Instruction data**: 1 byte (discriminant only)

**Required accounts** (10):

| Index | Account            | Constraints                    |
|-------|--------------------|--------------------------------|
| 0     | User               | signer, writable, data_len = 0 |
| 1     | Market             | PDA-signed, writable, data_len = 0, not duplicate |
| 2     | BaseMint           | not duplicate                  |
| 3     | QuoteMint          | not duplicate                  |
| 4     | SystemProgram      | not duplicate, known address   |
| 5     | RentSysvar         | not duplicate, known address   |
| 6     | BaseTokenProgram   | —                              |
| 7     | QuoteTokenProgram  | —                              |
| 8     | BaseVault          | —                              |
| 9     | QuoteVault         | —                              |

**Preconditions**:
1. `n_accounts >= 10`
2. `insn_len == 1`
3. `discriminant == 0`

**Effects**:
1. Validate all preconditions and account guards (see §3)
2. Derive market PDA from `[base_mint_pubkey, quote_mint_pubkey]`
3. Verify derived PDA matches provided market pubkey (4 × 8-byte chunks)
4. Compute rent-exempt lamports from Rent sysvar data
5. Build CreateAccount CPI (System Program) with market PDA as signer
6. Invoke `sol_invoke_signed_c`

**Postconditions**:
- Market account exists, owned by Dropset program
- Market account has space for `MarketHeader`

## 3. Formal Properties

### 3.1 Error Dispatch Correctness

**P1**: For all `(inputAddr insnAddr mem disc)`,
if `readU8 mem insnAddr ≠ 0` (discriminant ≠ RegisterMarket)
then `exitCode = 1` (E_INVALID_DISCRIMINANT).

### 3.2 Account Count Validation

**P2**: For all `(inputAddr insnAddr mem nAccounts)`,
if discriminant = 0 and `nAccounts < 10`
then `exitCode = 3` (E_INVALID_NUMBER_OF_ACCOUNTS).

### 3.3 Instruction Length Validation

**P3**: For all `(inputAddr insnAddr mem nAccounts insnLen)`,
if discriminant = 0, nAccounts ≥ 10, and `insnLen ≠ 1`
then `exitCode = 2` (E_INVALID_INSTRUCTION_LENGTH).

### 3.4 User Account Freshness

**P4**: For all `(inputAddr insnAddr mem ... userDataLen)`,
if prior checks pass and `userDataLen ≠ 0`
then `exitCode = 4` (E_USER_HAS_DATA).

### 3.5 Market Account Uniqueness

**P5**: For all `(inputAddr insnAddr mem ... mktDup)`,
if prior checks pass, user data = 0, and `mktDup ≠ 255`
then `exitCode = 5` (E_MARKET_ACCOUNT_IS_DUPLICATE).

### 3.6 Market Account Freshness

**P6**: For all `(inputAddr insnAddr mem ... mktDataLen)`,
if prior checks pass, market not duplicate, and `mktDataLen ≠ 0`
then `exitCode = 6` (E_MARKET_HAS_DATA).

### 3.7 Base Mint Uniqueness (future)

**P7**: For all inputs where prior checks pass and `baseMintDup ≠ 255`,
then `exitCode = 7` (E_BASE_MINT_IS_DUPLICATE).

### 3.8 Quote Mint Uniqueness (future)

**P8**: For all inputs where prior checks pass and `quoteMintDup ≠ 255`,
then `exitCode = 8` (E_QUOTE_MINT_IS_DUPLICATE).

### 3.9 PDA Integrity (future)

**P9**: For all inputs where prior checks pass and derived PDA ≠ provided market pubkey,
then `exitCode = 9` (E_INVALID_MARKET_PUBKEY).

## 4. Trust Boundary

- **Trusted**: Solana runtime (account serialization, PDA derivation via `sol_try_find_program_address`),
  System Program (`CreateAccount` CPI), SPL Token programs
- **Verified**: Input validation guards, error dispatch, account uniqueness/freshness checks
- **Axiomatic**: Memory read functions (`readU8`, `readU64`) faithfully model sBPF load instructions

## 5. Error Codes

| Code | Name                          | Verified |
|------|-------------------------------|----------|
| 1    | InvalidDiscriminant           | **P1**   |
| 2    | InvalidInstructionLength      | **P3**   |
| 3    | InvalidNumberOfAccounts       | **P2**   |
| 4    | UserHasData                   | **P4**   |
| 5    | MarketAccountIsDuplicate      | **P5**   |
| 6    | MarketHasData                 | **P6**   |
| 7    | BaseMintIsDuplicate           | Open     |
| 8    | QuoteMintIsDuplicate          | Open     |
| 9    | InvalidMarketPubkey           | Open     |
| 10   | SystemProgramIsDuplicate      | Open     |
| 11   | InvalidSystemProgramPubkey    | Open     |
| 12   | RentSysvarIsDuplicate         | Open     |
| 13   | InvalidRentSysvarPubkey       | Open     |

## 6. Verification Results

| Property | Status       | Proof                                        |
|----------|-------------|----------------------------------------------|
| P1       | **Verified** | `rejects_invalid_discriminant`               |
| P2       | **Verified** | `rejects_invalid_account_count`              |
| P3       | **Verified** | `rejects_invalid_instruction_length`         |
| P4       | **Verified** | `rejects_user_has_data`                      |
| P5       | **Verified** | `rejects_market_duplicate`                   |
| P6       | **Verified** | `rejects_market_has_data`                    |
| P7       | **Open**     |                                              |
| P8       | **Open**     |                                              |
| P9       | **Open**     |                                              |
