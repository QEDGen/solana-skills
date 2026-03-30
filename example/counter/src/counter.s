# Error codes.
# ------------
.equ E_N_ACCOUNTS, 1 # Invalid number of accounts.
.equ E_USER_DATA_LEN, 2 # User data length is nonzero.
.equ E_PDA_DATA_LEN, 3 # Invalid PDA data length.
.equ E_SYSTEM_PROGRAM_DATA_LEN, 4 # System Program data length is nonzero.
.equ E_PDA_DUPLICATE, 5 # PDA is a duplicate account.
.equ E_SYSTEM_PROGRAM_DUPLICATE, 6 # System Program is a duplicate account.
.equ E_UNABLE_TO_DERIVE_PDA, 7 # Unable to derive PDA.
.equ E_PDA_MISMATCH, 8 # Passed PDA does not match computed PDA.
.equ E_INVALID_INSTRUCTION_DATA_LEN, 9 # Invalid instruction data length.

# Size of assorted types.
# -----------------------
.equ SIZE_OF_PUBKEY, 32 # Size of Pubkey.
.equ SIZE_OF_U8, 1 # Size of u8.
.equ SIZE_OF_U64, 8 # Size of u64.
.equ SIZE_OF_U64_2X, 16 # Size of u64 times 2.
.equ SIZE_OF_U64_3X, 24 # Size of u64 times 3.

# Memory map layout.
# ------------------
.equ NON_DUP_MARKER, 0xff # Flag that an account is not a duplicate.
.equ DATA_LEN_ZERO, 0 # Data length of zero.
.equ DATA_LEN_SYSTEM_PROGRAM, 14 # Data length of System Program.
.equ N_ACCOUNTS_INCREMENT, 2 # Number of accounts for increment operation.
.equ N_ACCOUNTS_INIT, 3 # Number of accounts for initialize operation.
.equ N_ACCOUNTS_OFF, 0 # Number of accounts in virtual memory map.
.equ USER_DATA_LEN_OFF, 88 # User data length.
.equ USER_PUBKEY_OFF, 16 # User pubkey.
# Offset from user account data to PDA owner.
.equ USER_DATA_TO_PDA_OWNER_OFF, 10288
.equ PDA_NON_DUP_MARKER_OFF, 10344 # PDA non-duplicate marker.
.equ PDA_PUBKEY_OFF, 10352 # PDA pubkey.
.equ PDA_DATA_LEN_OFF, 10424 # PDA data length.
# PDA account data length plus account overhead.
.equ PDA_DATA_WITH_ACCOUNT_OVERHEAD, 137
.equ PDA_COUNTER_OFF, 10432 # PDA counter.
.equ PDA_BUMP_SEED_OFF, 10440 # PDA bump seed.
# System Program non-duplicate marker.
.equ SYSTEM_PROGRAM_NON_DUP_MARKER_OFF, 20680
.equ SYSTEM_PROGRAM_DATA_LEN_OFF, 20760 # System program data length.
.equ PROGRAM_ID_INIT_OFF, 31040 # Program ID during initialize operation.
# Instruction data length during increment operation.
.equ INSTRUCTION_DATA_LEN_INC_OFF, 20696
.equ COUNTER_INCREMENT_OFF, 20704 # Counter increment value.
.equ PROGRAM_ID_INC_OFF, 20712 # Program ID during increment operation.

# CreateAccount instruction data.
# -------------------------------
.equ INIT_CPI_N_ACCOUNTS, 2 # Number of accounts for CPI.
.equ INIT_CPI_INSN_DATA_LEN, 52 # Length of instruction data.
.equ INIT_CPI_DISCRIMINATOR, 0 # Discriminator.
.equ INIT_CPI_N_SIGNERS_SEEDS, 1 # Number of signers seeds.
.equ INIT_CPI_ACCT_SIZE, 9 # Account size.

# Stack frame layout for increment operation.
# -------------------------------------------
.equ STK_INC_SEED_0_ADDR_OFF, 64 # Pointer to user pubkey.
.equ STK_INC_SEED_0_LEN_OFF, 56 # Length of user pubkey.
.equ STK_INC_SEED_1_ADDR_OFF, 48 # Pointer to bump seed.
.equ STK_INC_SEED_1_LEN_OFF, 40 # Length of bump seed.
.equ STK_INC_PDA_OFF, 32 # Pointer to PDA.

# Stack frame layout for initialize operation.
# --------------------------------------------
# System Program pubkey for CreateAccount CPI.
.equ STK_INIT_SYSTEM_PROGRAM_PUBKEY_OFF, 384
.equ STK_INIT_INSN_OFF, 352 # SolInstruction for CreateAccount CPI.
# Accounts address in SolInstruction.
.equ STK_INIT_INSN_ACCOUNTS_ADDR_OFF, 344
# Accounts length in SolInstruction.
.equ STK_INIT_INSN_ACCOUNTS_LEN_OFF, 336
.equ STK_INIT_INSN_DATA_ADDR_OFF, 328 # Data address in SolInstruction.
.equ STK_INIT_INSN_DATA_LEN_OFF, 320 # Data length in SolInstruction.
# Offset from System Program pubkey to account metas.
.equ STK_INIT_SYSTEM_PROGRAM_PUBKEY_TO_ACCOUNT_METAS_OFF, 72
# Offset from account metas to instruction data.
.equ STK_INIT_ACCOUNT_METAS_TO_INSN_DATA_OFF, 32
.equ STK_INIT_INSN_DATA_OFF, 280 # CreateAccount instruction data.
# Offset of lamports field inside CreateAccount instruction data.
.equ STK_INIT_INSN_DATA_LAMPORTS_OFF, 276
# Offset of space field inside CreateAccount instruction data.
.equ STK_INIT_INSN_DATA_SPACE_OFF, 268
# Offset of owner field inside CreateAccount instruction data.
.equ STK_INIT_INSN_DATA_OWNER_OFF, 260
.equ STK_INIT_ACCT_INFOS_OFF, 224 # User account infos.
# User account meta pubkey address.
.equ STK_INIT_ACCT_META_USER_PUBKEY_ADDR_OFF, 312
# User account meta is_writable.
.equ STK_INIT_ACCT_META_USER_IS_WRITABLE_OFF, 304
# User account meta is_signer.
.equ STK_INIT_ACCT_META_USER_IS_SIGNER_OFF, 303
# PDA account meta pubkey address.
.equ STK_INIT_ACCT_META_PDA_PUBKEY_ADDR_OFF, 296
# PDA account meta is_writable.
.equ STK_INIT_ACCT_META_PDA_IS_WRITABLE_OFF, 288
# PDA account meta is_signer.
.equ STK_INIT_ACCT_META_PDA_IS_SIGNER_OFF, 287
# User account info key address.
.equ STK_INIT_ACCT_INFO_USER_KEY_ADDR_OFF, 224
# PDA account info key address.
.equ STK_INIT_ACCT_INFO_PDA_KEY_ADDR_OFF, 168
# User account info Lamports pointer.
.equ STK_INIT_ACCT_INFO_USER_LAMPORTS_ADDR_OFF, 216
# PDA account info Lamports pointer.
.equ STK_INIT_ACCT_INFO_PDA_LAMPORTS_ADDR_OFF, 160
# User account info owner pubkey pointer.
.equ STK_INIT_ACCT_INFO_USER_OWNER_ADDR_OFF, 192
# PDA account info owner pubkey pointer.
.equ STK_INIT_ACCT_INFO_PDA_OWNER_ADDR_OFF, 136
# User account info data pointer.
.equ STK_INIT_ACCT_INFO_USER_DATA_ADDR_OFF, 200
# PDA account info data pointer.
.equ STK_INIT_ACCT_INFO_PDA_DATA_ADDR_OFF, 144
# User account info is_signer.
.equ STK_INIT_ACCT_INFO_USER_IS_SIGNER_OFF, 176
# User account info is_writable.
.equ STK_INIT_ACCT_INFO_USER_IS_WRITABLE_OFF, 175
# PDA account info is_signer.
.equ STK_INIT_ACCT_INFO_PDA_IS_SIGNER_OFF, 120
# PDA account info is_writable.
.equ STK_INIT_ACCT_INFO_PDA_IS_WRITABLE_OFF, 119
.equ STK_INIT_SEED_0_ADDR_OFF, 112 # Pointer to user pubkey.
.equ STK_INIT_SEED_0_LEN_OFF, 104 # Length of user pubkey.
.equ STK_INIT_SEED_1_ADDR_OFF, 96 # Pointer to bump seed.
.equ STK_INIT_SEED_1_LEN_OFF, 88 # Length of bump seed.
.equ STK_INIT_SIGNERS_SEEDS_OFF, 80 # Pointer to signer seeds array.
# Pointer to signer seeds array element 0 length field.
.equ STK_INIT_SIGNER_SEEDS_0_LEN_OFF, 72
.equ STK_INIT_PDA_OFF, 64 # PDA.
.equ STK_INIT_RENT_OFF, 32 # Rent struct return.
.equ STK_INIT_BUMP_SEED_OFF, 8 # Bump seed.

# Assorted constants.
# -------------------
.equ NO_OFFSET, 0 # Offset of zero.
.equ SUCCESS, 0 # Indicates successful operation.
.equ BOOL_TRUE, 1 # Boolean true.
# Double wide boolean true for two consecutive fields.
.equ BOOL_TRUE_2X, 0x101
.equ N_SIGNER_SEEDS, 2 # Number of signer seeds for PDA.
.equ COMPARE_EQUAL, 0 # Compare result indicating equality.

.global entrypoint

entrypoint:
    ldxdw r2, [r1 + N_ACCOUNTS_OFF] # Get n accounts from input buffer.
    jeq r2, N_ACCOUNTS_INCREMENT, increment # Fast path to cheap operation.
    jeq r2, N_ACCOUNTS_INIT, initialize # Low priority, expensive anyways.
    mov64 r0, E_N_ACCOUNTS # Else fail.
    exit

initialize:

    # Check input memory map.
    # -----------------------
    ldxdw r2, [r1 + USER_DATA_LEN_OFF] # Get user data length.
    jne r2, DATA_LEN_ZERO, e_user_data_len # Exit if user account has data.
    ldxb r2, [r1 + PDA_NON_DUP_MARKER_OFF] # Check if PDA is a duplicate.
    jne r2, NON_DUP_MARKER, e_pda_duplicate # Exit if PDA is a duplicate.
    ldxdw r2, [r1 + PDA_DATA_LEN_OFF] # Get PDA data length.
    jne r2, DATA_LEN_ZERO, e_pda_data_len # Exit if PDA account has data.
    # Exit early if System Program is a duplicate.
    ldxb r2, [r1 + SYSTEM_PROGRAM_NON_DUP_MARKER_OFF]
    jne r2, NON_DUP_MARKER, e_system_program_duplicate
    # Exit early if System Program data length is invalid.
    ldxdw r2, [r1 + SYSTEM_PROGRAM_DATA_LEN_OFF]
    jne r2, DATA_LEN_SYSTEM_PROGRAM, e_system_program_data_len

    # Initialize signer seed for user pubkey.
    # ---------------------------------------
    mov64 r2, r1 # Get input buffer pointer.
    add64 r2, USER_PUBKEY_OFF # Update pointer to point at user pubkey.
    # Store pointer in seed 0 pointer field.
    stxdw [r10 - STK_INIT_SEED_0_ADDR_OFF], r2
    # Store length in seed 0 length field (32-bit immediate).
    stdw [r10 - STK_INIT_SEED_0_LEN_OFF], SIZE_OF_PUBKEY

    # Initialize signer seed for PDA bump key.
    # ----------------------------------------
    mov64 r2, r10 # Get stack frame pointer.
    sub64 r2, STK_INIT_BUMP_SEED_OFF # Update to point at PDA bump seed.
    # Store pointer in seed 1 pointer field.
    stxdw [r10 - STK_INIT_SEED_1_ADDR_OFF], r2
    # Store length in seed 1 length field (32-bit immediate).
    stdw [r10 - STK_INIT_SEED_1_LEN_OFF], SIZE_OF_U8

    # Compute PDA.
    # ------------
    mov64 r9, r1 # Store input buffer pointer for later.
    mov64 r1, r10 # Get stack frame pointer.
    # Update to point at user pubkey signer seed.
    sub64 r1, STK_INIT_SEED_0_ADDR_OFF
    mov64 r2, 1 # Indicate single signer seed (user pubkey).
    mov64 r3, r9 # Get input buffer pointer.
    add64 r3, PROGRAM_ID_INIT_OFF # Update to point at program ID.
    mov64 r4, r10 # Get stack frame pointer.
    sub64 r4, STK_INIT_PDA_OFF # Update to point to PDA region on stack.
    mov64 r5, r10 # Get stack frame pointer.
    sub64 r5, STK_INIT_BUMP_SEED_OFF # Update to point to bump seed region.
    call sol_try_find_program_address # Find PDA.
    # Skip check to error out if unable to derive a PDA (failure to derive
    # is practically impossible to test since odds of not finding bump seed
    # are astronomically low):
    # ```
    # jne r0, SUCCESS, e_unable_to_derive_pda
    # ```
    mov64 r1, r9 # Restore input buffer pointer.

    # Compare computed PDA against passed account.
    # --------------------------------------------
    # Update input buffer pointer to point to passed PDA.
    add64 r1, PDA_PUBKEY_OFF
    # As an optimization, store this pointer on the stack in the account
    # meta and info for the PDA, rather than deriving the pointer again.
    # Note that this must point to the pubkey in the input memory map, not
    # the one on the stack, otherwise the CPI will fail.
    stxdw [r10 - STK_INIT_ACCT_META_PDA_PUBKEY_ADDR_OFF], r1
    stxdw [r10 - STK_INIT_ACCT_INFO_PDA_KEY_ADDR_OFF], r1
    mov64 r2, r10 # Get stack frame pointer.
    sub64 r2, STK_INIT_PDA_OFF # Update to point to computed PDA.
    # Compare the pubkey values in 64-bit chunks, which is less CUs than:
    # ```
    # mov64 r3, SIZE_OF_PUBKEY # Flag size of bytes to compare.
    # mov64 r4, r10 # Get stack frame pointer.
    # sub64 r4, STK_INIT_MEMCMP_RESULT_OFF # Update to point to result.
    # call sol_memcmp_
    # ldxw r2, [r4 + NO_OFFSET] # Get compare result.
    # jne r2, COMPARE_EQUAL, e_pda_mismatch # Error out if PDA mismatch.
    # ```
    ldxdw r3, [r1 + 0]
    ldxdw r4, [r2 + 0]
    jne r3, r4, e_pda_mismatch
    ldxdw r3, [r1 + SIZE_OF_U64]
    ldxdw r4, [r2 + SIZE_OF_U64]
    jne r3, r4, e_pda_mismatch
    ldxdw r3, [r1 + SIZE_OF_U64_2X]
    ldxdw r4, [r2 + SIZE_OF_U64_2X]
    jne r3, r4, e_pda_mismatch
    ldxdw r3, [r1 + SIZE_OF_U64_3X]
    ldxdw r4, [r2 + SIZE_OF_U64_3X]
    jne r3, r4, e_pda_mismatch
    # Skip input buffer restoration since next block overwrites r1:
    # ```
    # mov64 r1, r9 # Restore input buffer pointer.
    # ```

    # Calculate Lamports required for new account.
    # --------------------------------------------
    mov64 r1, r10 # Get stack frame pointer.
    sub64 r1, STK_INIT_RENT_OFF # Update to point to Rent struct.
    call sol_get_rent_sysvar # Get Rent struct.
    ldxdw r2, [r1 + NO_OFFSET] # Get Lamports per byte field.
    # Multiply by sum of PDA account data length, account storage overhead.
    mul64 r2, PDA_DATA_WITH_ACCOUNT_OVERHEAD
    # Store value directly in instruction data on stack.
    stxdw [r10 - STK_INIT_INSN_DATA_LAMPORTS_OFF], r2
    # Skip input buffer restoration since block after next overwrites r1:
    # ```
    # mov64 r1, r9 # Restore input buffer pointer.
    # ```

    # Populate SolInstruction on stack.
    # ---------------------------------
    mov64 r3, r10 # Get stack frame pointer for stepping through stack.
    # Update to point to zero-initialized System Program pubkey on stack.
    sub64 r3, STK_INIT_SYSTEM_PROGRAM_PUBKEY_OFF
    stxdw [r10 - STK_INIT_INSN_OFF], r3 # Store as CPI program ID.
    # Advance to point to account metas.
    add64 r3, STK_INIT_SYSTEM_PROGRAM_PUBKEY_TO_ACCOUNT_METAS_OFF
    # Store pointer to account metas as CPI account metas address.
    stxdw [r10 - STK_INIT_INSN_ACCOUNTS_ADDR_OFF], r3
    # Store number of CPI accounts (fits in 32-bit immediate).
    stdw [r10 - STK_INIT_INSN_ACCOUNTS_LEN_OFF], INIT_CPI_N_ACCOUNTS
    # Advance to point to instruction data.
    add64 r3, STK_INIT_ACCOUNT_METAS_TO_INSN_DATA_OFF
    stxdw [r10 - STK_INIT_INSN_DATA_ADDR_OFF], r3 # Store CPI data address.
    # Store instruction data length (fits in 32-bit immediate).
    stdw [r10 - STK_INIT_INSN_DATA_LEN_OFF], INIT_CPI_INSN_DATA_LEN

    # Populate CreateAccount instruction data on stack.
    # ---------------------------------------------------------------------
    # - Discriminator is already set to 0 since stack is zero initialized.
    # - Lamports field was already set in the minimum balance calculation.
    # ---------------------------------------------------------------------
    # Store the data length of the account to create (fits in 32 bits).
    stdw [r10 - STK_INIT_INSN_DATA_SPACE_OFF], INIT_CPI_ACCT_SIZE
    mov64 r1, r10 # Get pointer to stack frame.
    sub64 r1, STK_INIT_INSN_DATA_OWNER_OFF # Point to new owner field.
    mov64 r2, r9 # Get input buffer pointer.
    add64 r2, PROGRAM_ID_INIT_OFF # Point to program ID.
    # Copy the pubkey value in 64-bit chunks, which is less CUs than:
    # ```
    # mov64 r3, SIZE_OF_PUBKEY # Set length of bytes to copy.
    # call sol_memcpy_ # Copy program ID into CreateAccount owner field.
    # ```
    ldxdw r3, [r2 + 0]
    stxdw [r1 + 0], r3
    ldxdw r3, [r2 + SIZE_OF_U64]
    stxdw [r1 + SIZE_OF_U64], r3
    ldxdw r3, [r2 + SIZE_OF_U64_2X]
    stxdw [r1 + SIZE_OF_U64_2X], r3
    ldxdw r3, [r2 + SIZE_OF_U64_3X]
    stxdw [r1 + SIZE_OF_U64_3X], r3

    # Flag user and PDA accounts as CPI writable signers.
    # ---------------------------------------------------
    sth [r10 - STK_INIT_ACCT_META_USER_IS_WRITABLE_OFF], BOOL_TRUE_2X
    sth [r10 - STK_INIT_ACCT_META_PDA_IS_WRITABLE_OFF], BOOL_TRUE_2X
    sth [r10 - STK_INIT_ACCT_INFO_USER_IS_SIGNER_OFF], BOOL_TRUE_2X
    sth [r10 - STK_INIT_ACCT_INFO_PDA_IS_SIGNER_OFF], BOOL_TRUE_2X
    # Optimize out 4 CUs by omitting the following assignments, which are
    # covered by the double wide boolean true assign since is_signer
    # follows is_writable in SolAccountMeta, and is_writable follows
    # is_signer in SolAccountInfo.
    # ```
    # stb [r10 - STK_INIT_ACCT_META_USER_IS_SIGNER_OFF], BOOL_TRUE
    # stb [r10 - STK_INIT_ACCT_META_PDA_IS_SIGNER_OFF], BOOL_TRUE
    # stb [r10 - STK_INIT_ACCT_INFO_USER_IS_WRITABLE_OFF], BOOL_TRUE
    # stb [r10 - STK_INIT_ACCT_INFO_PDA_IS_WRITABLE_OFF], BOOL_TRUE
    # ```

    # Walk through remaining pointer fields for account metas and infos.
    # ---------------------------------------------------------------------
    # - Rent epoch is ignored since is not needed.
    # - Data length and executable status are ignored since both values are
    #   zero and the stack is zero-initialized.
    # - PDA pubkey is ignored since it was set above as an optimization
    #   during the PDA compare operation.
    # ---------------------------------------------------------------------
    mov64 r2, r9 # Get input buffer pointer.
    add64 r2, USER_PUBKEY_OFF # Update to point at user pubkey.
    # Store in account meta and account info.
    stxdw [r10 - STK_INIT_ACCT_META_USER_PUBKEY_ADDR_OFF], r2
    stxdw [r10 - STK_INIT_ACCT_INFO_USER_KEY_ADDR_OFF], r2
    add64 r2, SIZE_OF_PUBKEY # Advance to point at user owner.
    # Store in account info.
    stxdw [r10 - STK_INIT_ACCT_INFO_USER_OWNER_ADDR_OFF], r2
    add64 r2, SIZE_OF_PUBKEY # Advance to point at user Lamports.
    # Store in account info.
    stxdw [r10 - STK_INIT_ACCT_INFO_USER_LAMPORTS_ADDR_OFF], r2
    add64 r2, SIZE_OF_U64_2X # Advance to point to user account data.
    # Store in account info.
    stxdw [r10 - STK_INIT_ACCT_INFO_USER_DATA_ADDR_OFF], r2
    # Advance to point to PDA owner. Note that this must be used instead of
    # the System Program pubkey on the stack or the CPI will fail.
    add64 r2, USER_DATA_TO_PDA_OWNER_OFF
    stxdw [r10 - STK_INIT_ACCT_INFO_PDA_OWNER_ADDR_OFF], r2
    # Advance to point to PDA Lamports.
    add64 r2, SIZE_OF_PUBKEY
    # Store in account info.
    stxdw [r10 - STK_INIT_ACCT_INFO_PDA_LAMPORTS_ADDR_OFF], r2
    add64 r2, SIZE_OF_U64_2X # Advance to point to PDA account data.
    # Store in account info.
    stxdw [r10 - STK_INIT_ACCT_INFO_PDA_DATA_ADDR_OFF], r2

    # Populate SignerSeeds structure.
    # -------------------------------
    mov64 r2, r10 # Get stack frame pointer.
    sub64 r2, STK_INIT_SEED_0_ADDR_OFF # Update to point to signer seed 0.
    stxdw [r10 - STK_INIT_SIGNERS_SEEDS_OFF], r2 # Store in SignerSeeds.
    # Store number of signer seeds for PDA (32-bit immediate).
    stdw [r10 - STK_INIT_SIGNER_SEEDS_0_LEN_OFF], N_SIGNER_SEEDS

    # Invoke CreateAccount CPI.
    # -------------------------
    mov64 r1, r10 # Get stack frame pointer.
    sub64 r1, STK_INIT_INSN_OFF # Point to instruction.
    mov64 r2, r10 # Get stack frame pointer.
    sub64 r2, STK_INIT_ACCT_INFOS_OFF # Point to account infos.
    mov64 r3, INIT_CPI_N_ACCOUNTS # Indicate number of account infos.
    mov64 r4, r10 # Get stack frame pointer.
    sub64 r4, STK_INIT_SIGNERS_SEEDS_OFF # Point to single SignerSeeds.
    mov64 r5, INIT_CPI_N_SIGNERS_SEEDS # Indicate a single signer.
    call sol_invoke_signed_c

    # Write bump seed to new account.
    # -------------------------------
    ldxb r2, [r10 - STK_INIT_BUMP_SEED_OFF] # Load bump seed from stack.
    stxb [r9 + PDA_BUMP_SEED_OFF], r2 # Store in new PDA account data.

    exit

increment:

    # Get user data length with padding.
    # ----------------------------------
    ldxdw r9, [r1 + USER_DATA_LEN_OFF] # Get user data length.
    # Speculatively add max possible padding. This will not overflow
    # because max account data length fits in a u32.
    add64 r9, 7
    # Clear low 3 bits, thereby truncating to 8-byte alignment. This yields
    # the data length plus (optional) required padding.
    and64 r9, -8

    # Check remaining memory map layout.
    # ----------------------------------
    # Sum input buffer offset and padded user data length, affecting
    # subsequent offsets originally calculated assuming no user account
    # data: get input buffer pointer offset by padded user data length.
    add64 r9, r1
    ldxb r8, [r9 + PDA_NON_DUP_MARKER_OFF] # Load PDA duplicate marker.
    jne r8, NON_DUP_MARKER, e_pda_duplicate # Exit if PDA is a duplicate.
    ldxdw r8, [r9 + PDA_DATA_LEN_OFF] # Get PDA data length.
    jne r8, INIT_CPI_ACCT_SIZE, e_pda_data_len # Exit if invalid length.
    # Get instruction data length.
    ldxdw r8, [r9 + INSTRUCTION_DATA_LEN_INC_OFF]
    # Exit if invalid length.
    jne r8, SIZE_OF_U64, e_invalid_instruction_data_len
    mov64 r3, r9 # Copy input buffer offset by padded user data length.
    # Update to point to program ID, for later verification syscall.
    add64 r3, PROGRAM_ID_INC_OFF
    mov64 r6, r9 # Copy input buffer offset by padded user data length.
    # Update to point to PDA pubkey, for later verification syscall.
    add64 r6, PDA_PUBKEY_OFF

    # Process counter increment instruction.
    # ---------------------------------------------------------------------
    # This is done speculatively, before PDA bump seeds are verified, to
    # minimize number of pointer copies during happy path. If this were
    # done after signer seed verification, the pointer to the input buffer
    # offset by user data length would need to be copied.
    # ---------------------------------------------------------------------
    ldxdw r8, [r9 + COUNTER_INCREMENT_OFF] # Get increment amount.
    ldxdw r7, [r9 + PDA_COUNTER_OFF] # Get current PDA counter value.
    add64 r7, r8 # Wrapping increment counter value by instruction amount.
    stxdw [r9 + PDA_COUNTER_OFF], r7 # Store value in PDA account.

    # Prepare signer seeds for PDA verification.
    # ------------------------------------------
    # Directly mutate input buffer pointer to point to user pubkey, since
    # this is the last access of the input buffer pointer for this branch
    # and a pointer copy can thus be optimized out.
    add64 r1, USER_PUBKEY_OFF
    # Store pointer in seed 0 pointer field.
    stxdw [r10 - STK_INC_SEED_0_ADDR_OFF], r1
    # Store length in seed 0 length field (32-bit immediate).
    stdw [r10 - STK_INC_SEED_0_LEN_OFF], SIZE_OF_PUBKEY
    # Directly mutate input pointer offset by padded user data length to
    # point at PDA bump seed, since this is the last access of the offset
    # pointer for this branch and a pointer copy can thus be optimized out.
    add64 r9, PDA_BUMP_SEED_OFF
    # Store pointer in seed 1 pointer field.
    stxdw [r10 - STK_INC_SEED_1_ADDR_OFF], r9
    # Store length in seed 1 length field (32-bit immediate).
    stdw [r10 - STK_INC_SEED_1_LEN_OFF], SIZE_OF_U8

    # Re-derive PDA.
    # ---------------------------------------------------------------------
    # r3 was set to program ID pointer during memory map checks.
    # ---------------------------------------------------------------------
    mov64 r1, r10 # Get stack frame pointer.
    sub64 r1, STK_INC_SEED_0_ADDR_OFF # Update to point to signer seeds.
    mov64 r2, N_SIGNER_SEEDS # Load signer seeds count (32-bit immediate).
    mov64 r4, r10 # Get stack frame pointer.
    sub64 r4, STK_INC_PDA_OFF # Update to point to PDA result on stack.
    call sol_create_program_address # Create PDA.
    jne r0, SUCCESS, e_unable_to_derive_pda # Error if unable to create.

    # Verify PDA.
    # ---------------------------------------------------------------------
    # r6 was set to passed PDA pubkey pointer during memory map checks.
    # ---------------------------------------------------------------------
    mov64 r1, r6 # Get pointer to passed PDA pubkey, set above.
    mov64 r2, r4 # Get pointer to computed PDA.
    # Compare pubkey values in 64-bit chunks (same optimization as in the
    # initialize operation):
    ldxdw r3, [r1 + 0]
    ldxdw r4, [r2 + 0]
    jne r3, r4, e_pda_mismatch
    ldxdw r3, [r1 + SIZE_OF_U64]
    ldxdw r4, [r2 + SIZE_OF_U64]
    jne r3, r4, e_pda_mismatch
    ldxdw r3, [r1 + SIZE_OF_U64_2X]
    ldxdw r4, [r2 + SIZE_OF_U64_2X]
    jne r3, r4, e_pda_mismatch
    ldxdw r3, [r1 + SIZE_OF_U64_3X]
    ldxdw r4, [r2 + SIZE_OF_U64_3X]
    jne r3, r4, e_pda_mismatch

    exit

e_user_data_len:
    mov32 r0, E_USER_DATA_LEN
    exit

e_pda_data_len:
    mov32 r0, E_PDA_DATA_LEN
    exit

e_system_program_data_len:
    mov32 r0, E_SYSTEM_PROGRAM_DATA_LEN
    exit

e_pda_duplicate:
    mov32 r0, E_PDA_DUPLICATE
    exit

e_system_program_duplicate:
    mov32 r0, E_SYSTEM_PROGRAM_DUPLICATE
    exit

e_pda_mismatch:
    mov32 r0, E_PDA_MISMATCH
    exit

e_invalid_instruction_data_len:
    mov32 r0, E_INVALID_INSTRUCTION_DATA_LEN
    exit

e_unable_to_derive_pda:
    mov32 r0, E_UNABLE_TO_DERIVE_PDA
    exit
