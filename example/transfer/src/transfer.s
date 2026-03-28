# Source: https://github.com/DASMAC-com/solana-opcode-guide
#
# Invalid number of accounts.
.equ E_N_ACCOUNTS, 1
# Sender data length is nonzero.
.equ E_DATA_LENGTH_NONZERO_SENDER, 2
# Recipient account is a duplicate.
.equ E_DUPLICATE_ACCOUNT_RECIPIENT, 3
# Recipient data length is nonzero.
.equ E_DATA_LENGTH_NONZERO_RECIPIENT, 4
# System program account is a duplicate.
.equ E_DUPLICATE_ACCOUNT_SYSTEM_PROGRAM, 5
# Invalid instruction data length.
.equ E_INSTRUCTION_DATA_LENGTH, 6
# Sender has insufficient Lamports.
.equ E_INSUFFICIENT_LAMPORTS, 7

# Stack offsets.
.equ STACK_SYSTEM_PROGRAM_PUBKEY_OFFSET, 232
.equ STACK_INSN_OFFSET, 200
.equ STACK_INSN_DATA_OFFSET, 160
.equ STACK_ACCT_METAS_OFFSET, 144
.equ STACK_ACCT_INFOS_OFFSET, 112

# Account layout.
.equ N_ACCOUNTS_OFFSET, 0
.equ N_ACCOUNTS_EXPECTED, 3
.equ NON_DUP_MARKER, 0xff

# Sender account.
.equ SENDER_LAMPORTS_OFFSET, 80
.equ SENDER_DATA_LENGTH_OFFSET, 88

# Recipient account.
.equ RECIPIENT_OFFSET, 10344
.equ RECIPIENT_DATA_LENGTH_OFFSET, 10424

# System program account.
.equ SYSTEM_PROGRAM_OFFSET, 20680

# Transfer input.
.equ INSTRUCTION_DATA_LENGTH_OFFSET, 31032
.equ INSTRUCTION_DATA_LENGTH_EXPECTED, 8
.equ INSTRUCTION_DATA_OFFSET, 31040

.global entrypoint

entrypoint:
    ldxdw r2, [r1 + N_ACCOUNTS_OFFSET]
    jne r2, N_ACCOUNTS_EXPECTED, e_n_accounts

    ldxdw r2, [r1 + SENDER_DATA_LENGTH_OFFSET]
    jne r2, DATA_LENGTH_ZERO, e_data_length_nonzero_sender

    ldxb r2, [r1 + RECIPIENT_OFFSET]
    jne r2, NON_DUP_MARKER, e_duplicate_account_recipient

    ldxdw r2, [r1 + RECIPIENT_DATA_LENGTH_OFFSET]
    jne r2, DATA_LENGTH_ZERO, e_data_length_nonzero_recipient

    ldxb r2, [r1 + SYSTEM_PROGRAM_OFFSET]
    jne r2, NON_DUP_MARKER, e_duplicate_account_system_program

    ldxdw r4, [r1 + INSTRUCTION_DATA_LENGTH_OFFSET]
    jne r4, INSTRUCTION_DATA_LENGTH_EXPECTED, e_instruction_data_length

    ldxdw r4, [r1 + INSTRUCTION_DATA_OFFSET]
    ldxdw r2, [r1 + SENDER_LAMPORTS_OFFSET]
    jlt r2, r4, e_insufficient_lamports

    # ... CPI construction and invocation ...
    call sol_invoke_signed
    exit

e_n_accounts:
    mov64 r0, 1
    exit

e_data_length_nonzero_sender:
    mov64 r0, 2
    exit

e_duplicate_account_recipient:
    mov64 r0, 3
    exit

e_data_length_nonzero_recipient:
    mov64 r0, 4
    exit

e_duplicate_account_system_program:
    mov64 r0, 5
    exit

e_instruction_data_length:
    mov64 r0, 6
    exit

e_insufficient_lamports:
    mov64 r0, 7
    exit
