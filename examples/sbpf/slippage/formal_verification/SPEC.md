# Slippage Guard Verification Spec v1.0

A hand-written sBPF assembly program that rejects transactions when the token
account balance drops below a configured minimum threshold.

## 0. Security Goals

1. **Slippage Rejection**: The program MUST reject (exit code 1) when the minimum required balance exceeds or equals the actual token account balance.
2. **Slippage Acceptance**: The program MUST accept (exit code 0) when the token account balance strictly exceeds the minimum required balance.

## 1. State Model

The program reads two u64 values from the Solana input buffer (pointed to by r1):

| Field | Offset | Type | Description |
|---|---|---|---|
| `token_account_balance` | `0x00a0` | u64 LE | Current token account balance |
| `minimum_balance` | `0x2918` | u64 LE | Minimum required balance threshold |

No mutable state. The program is a pure guard: compare and exit.

## 2. Operations

### 2.1 Entrypoint

**Signers**: N/A (no signer checks — this is a guard, not an instruction handler)

**Preconditions**: r1 points to a valid Solana input buffer containing serialized accounts.

**Effects**:
1. Load `minimum_balance` from `mem[r1 + 0x2918]` into r3
2. Load `token_account_balance` from `mem[r1 + 0x00a0]` into r4
3. Compare r3 >= r4:
   - **True (slippage exceeded)**: Log "Slippage exceeded" via `sol_log_`, set r0 = 1, exit
   - **False (balance OK)**: Exit immediately (r0 = 0, the default)

**Postconditions**: Exit code is 1 if slippage exceeded, 0 otherwise.

## 3. Formal Properties

### 3.1 Guard Correctness

**P1 (rejects_insufficient_balance)**: For all input addresses and memory states,
if `readU64(mem, inputAddr + 0x2918) = minBal` and `readU64(mem, inputAddr + 0x00a0) = tokenBal`
and `minBal >= tokenBal`, then `execute(prog, initState(inputAddr, mem), 10).exitCode = some 1`.

**P2 (accepts_sufficient_balance)**: For all input addresses and memory states,
if `readU64(mem, inputAddr + 0x2918) = minBal` and `readU64(mem, inputAddr + 0x00a0) = tokenBal`
and `minBal < tokenBal`, then `execute(prog, initState(inputAddr, mem), 10).exitCode = some 0`.

## 4. Trust Boundary

- **Solana runtime**: Input buffer layout, r1 initialization
- **Memory model**: Little-endian byte reads via `readU64` (axiomatic round-trip)
- **Syscall behavior**: `sol_log_` sets r0 = 0, does not modify memory or halt

## 5. Verification Results

| Property | Status | Proof |
|---|---|---|
| P1: rejects_insufficient_balance | **Verified** | `SlippageProofs.rejects_insufficient_balance` |
| P2: accepts_sufficient_balance | **Verified** | `SlippageProofs.accepts_sufficient_balance` |
