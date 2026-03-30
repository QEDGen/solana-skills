# Transfer Program Verification Spec v1.0

A hand-written sBPF assembly program that validates inputs and invokes the
System Program's Transfer instruction via CPI.

## 0. Security Goals

1. **Input Validation**: The program MUST reject invalid inputs with distinct error codes.
2. **Balance Safety**: The program MUST reject when the sender has insufficient lamports.
3. **Happy Path**: The program MUST accept when all validation checks pass and the sender has sufficient balance.

## 1. State Model

The program reads from the Solana input buffer (pointed to by r1):

| Field | Offset | Type | Description |
|---|---|---|---|
| `num_accounts` | `0x00` | u64 LE | Number of accounts (must be 3) |
| `sender_lamports` | `0x50` | u64 LE | Sender lamport balance |
| `sender_data_length` | `0x58` | u64 LE | Sender account data length (must be 0) |
| `recipient_dup_marker` | `0x2868` | u8 | Recipient duplicate marker (must be 0xff) |
| `recipient_data_length` | `0x28b8` | u64 LE | Recipient account data length (must be 0) |
| `system_program_dup_marker` | `0x50c8` | u8 | System program duplicate marker (must be 0xff) |
| `instruction_data_length` | `0x7938` | u64 LE | Instruction data length (must be 8) |
| `transfer_amount` | `0x7940` | u64 LE | SOL amount to transfer |

No mutable state in the validation prefix. After validation, the program constructs and invokes a CPI.

## 2. Operations

### 2.1 Entrypoint

**Signers**: Sender account (enforced by CPI, not modeled here)

**Preconditions**: r1 points to a valid Solana input buffer.

**Effects** (validation prefix):
1. Load `num_accounts`; reject with error 1 if ≠ 3
2. Load `sender_data_length`; reject with error 2 if ≠ 0
3. Load `recipient_dup_marker`; reject with error 3 if ≠ 0xff
4. Load `recipient_data_length`; reject with error 4 if ≠ 0
5. Load `system_program_dup_marker`; reject with error 5 if ≠ 0xff
6. Load `instruction_data_length`; reject with error 6 if ≠ 8
7. Load `transfer_amount` and `sender_lamports`; reject with error 7 if lamports < amount
8. Construct System Program Transfer CPI and invoke (not modeled)

**Postconditions**: Exit code 0 on success, 1-7 for specific validation failures.

## 3. Formal Properties

### 3.1 Input Validation

**P1 (rejects_wrong_account_count)**: For all input addresses, memory states, and `numAccounts`,
if `readU64(mem, inputAddr) = numAccounts` and `numAccounts ≠ 3`,
then `execute(prog, initState(inputAddr, mem), 6).exitCode = some 1`.

### 3.2 Balance Safety

**P2 (rejects_insufficient_lamports)**: For all input addresses, memory states, amounts, and balances,
if all 7 validation checks pass and `senderLamports < amount`,
then `execute(prog, initState(inputAddr, mem), 20).exitCode = some 7`.

### 3.3 Happy Path

**P3 (accepts_valid_transfer)**: For all input addresses, memory states, amounts, and balances,
if all 7 validation checks pass and `senderLamports ≥ amount`,
then `execute(prog, initState(inputAddr, mem), 20).exitCode = some 0`.

## 4. Trust Boundary

- **Solana runtime**: Input buffer layout, r1 initialization
- **Memory model**: Little-endian byte reads via `readU64`/`readU8` (axiomatic round-trip)
- **CPI construction**: Instructions 15+ build and invoke the System Program Transfer CPI; modeled as a successful exit (r0 = 0)

## 5. Verification Results

| Property | Status | Proof |
|---|---|---|
| P1: rejects_wrong_account_count | **Verified** | `TransferProofs.rejects_wrong_account_count` |
| P2: rejects_insufficient_lamports | **Verified** | `TransferProofs.rejects_insufficient_lamports` |
| P3: accepts_valid_transfer | **Verified** | `TransferProofs.accepts_valid_transfer` |
