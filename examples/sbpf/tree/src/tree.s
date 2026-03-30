# Source: https://github.com/DASMAC-com/solana-opcode-guide
# ANCHOR: constants
# Error codes.
# ------------
.equ E_N_ACCOUNTS, 1 # An invalid number of accounts were passed.
.equ E_USER_DATA_LEN, 2 # The user account has invalid data length.
.equ E_TREE_DATA_LEN, 3 # The tree account has invalid data length.
# The System Program account has invalid data length.
.equ E_SYSTEM_PROGRAM_DATA_LEN, 4
.equ E_TREE_DUPLICATE, 5 # The tree account is a duplicate.
# The System Program account is a duplicate.
.equ E_SYSTEM_PROGRAM_DUPLICATE, 6
.equ E_RENT_DUPLICATE, 7 # The rent sysvar account is a duplicate.
.equ E_RENT_ADDRESS, 8 # The rent sysvar account has invalid data length.
# Instruction data provided during initialization instruction.
.equ E_INSTRUCTION_DATA, 9
# The passed PDA does not match the expected address.
.equ E_PDA_MISMATCH, 10
.equ E_INSTRUCTION_DISCRIMINATOR, 11 # Invalid instruction discriminator.
.equ E_INSTRUCTION_DATA_LEN, 12 # Invalid instruction data length.
# Not enough accounts passed for insertion allocation.
.equ E_N_ACCOUNTS_INSERT_ALLOCATION, 13
.equ E_KEY_EXISTS, 14 # Key already exists in tree during insertion.
.equ E_KEY_DOES_NOT_EXIST, 15 # Key does not exist in tree during removal.

# Type sizes.
# -----------
.equ SIZE_OF_U8, 1 # Size of u8.
.equ SIZE_OF_U64, 8 # Size of u64.
.equ SIZE_OF_ADDRESS, 32 # Size of Address.
.equ SIZE_OF_U128, 16 # Size of u128.
.equ SIZE_OF_TREE_HEADER, 24 # Size of TreeHeader.
.equ SIZE_OF_INITIALIZE_INSTRUCTION, 1 # Size of InitializeInstruction.
.equ SIZE_OF_INSERT_INSTRUCTION, 5 # Size of InsertInstruction.
.equ SIZE_OF_REMOVE_INSTRUCTION, 3 # Size of RemoveInstruction.
.equ SIZE_OF_TREE_NODE, 29 # Size of TreeNode.

# Data layout constants.
# ----------------------
.equ DATA_LEN_ZERO, 0 # Data length of zero.
.equ BPF_ALIGN_OF_U128, 8 # Data alignment during runtime.
.equ OFFSET_ZERO, 0 # No offset.
.equ NULL, 0 # Null pointer.
.equ DATA_LEN_AND_MASK, -8 # And mask for data length alignment.
.equ MAX_DATA_PAD, 7 # Maximum possible data length padding.
.equ BOOL_TRUE, 1 # Boolean true value.

# Pubkey chunking offsets.
# ------------------------
.equ PUBKEY_CHUNK_OFF_0, 0 # Offset for the first 8 bytes.
.equ PUBKEY_CHUNK_OFF_1, 8 # Offset for the second 8 bytes.
.equ PUBKEY_CHUNK_OFF_2, 16 # Offset for the third 8 bytes.
.equ PUBKEY_CHUNK_OFF_3, 24 # Offset for the fourth 8 bytes.

# Input buffer layout.
# --------------------
.equ IB_N_ACCOUNTS_OFF, 0 # Number of accounts field.
.equ IB_USER_ACCOUNT_OFF, 8 # User runtime account.
.equ IB_USER_LAMPORTS_OFF, 80 # User Lamports field.
.equ IB_USER_DATA_OFF, 96 # User data field.
.equ IB_USER_OWNER_OFF, 48 # User owner field.
.equ IB_TREE_LAMPORTS_OFF, 10416 # Tree Lamports field.
.equ IB_TREE_DATA_OFF, 10432 # Tree data field.
.equ IB_TREE_OWNER_OFF, 10384 # Tree owner field.
.equ IB_TREE_ACCOUNT_OFF, 10344 # Tree runtime account header.
.equ IB_TREE_ADDRESS_OFF, 10352 # Tree address field.
# System Program runtime account header.
.equ IB_SYSTEM_PROGRAM_ACCOUNT_OFF, 20680
.equ IB_RENT_ACCOUNT_OFF, 31032 # Rent sysvar account header.
.equ IB_RENT_DATA_OFF, 31120 # Rent sysvar account data.
# Expected number of accounts for general instructions.
.equ IB_N_ACCOUNTS_GENERAL, 2
# Expected number of accounts for tree initialization.
.equ IB_N_ACCOUNTS_INIT, 4
# Expected data length of system program account.
.equ IB_SYSTEM_PROGRAM_DATA_LEN, 14
.equ IB_RENT_DATA_LEN, 17 # Expected data length of rent sysvar account.
.equ IB_USER_ADDRESS_OFF, 16 # User address field.
.equ IB_USER_DATA_LEN_OFF, 88 # User data length field.
.equ IB_NON_DUP_MARKER, 255 # Non-duplicate marker value.
.equ IB_TREE_NON_DUP_MARKER_OFF, 10344 # Tree non-duplicate marker field.
.equ IB_TREE_ADDRESS_OFF_0, 10352 # Tree address field (chunk index 0).
.equ IB_TREE_ADDRESS_OFF_1, 10360 # Tree address field (chunk index 1).
.equ IB_TREE_ADDRESS_OFF_2, 10368 # Tree address field (chunk index 2).
.equ IB_TREE_ADDRESS_OFF_3, 10376 # Tree address field (chunk index 3).
.equ IB_TREE_DATA_LEN_OFF, 10424 # Tree data length field.
# System Program non-duplicate marker field.
.equ IB_SYSTEM_PROGRAM_NON_DUP_MARKER_OFF, 20680
# System Program data length field.
.equ IB_SYSTEM_PROGRAM_DATA_LEN_OFF, 20760
# Rent account non-duplicate marker field.
.equ IB_RENT_NON_DUP_MARKER_OFF, 31032
.equ IB_RENT_ADDRESS_OFF_0, 31040 # Rent address field (chunk index 0).
.equ IB_RENT_ADDRESS_OFF_1, 31048 # Rent address field (chunk index 1).
.equ IB_RENT_ADDRESS_OFF_2, 31056 # Rent address field (chunk index 2).
.equ IB_RENT_ADDRESS_OFF_3, 31064 # Rent address field (chunk index 3).
.equ IB_RENT_ID_CHUNK_0, 5862609301215225606 # Rent sysvar ID (chunk 0).
.equ IB_RENT_ID_CHUNK_0_LO, 399877894 # Rent sysvar ID (chunk 0 lo).
.equ IB_RENT_ID_CHUNK_0_HI, 1364995097 # Rent sysvar ID (chunk 0 hi).
.equ IB_RENT_ID_CHUNK_1, 9219231539345853473 # Rent sysvar ID (chunk 1).
.equ IB_RENT_ID_CHUNK_1_LO, 1288277025 # Rent sysvar ID (chunk 1 lo).
.equ IB_RENT_ID_CHUNK_1_HI, 2146519613 # Rent sysvar ID (chunk 1 hi).
.equ IB_RENT_ID_CHUNK_2, 4971307250928769624 # Rent sysvar ID (chunk 2).
.equ IB_RENT_ID_CHUNK_2_LO, 149871192 # Rent sysvar ID (chunk 2 lo).
.equ IB_RENT_ID_CHUNK_2_HI, 1157472667 # Rent sysvar ID (chunk 2 hi).
.equ IB_RENT_ID_CHUNK_3, 2329533411 # Rent sysvar ID (chunk 3).
.equ IB_RENT_ID_CHUNK_3_LO, -1965433885 # Rent sysvar ID (chunk 3 lo).
.equ IB_RENT_ID_CHUNK_3_HI, 0 # Rent sysvar ID (chunk 3 hi).
# Program ID field for initialize instruction.
.equ IB_INIT_PROGRAM_ID_OFF_IMM, 41401
.equ IB_TREE_DATA_TOP_OFF, 10440 # Tree top pointer field within tree data.
# Tree next pointer field within tree data.
.equ IB_TREE_DATA_NEXT_OFF, 10448
# Tree root pointer field within tree data.
.equ IB_TREE_DATA_ROOT_OFF, 10432
# Relative offset from user data field to tree pubkey field.
.equ IB_USER_DATA_TO_TREE_ADDRESS_REL_OFF_IMM, 10256

# Offsets for instruction processing.
# -----------------------------------
.equ INSN_DISCRIMINATOR_OFF, 0 # Offset to instruction discriminator byte.
# Initialize instruction discriminator.
.equ INSN_DISCRIMINATOR_INITIALIZE, 0
.equ INSN_DISCRIMINATOR_INSERT, 1 # Insert instruction discriminator.
.equ INSN_DISCRIMINATOR_REMOVE, 2 # Remove instruction discriminator.
.equ INSN_INSERT_KEY_OFF, 1 # Key field in insert instruction.
.equ INSN_INSERT_VALUE_OFF, 3 # Value field in insert instruction.
.equ INSN_REMOVE_KEY_OFF, 1 # Key field in remove instruction.

# Init stack frame layout.
# ------------------------
.equ SF_INIT_BUMP_SEED_OFF, -352 # Bump seed.
.equ SF_INIT_SIGNER_SEED_ADDR_OFF, -96 # Bump signer seed address field.
.equ SF_INIT_SIGNER_SEED_LEN_OFF, -88 # Bump signer seed length field.
.equ SF_INIT_PDA_OFF, -80 # PDA address field.
# Discriminator field in CPI instruction data.
.equ SF_INIT_CREATE_ACCOUNT_DISCRIMINATOR_UOFF, -351
# Lamports field in CreateAccount instruction data.
.equ SF_INIT_CREATE_ACCOUNT_LAMPORTS_UOFF, -347
# Space address field in CreateAccount instruction data.
.equ SF_INIT_CREATE_ACCOUNT_SPACE_UOFF, -339
# Owner field in CreateAccount instruction data (chunk index 0).
.equ SF_INIT_CREATE_ACCOUNT_OWNER_UOFF_0, -331
# Owner field in CreateAccount instruction data (chunk index 1).
.equ SF_INIT_CREATE_ACCOUNT_OWNER_UOFF_1, -323
# Owner field in CreateAccount instruction data (chunk index 2).
.equ SF_INIT_CREATE_ACCOUNT_OWNER_UOFF_2, -315
# Owner field in CreateAccount instruction data (chunk index 3).
.equ SF_INIT_CREATE_ACCOUNT_OWNER_UOFF_3, -307
.equ SF_INIT_SIGNERS_SEEDS_ADDR_OFF, -112 # Signers seeds address field.
.equ SF_INIT_SIGNERS_SEEDS_LEN_OFF, -104 # Signers seeds length field.
.equ SF_INIT_SYSTEM_PROGRAM_ADDRESS_OFF, -32 # System Program address.
.equ SF_INIT_INSN_PROGRAM_ID_OFF, -296 # SolInstruction program_id field.
.equ SF_INIT_INSN_ACCOUNTS_OFF, -288 # SolInstruction accounts field.
.equ SF_INIT_INSN_ACCOUNT_LEN_OFF, -280 # SolInstruction account_len field.
.equ SF_INIT_INSN_DATA_OFF, -272 # SolInstruction data field.
.equ SF_INIT_INSN_DATA_LEN_OFF, -264 # SolInstruction data_len field.
# SolAccountMeta is_writable field for user account.
.equ SF_INIT_USER_META_IS_WRITABLE_OFF, -248
# SolAccountMeta is_writable field for tree account.
.equ SF_INIT_TREE_META_IS_WRITABLE_OFF, -232
# SolAccountInfo is_signer field for user account.
.equ SF_INIT_USER_INFO_IS_SIGNER_OFF, -176
# SolAccountMeta pubkey field for user account.
.equ SF_INIT_USER_META_PUBKEY_OFF, -256
# SolAccountInfo pubkey field for user account.
.equ SF_INIT_USER_INFO_PUBKEY_OFF, -224
# SolAccountInfo owner field for user account.
.equ SF_INIT_USER_INFO_OWNER_OFF, -192
# SolAccountInfo lamports field for user account.
.equ SF_INIT_USER_INFO_LAMPORTS_OFF, -216
# SolAccountInfo data_len field for user account.
.equ SF_INIT_USER_INFO_DATA_OFF, -200
# SolAccountInfo data_len for tree account.
.equ SF_INIT_TREE_INFO_DATA_LEN_OFF, -152
# SolAccountInfo is_signer field for tree account.
.equ SF_INIT_TREE_INFO_IS_SIGNER_OFF, -120
# SolAccountInfo is_writable field for tree account.
.equ SF_INIT_TREE_INFO_IS_WRITABLE_UOFF, -119
# SolAccountMeta pubkey field for tree account.
.equ SF_INIT_TREE_META_PUBKEY_OFF, -240
# SolAccountInfo pubkey field for tree account.
.equ SF_INIT_TREE_INFO_PUBKEY_OFF, -168
# SolAccountInfo owner field for tree account.
.equ SF_INIT_TREE_INFO_OWNER_OFF, -136
# SolAccountInfo lamports field for tree account.
.equ SF_INIT_TREE_INFO_LAMPORTS_OFF, -160
# SolAccountInfo data_len field for tree account.
.equ SF_INIT_TREE_INFO_DATA_OFF, -144
# Relative offset from PDA on stack to System Program ID.
.equ SF_INIT_PDA_TO_SYSTEM_PROGRAM_ID_REL_OFF_IMM, 48
# Relative offset from System Program ID to first SolAccountMeta.
.equ SF_INIT_SYSTEM_PROGRAM_ID_TO_ACCT_METAS_REL_OFF_IMM, -224
# Relative offset from SolAccountMeta array to instruction data.
.equ SF_INIT_ACCT_METAS_TO_INSN_DATA_REL_OFF_IMM, -95
# Relative offset from instruction data to signer seeds.
.equ SF_INIT_INSN_DATA_TO_SIGNER_SEEDS_REL_OFF_IMM, 255
# Relative offset from signer seeds to signers seeds.
.equ SF_INIT_SIGNER_SEEDS_TO_SIGNERS_SEEDS_REL_OFF_IMM, -16
.equ SF_INIT_ACCT_INFOS_OFF, -224 # Account infos array.

# CPI-specific constants.
# -----------------------
.equ CPI_N_ACCOUNTS, 2 # User and tree accounts.
# The tree account is a PDA for CreateAccount CPI.
.equ CPI_N_PDA_SIGNERS, 1
# Number of seeds for CreateAccount PDA signer (bump only).
.equ CPI_N_SEEDS_CREATE_ACCOUNT, 1
# PDA signers for Transfer CPI (none — user is already a signer).
.equ CPI_N_PDA_SIGNERS_TRANSFER, 0
.equ CPI_N_SEEDS_TRY_FIND_PDA, 0 # Number of seeds for PDA generation.
.equ CPI_TREE_DATA_LEN, 24 # Tree account data length.
# Account data scalar for base rent calculation.
.equ CPI_ACCOUNT_DATA_SCALAR, 152
# CreateAccount discriminator for CPI.
.equ CPI_CREATE_ACCOUNT_DISCRIMINATOR, 0
# Length of CreateAccount instruction data.
.equ CPI_CREATE_ACCOUNT_INSN_DATA_LEN, 52
.equ CPI_TRANSFER_DISCRIMINATOR, 2 # Transfer discriminator for CPI.
.equ CPI_TRANSFER_INSN_DATA_LEN, 12 # Length of Transfer instruction data.
.equ CPI_WRITABLE_SIGNER, 0x0101 # Mask for writable signer.
.equ CPI_USER_ACCOUNT_INDEX, 0 # Account index for user account in CPI.
.equ CPI_TREE_ACCOUNT_INDEX, 1 # Account index for tree account in CPI.
.equ CPI_RENT_EPOCH_NULL, 0 # Null rent epoch.

# Tree constants.
# ---------------
.equ TREE_N_CHILDREN, 2 # Max number of children per node.
.equ TREE_DIR_L, 0 # Left direction.
.equ TREE_DIR_R, 1 # Right direction.
.equ TREE_COLOR_B, 0 # Black color.
.equ TREE_COLOR_R, 1 # Red color.
.equ TREE_HEADER_TOP_OFF, 8 # Stack top field in header.
.equ TREE_HEADER_NEXT_OFF, 16 # Next node field in header.
.equ TREE_DISCRIMINATOR_INSERT, 1 # Discriminator for insert instruction.
.equ TREE_NODE_KEY_OFF, 24 # Node key field.
.equ TREE_NODE_VALUE_OFF, 26 # Node value field.
.equ TREE_NODE_CHILD_L_OFF, 8 # Node left child field.
.equ TREE_NODE_CHILD_R_OFF, 16 # Node right child field.
.equ TREE_NODE_PARENT_OFF, 0 # Node parent field.
.equ TREE_NODE_COLOR_OFF, 28 # Color field.
# ANCHOR_END: constants

# ANCHOR: entrypoint-branching
.globl entrypoint

entrypoint:
    # Read instruction data length and discriminator.
    # ---------------------------------------------------------------------
    ldxdw r9, [r2 - SIZE_OF_U64] # Get instruction data length.
    ldxdw r8, [r1 + IB_N_ACCOUNTS_OFF] # Get n input buffer accounts.
    ldxb r7, [r2 + OFFSET_ZERO] # Get discriminator.

    # Jump to branch for given discriminator.
    # ---------------------------------------------------------------------
    jeq r7, INSN_DISCRIMINATOR_INSERT, insert
    jeq r7, INSN_DISCRIMINATOR_REMOVE, remove
    jeq r7, INSN_DISCRIMINATOR_INITIALIZE, initialize
    # Error if invalid discriminator provided.
    mov64 r0, E_INSTRUCTION_DISCRIMINATOR
    exit
    # ANCHOR_END: entrypoint-branching

# ANCHOR: initialize-input-checks
initialize:
    # Error if invalid instruction data length.
    # ---------------------------------------------------------------------
    jne r9, SIZE_OF_INITIALIZE_INSTRUCTION, e_instruction_data_len

    # Error if invalid number of accounts.
    # ---------------------------------------------------------------------
    jne r8, IB_N_ACCOUNTS_INIT, e_n_accounts

    # Error if user has data.
    # ---------------------------------------------------------------------
    ldxdw r9, [r1 + IB_USER_DATA_LEN_OFF]
    jne r9, DATA_LEN_ZERO, e_user_data_len

    # Error if tree is duplicate or has data.
    # ---------------------------------------------------------------------
    ldxb r9, [r1 + IB_TREE_NON_DUP_MARKER_OFF]
    jne r9, IB_NON_DUP_MARKER, e_tree_duplicate
    ldxdw r9, [r1 + IB_TREE_DATA_LEN_OFF]
    jne r9, DATA_LEN_ZERO, e_tree_data_len

    # Error if System Program is duplicate or has invalid data length.
    # ---------------------------------------------------------------------
    ldxb r9, [r1 + IB_SYSTEM_PROGRAM_NON_DUP_MARKER_OFF]
    jne r9, IB_NON_DUP_MARKER, e_system_program_duplicate
    ldxdw r9, [r1 + IB_SYSTEM_PROGRAM_DATA_LEN_OFF]
    jne r9, IB_SYSTEM_PROGRAM_DATA_LEN, e_system_program_data_len

    # Error if Rent account is duplicate or has incorrect address.
    # ---------------------------------------------------------------------
    ldxb r9, [r1 + IB_RENT_NON_DUP_MARKER_OFF]
    jne r9, IB_NON_DUP_MARKER, e_rent_duplicate
    ldxdw r9, [r1 + IB_RENT_ADDRESS_OFF_0]
    lddw r8, IB_RENT_ID_CHUNK_0
    jne r9, r8, e_rent_address
    ldxdw r9, [r1 + IB_RENT_ADDRESS_OFF_1]
    lddw r8, IB_RENT_ID_CHUNK_1
    jne r9, r8, e_rent_address
    ldxdw r9, [r1 + IB_RENT_ADDRESS_OFF_2]
    lddw r8, IB_RENT_ID_CHUNK_2
    jne r9, r8, e_rent_address
    ldxdw r9, [r1 + IB_RENT_ADDRESS_OFF_3]
    # Optimize out the following line, which costs two CUs due to two
    # 32-bit immediate loads across two opcodes:
    # ```
    # lddw r8, IB_RENT_ID_CHUNK_3
    # ```
    # Instead, replace with mov32, which only loads one 32-bit immediate,
    # since the rent sysvar address has all chunk 3 hi bits unset.
    mov32 r8, IB_RENT_ID_CHUNK_3_LO
    jne r9, r8, e_rent_address
    # ANCHOR_END: initialize-input-checks

    # ANCHOR: initialize-pda-checks
    # Compute PDA.
    # ---------------------------------------------------------------------
    # Skip assignment for r1, since no seeds need to be parsed and this
    # argument is effectively ignored.
    # ---------------------------------------------------------------------
    mov64 r2, CPI_N_SEEDS_TRY_FIND_PDA # Declare no seeds to parse.
    mov64 r3, r1 # Get input buffer pointer.
    add64 r3, IB_INIT_PROGRAM_ID_OFF_IMM # Point at program ID.
    mov64 r4, r10 # Get stack frame pointer.
    add64 r4, SF_INIT_PDA_OFF # Point to PDA region on stack.
    mov64 r5, r10 # Get stack frame pointer.
    add64 r5, SF_INIT_BUMP_SEED_OFF # Point to bump seed region on stack.
    call sol_try_find_program_address # Find PDA.

    # Compare computed PDA against passed account.
    # ---------------------------------------------------------------------
    ldxdw r9, [r1 + IB_TREE_ADDRESS_OFF_0]
    ldxdw r8, [r4 + PUBKEY_CHUNK_OFF_0]
    jne r9, r8, e_pda_mismatch
    ldxdw r9, [r1 + IB_TREE_ADDRESS_OFF_1]
    ldxdw r8, [r4 + PUBKEY_CHUNK_OFF_1]
    jne r9, r8, e_pda_mismatch
    ldxdw r9, [r1 + IB_TREE_ADDRESS_OFF_2]
    ldxdw r8, [r4 + PUBKEY_CHUNK_OFF_2]
    jne r9, r8, e_pda_mismatch
    ldxdw r9, [r1 + IB_TREE_ADDRESS_OFF_3]
    ldxdw r8, [r4 + PUBKEY_CHUNK_OFF_3]
    jne r9, r8, e_pda_mismatch
    # ANCHOR_END: initialize-pda-checks

    // ANCHOR: initialize-create-account
    # Pack SolInstruction.
    # ---------------------------------------------------------------------
    # Packed later during bulk pointer load operation:
    # - [x] System Program ID pointer.
    # - [x] Account metas pointer.
    # - [x] Instruction data pointer.
    # ---------------------------------------------------------------------
    stdw [r10 + SF_INIT_INSN_ACCOUNT_LEN_OFF], CPI_N_ACCOUNTS
    stdw [r10 + SF_INIT_INSN_DATA_LEN_OFF], CPI_CREATE_ACCOUNT_INSN_DATA_LEN

    # Pack CreateAccount instruction data.
    # ---------------------------------------------------------------------
    # - Discriminator is already set to 0 since stack is zero initialized.
    # - Reuses r3 from PDA syscall.
    # ---------------------------------------------------------------------
    ldxdw r9, [r1 + IB_RENT_DATA_OFF] # Load lamports per byte
    mul64 r9, CPI_ACCOUNT_DATA_SCALAR # Multiply to get rent-exempt cost.
    # Store in instruction data.
    stxdw [r10 + SF_INIT_CREATE_ACCOUNT_LAMPORTS_UOFF], r9
    # Store new account data length.
    stdw [r10 + SF_INIT_CREATE_ACCOUNT_SPACE_UOFF], CPI_TREE_DATA_LEN
    # Copy in program ID to instruction data.
    ldxdw r9, [r3 + PUBKEY_CHUNK_OFF_0]
    stxdw [r10 + SF_INIT_CREATE_ACCOUNT_OWNER_UOFF_0], r9
    ldxdw r9, [r3 + PUBKEY_CHUNK_OFF_1]
    stxdw [r10 + SF_INIT_CREATE_ACCOUNT_OWNER_UOFF_1], r9
    ldxdw r9, [r3 + PUBKEY_CHUNK_OFF_2]
    stxdw [r10 + SF_INIT_CREATE_ACCOUNT_OWNER_UOFF_2], r9
    ldxdw r9, [r3 + PUBKEY_CHUNK_OFF_3]
    stxdw [r10 + SF_INIT_CREATE_ACCOUNT_OWNER_UOFF_3], r9

    # Pack SolAccountMeta for user and tree.
    # ---------------------------------------------------------------------
    # Packed later during bulk pointer load operation:
    # - [x] User pubkey pointer.
    # - [x] Tree pubkey pointer.
    # ---------------------------------------------------------------------
    sth [r10 + SF_INIT_USER_META_IS_WRITABLE_OFF], CPI_WRITABLE_SIGNER
    sth [r10 + SF_INIT_TREE_META_IS_WRITABLE_OFF], CPI_WRITABLE_SIGNER

    # Pack SolAccountInfo for user and tree.
    # ---------------------------------------------------------------------
    # Packed later during bulk pointer load operation:
    # - [x] User pubkey pointer.
    # - [x] Tree pubkey pointer.
    # - [x] User lamports pointer.
    # - [x] Tree lamports pointer.
    # - [x] User data pointer.
    # - [x] Tree data pointer.
    # - [x] User owner pointer.
    # - [x] Tree owner pointer.
    # Skipped due to zero-initialized stack memory:
    # - User data length (already checked as zero).
    # - Tree data length (already checked as zero).
    # - User rent epoch.
    # - Tree rent epoch.
    # - User executable.
    # - Tree executable.
    # ---------------------------------------------------------------------
    sth [r10 + SF_INIT_USER_INFO_IS_SIGNER_OFF], CPI_WRITABLE_SIGNER
    sth [r10 + SF_INIT_TREE_INFO_IS_SIGNER_OFF], CPI_WRITABLE_SIGNER

    # Initialize signer seed for PDA bump seed.
    # ---------------------------------------------------------------------
    # Reuses r5 from PDA derivation syscall.
    # ---------------------------------------------------------------------
    # Store pointer to bump seed.
    stxdw [r10 + SF_INIT_SIGNER_SEED_ADDR_OFF], r5
    stdw [r10 + SF_INIT_SIGNER_SEED_LEN_OFF], SIZE_OF_U8 # Store length.

    # Initialize signers seeds for PDA.
    # ---------------------------------------------------------------------
    # Packed later during bulk pointer load operation:
    # - [x] Signer seed pointer.
    # ---------------------------------------------------------------------
    stdw [r10 + SF_INIT_SIGNERS_SEEDS_LEN_OFF], CPI_N_SEEDS_CREATE_ACCOUNT

    # Bulk assign/load pointers for account metas and infos.
    # ---------------------------------------------------------------------
    # Since pointers must be loaded from registers, this block steps
    # through the input buffer in order to reduce intermediate loads.
    # ---------------------------------------------------------------------
    add64 r1, IB_USER_ADDRESS_OFF # Point to user address in input buffer.
    stxdw [r10 + SF_INIT_USER_META_PUBKEY_OFF], r1 # Store in account meta.
    stxdw [r10 + SF_INIT_USER_INFO_PUBKEY_OFF], r1 # Store in account info.
    add64 r1, SIZE_OF_ADDRESS # Advance to user owner.
    stxdw [r10 + SF_INIT_USER_INFO_OWNER_OFF], r1 # Store in account info.
    add64 r1, SIZE_OF_ADDRESS # Advance to user lamports.
    stxdw [r10 + SF_INIT_USER_INFO_LAMPORTS_OFF], r1 # Store in acct info.
    add64 r1, SIZE_OF_U128 # Advance to user data.
    stxdw [r10 + SF_INIT_USER_INFO_DATA_OFF], r1 # Store in account info.
    # Advance to tree address field.
    add64 r1, IB_USER_DATA_TO_TREE_ADDRESS_REL_OFF_IMM
    stxdw [r10 + SF_INIT_TREE_META_PUBKEY_OFF], r1 # Store in account meta.
    stxdw [r10 + SF_INIT_TREE_INFO_PUBKEY_OFF], r1 # Store in account info.
    add64 r1, SIZE_OF_ADDRESS # Advance to tree owner.
    stxdw [r10 + SF_INIT_TREE_INFO_OWNER_OFF], r1 # Store in account info.
    add64 r1, SIZE_OF_ADDRESS # Advance to tree lamports.
    stxdw [r10 + SF_INIT_TREE_INFO_LAMPORTS_OFF], r1 # Store in acct info.
    add64 r1, SIZE_OF_U128 # Advance to tree data.
    stxdw [r10 + SF_INIT_TREE_INFO_DATA_OFF], r1 # Store in account info.
    mov64 r6, r1 # Store tree data pointer for later.

    # Bulk assign/load pointers for CPI bindings.
    # ---------------------------------------------------------------------
    # This block steps through the stack frame, optimizing assignments in
    # preparation for the impending CreateAccount CPI, which requires:
    # - [x] r1 = pointer to instruction.
    # - [x] r2 = pointer to account infos.
    # - [x] r4 = pointer to signers seeds.
    # Notably, it reuses r4 from the PDA derivation syscall to walk through
    # pointers on the stack, before advancing it to its final value.
    # ---------------------------------------------------------------------
    # Advance to System Program ID pointer on zero-initialized stack.
    add64 r4, SF_INIT_PDA_TO_SYSTEM_PROGRAM_ID_REL_OFF_IMM
    # Store in SolInstruction.
    stxdw [r10 + SF_INIT_INSN_PROGRAM_ID_OFF], r4
    # Advance to SolAccountMeta array pointer.
    add64 r4, SF_INIT_SYSTEM_PROGRAM_ID_TO_ACCT_METAS_REL_OFF_IMM
    stxdw [r10 + SF_INIT_INSN_ACCOUNTS_OFF], r4 # Store in SolInstruction.
    # Advance to instruction data pointer.
    add64 r4, SF_INIT_ACCT_METAS_TO_INSN_DATA_REL_OFF_IMM
    stxdw [r10 + SF_INIT_INSN_DATA_OFF], r4 # Store in SolInstruction.
    # Advance to signer seeds pointer.
    add64 r4, SF_INIT_INSN_DATA_TO_SIGNER_SEEDS_REL_OFF_IMM
    stxdw [r10 + SF_INIT_SIGNERS_SEEDS_ADDR_OFF], r4
    # Advance to signers seeds pointer.
    add64 r4, SF_INIT_SIGNER_SEEDS_TO_SIGNERS_SEEDS_REL_OFF_IMM
    # Assign remaining syscall pointers.
    mov64 r1, r10
    add64 r1, SF_INIT_INSN_PROGRAM_ID_OFF
    mov64 r2, r10
    add64 r2, SF_INIT_ACCT_INFOS_OFF

    # Invoke CPI.
    # ---------------------------------------------------------------------
    mov64 r3, CPI_N_ACCOUNTS
    mov64 r5, CPI_N_PDA_SIGNERS
    call sol_invoke_signed_c

    # Store next pointer in tree header.
    # ---------------------------------------------------------------------
    mov64 r7, r6 # Get copy of tree data pointer.
    add64 r7, SIZE_OF_TREE_HEADER # Advance to next node.
    stxdw [r6 + TREE_HEADER_NEXT_OFF], r7 # Store in next field.

    exit
    // ANCHOR_END: initialize-create-account

# ANCHOR: insert-input-checks
insert:
    # Error if invalid instruction data length.
    # ---------------------------------------------------------------------
    jne r9, SIZE_OF_INSERT_INSTRUCTION, e_instruction_data_len

    # Error if too few accounts.
    # ---------------------------------------------------------------------
    jlt r8, IB_N_ACCOUNTS_GENERAL, e_n_accounts

    # Error if user has data.
    # ---------------------------------------------------------------------
    ldxdw r9, [r1 + IB_USER_DATA_LEN_OFF]
    jne r9, DATA_LEN_ZERO, e_user_data_len

    # Error if tree is duplicate.
    # ---------------------------------------------------------------------
    ldxb r9, [r1 + IB_TREE_NON_DUP_MARKER_OFF]
    jne r9, IB_NON_DUP_MARKER, e_tree_duplicate
    # ANCHOR_END: insert-input-checks

    # ANCHOR: insert-allocate
    # Branch based on state of stack top.
    # ---------------------------------------------------------------------
    # Get stack top pointer.
    ldxdw r9, [r1 + IB_TREE_DATA_TOP_OFF]
    jne r9, NULL, insert_pop # Pop node from stack if non-null.

insert_allocate:
    # Error if wrong number of accounts for allocation.
    # ---------------------------------------------------------------------
    jne r8, IB_N_ACCOUNTS_INIT, e_n_accounts_insert_allocation

    # Compute shifted input buffer pointer based on tree data length.
    # ---------------------------------------------------------------------
    ldxdw r9, [r1 + IB_TREE_DATA_LEN_OFF] # Get tree data length.
    # Store in account info for CPI.
    stxdw [r10 + SF_INIT_TREE_INFO_DATA_LEN_OFF], r9
    mov64 r7, r9 # Store copy for later.
    add64 r9, MAX_DATA_PAD # Add max possible padding.
    and64 r9, DATA_LEN_AND_MASK # Truncate to 8-byte alignment.
    add64 r9, r1 # Increment by input buffer.

    # Check system program is not duplicate and has correct data length.
    # ---------------------------------------------------------------------
    ldxb r8, [r9 + IB_SYSTEM_PROGRAM_NON_DUP_MARKER_OFF]
    jne r8, IB_NON_DUP_MARKER, e_system_program_duplicate
    ldxdw r8, [r9 + IB_SYSTEM_PROGRAM_DATA_LEN_OFF]
    jne r8, IB_SYSTEM_PROGRAM_DATA_LEN, e_system_program_data_len

    # Check rent sysvar is not duplicate and has correct address.
    # ---------------------------------------------------------------------
    ldxb r8, [r9 + IB_RENT_NON_DUP_MARKER_OFF]
    jne r8, IB_NON_DUP_MARKER, e_rent_duplicate
    ldxdw r8, [r9 + IB_RENT_ADDRESS_OFF_0]
    lddw r4, IB_RENT_ID_CHUNK_0
    jne r8, r4, e_rent_address
    ldxdw r8, [r9 + IB_RENT_ADDRESS_OFF_1]
    lddw r4, IB_RENT_ID_CHUNK_1
    jne r8, r4, e_rent_address
    ldxdw r8, [r9 + IB_RENT_ADDRESS_OFF_2]
    lddw r4, IB_RENT_ID_CHUNK_2
    jne r8, r4, e_rent_address
    ldxdw r8, [r9 + IB_RENT_ADDRESS_OFF_3]
    mov32 r4, IB_RENT_ID_CHUNK_3_LO
    jne r8, r4, e_rent_address

    # Calculate transfer lamports.
    # ---------------------------------------------------------------------
    ldxdw r8, [r9 + IB_RENT_DATA_OFF] # Load lamports per byte.
    mul64 r8, SIZE_OF_TREE_NODE # Multiply to get transfer cost.

    # Pack Transfer instruction data in CreateAccount slot on stack.
    # ---------------------------------------------------------------------
    stw [r10 + SF_INIT_CREATE_ACCOUNT_DISCRIMINATOR_UOFF], CPI_TRANSFER_DISCRIMINATOR
    stxdw [r10 + SF_INIT_CREATE_ACCOUNT_LAMPORTS_UOFF], r8

    # Pack SolInstruction.
    # ---------------------------------------------------------------------
    stdw [r10 + SF_INIT_INSN_ACCOUNT_LEN_OFF], CPI_N_ACCOUNTS
    stdw [r10 + SF_INIT_INSN_DATA_LEN_OFF], CPI_TRANSFER_INSN_DATA_LEN

    # Pack SolAccountMeta flags for user and tree.
    # ---------------------------------------------------------------------
    sth [r10 + SF_INIT_USER_META_IS_WRITABLE_OFF], CPI_WRITABLE_SIGNER
    stb [r10 + SF_INIT_TREE_META_IS_WRITABLE_OFF], BOOL_TRUE

    # Pack SolAccountInfo flags for user and tree.
    # ---------------------------------------------------------------------
    sth [r10 + SF_INIT_USER_INFO_IS_SIGNER_OFF], CPI_WRITABLE_SIGNER
    stb [r10 + SF_INIT_TREE_INFO_IS_WRITABLE_UOFF], BOOL_TRUE

    # Bulk assign/load pointers for account metas and infos.
    # ---------------------------------------------------------------------
    mov64 r6, r1 # Store input buffer pointer for later.
    add64 r1, IB_USER_ADDRESS_OFF # Point to user address in input buffer.
    stxdw [r10 + SF_INIT_USER_META_PUBKEY_OFF], r1 # Store in account meta.
    stxdw [r10 + SF_INIT_USER_INFO_PUBKEY_OFF], r1 # Store in account info.
    add64 r1, SIZE_OF_ADDRESS # Advance to user owner.
    stxdw [r10 + SF_INIT_USER_INFO_OWNER_OFF], r1 # Store in account info.
    add64 r1, SIZE_OF_ADDRESS # Advance to user lamports.
    stxdw [r10 + SF_INIT_USER_INFO_LAMPORTS_OFF], r1 # Store in acct info.
    add64 r1, SIZE_OF_U128 # Advance to user data.
    stxdw [r10 + SF_INIT_USER_INFO_DATA_OFF], r1 # Store in account info.
    # Advance to tree address field.
    add64 r1, IB_USER_DATA_TO_TREE_ADDRESS_REL_OFF_IMM
    stxdw [r10 + SF_INIT_TREE_META_PUBKEY_OFF], r1 # Store in account meta.
    stxdw [r10 + SF_INIT_TREE_INFO_PUBKEY_OFF], r1 # Store in account info.
    add64 r1, SIZE_OF_ADDRESS # Advance to tree owner.
    stxdw [r10 + SF_INIT_TREE_INFO_OWNER_OFF], r1 # Store in account info.
    add64 r1, SIZE_OF_ADDRESS # Advance to tree lamports.
    stxdw [r10 + SF_INIT_TREE_INFO_LAMPORTS_OFF], r1 # Store in acct info.
    add64 r1, SIZE_OF_U128 # Advance to tree data.
    stxdw [r10 + SF_INIT_TREE_INFO_DATA_OFF], r1 # Store in account info.

    # Bulk assign/load pointers for CPI bindings.
    # ---------------------------------------------------------------------
    # Point to System Program ID on zero-initialized stack.
    mov64 r4, r10
    add64 r4, SF_INIT_SYSTEM_PROGRAM_ADDRESS_OFF
    # Store in SolInstruction.
    stxdw [r10 + SF_INIT_INSN_PROGRAM_ID_OFF], r4
    # Advance to SolAccountMeta array pointer.
    add64 r4, SF_INIT_SYSTEM_PROGRAM_ID_TO_ACCT_METAS_REL_OFF_IMM
    # Store in SolInstruction.
    stxdw [r10 + SF_INIT_INSN_ACCOUNTS_OFF], r4
    # Advance to instruction data pointer.
    add64 r4, SF_INIT_ACCT_METAS_TO_INSN_DATA_REL_OFF_IMM
    stxdw [r10 + SF_INIT_INSN_DATA_OFF], r4 # Store in SolInstruction.

    # Invoke Transfer CPI.
    # ---------------------------------------------------------------------
    mov64 r1, r10
    add64 r1, SF_INIT_INSN_PROGRAM_ID_OFF
    mov64 r8, r2 # Save instruction data pointer for later.
    mov64 r2, r10
    add64 r2, SF_INIT_ACCT_INFOS_OFF
    mov64 r3, CPI_N_ACCOUNTS
    # Ignore PDA signer seeds pointer, since none required.
    mov64 r5, CPI_N_PDA_SIGNERS_TRANSFER
    call sol_invoke_signed_c
    mov64 r2, r8 # Restore instruction data pointer.
    mov64 r1, r6 # Restore input buffer pointer.

    # Update tree data length.
    # ---------------------------------------------------------------------
    add64 r7, SIZE_OF_TREE_NODE # Increment data length.
    stxdw [r1 + IB_TREE_DATA_LEN_OFF], r7 # Store in input buffer.

    # Get node = next, then advance next by one TreeNode.
    # ---------------------------------------------------------------------
    ldxdw r7, [r1 + IB_TREE_DATA_NEXT_OFF] # Get pointer to next node.
    mov64 r9, r7 # Store node pointer for later, the new node.
    add64 r7, SIZE_OF_TREE_NODE # Increment to point to new next.
    stxdw [r1 + IB_TREE_DATA_NEXT_OFF], r7 # Advance next.

    # Continue insert.
    # ---------------------------------------------------------------------
    ja insert_store_key_value_pair

insert_pop:
    # Pop node from free stack.
    # ---------------------------------------------------------------------
    ldxdw r8, [r9 + OFFSET_ZERO] # Load StackNode.next.
    stxdw [r1 + IB_TREE_DATA_TOP_OFF], r8 # Update top in header.

insert_store_key_value_pair:
    ldxw r4, [r2 + INSN_INSERT_KEY_OFF] # Load two fields together.
    stxw [r9 + TREE_NODE_KEY_OFF], r4 # Store both fields together.
    # ANCHOR_END: insert-allocate

# ANCHOR: insert-search
insert_search:                                                             # r9 = node
    ldxh r4, [r2 + INSN_INSERT_KEY_OFF]                                    # r4 = insn.key;
    ldxdw r3, [r1 + IB_TREE_DATA_ROOT_OFF]                                 # r3 = cursor = root;
    jeq r3, NULL, insert_root

insert_search_loop:
    mov64 r2, r3                                                           # r2 = parent = cursor;
    ldxh r5, [r3 + TREE_NODE_KEY_OFF]                                      # r5 = cursor.key;
    jlt r4, r5, insert_search_branch_l
    jgt r4, r5, insert_search_branch_r
    mov64 r0, E_KEY_EXISTS # Error if key already exists.
    exit

insert_root:
    # Root is null: new node becomes root.
    # ---------------------------------------------------------------------
    stb [r9 + TREE_NODE_COLOR_OFF], TREE_COLOR_R                           # node.color = red;
    mov64 r2, NULL                                                         # r2 = parent = null;
    stxdw [r9 + TREE_NODE_PARENT_OFF], r2                                  # node.parent = null;
    stxdw [r1 + IB_TREE_DATA_ROOT_OFF], r9                                 # root = node;
    exit

insert_search_branch_l:
    ldxdw r3, [r2 + TREE_NODE_CHILD_L_OFF]                                 # r3 = parent.child[L];
    jne r3, NULL, insert_search_loop
    # Null child: insert node as left child of parent.
    stb [r9 + TREE_NODE_COLOR_OFF], TREE_COLOR_R                           # node.color = red;
    stxdw [r9 + TREE_NODE_PARENT_OFF], r2                                  # node.parent = parent;
    stxdw [r2 + TREE_NODE_CHILD_L_OFF], r9                                 # parent.child[L] = node;
    # Inline case 1: if parent is black, tree is valid.
    ldxb r6, [r2 + TREE_NODE_COLOR_OFF]                                    # r6 = parent.color;
    jne r6, TREE_COLOR_B, insert_fixup_check_case_4
    exit

insert_search_branch_r:
    ldxdw r3, [r2 + TREE_NODE_CHILD_R_OFF]                                 # r3 = parent.child[R];
    jne r3, NULL, insert_search_loop
    # Null child: insert node as right child of parent.
    stb [r9 + TREE_NODE_COLOR_OFF], TREE_COLOR_R                           # node.color = red;
    stxdw [r9 + TREE_NODE_PARENT_OFF], r2                                  # node.parent = parent;
    stxdw [r2 + TREE_NODE_CHILD_R_OFF], r9                                 # parent.child[R] = node;
# ANCHOR_END: insert-search

# ANCHOR: insert-fixup-case-1
insert_fixup_main:
                                                                           # r2 := parent
                                                                           # r5 := parent.key
    # Case 1.                                                              # r9 := node
    # ---------------------------------------------------------------------
    ldxb r6, [r2 + TREE_NODE_COLOR_OFF]                                    # r6 = parent.color;
    jne r6, TREE_COLOR_B, insert_fixup_check_case_4
    exit # If parent is black, tree is still valid, so exit.
# ANCHOR_END: insert-fixup-case-1

# ANCHOR: insert-fixup-case-4
insert_fixup_check_case_4:
    # Check case 4.
    # ---------------------------------------------------------------------
    ldxdw r3, [r2 + TREE_NODE_PARENT_OFF]                                  # r3 = grandparent;
    jne r3, NULL, insert_fixup_check_case_5_6
    stb [r2 + TREE_NODE_COLOR_OFF], TREE_COLOR_B                           # parent.color = black;
    exit
# ANCHOR_END: insert-fixup-case-4

# ANCHOR: insert-fixup-case-5-6-dir-l
insert_fixup_check_case_5_6:
    # Get uncle and check for case 5 or 6.
    # ---------------------------------------------------------------------
    ldxh r4, [r3 + TREE_NODE_KEY_OFF]                                      # r4 = grandparent.key;
    jgt r5, r4, insert_fixup_check_case_5_6_dir_r

insert_fixup_check_case_5_6_dir_l:
    ldxdw r7, [r3 + TREE_NODE_CHILD_R_OFF]                                 # r7 = uncle;
    jeq r7, NULL, insert_fixup_case_5_6_dir_l
    ldxb r8, [r7 + TREE_NODE_COLOR_OFF]                                    # r8 = uncle.color;
    jne r8, TREE_COLOR_B, insert_fixup_case_2

insert_fixup_case_5_6_dir_l:
    ldxdw r6, [r2 + TREE_NODE_CHILD_R_OFF]                                 # r6 = new_root = parent.child[R];
    jne r9, r6, insert_fixup_case_6_dir_l

insert_fixup_case_5_dir_l:
    ldxdw r8, [r6 + TREE_NODE_CHILD_L_OFF]                                 # r8 = new_child = new_root.child[L];
    stxdw [r2 + TREE_NODE_CHILD_R_OFF], r8                                 # parent.child[R] = new_child;
    jeq r8, NULL, insert_fixup_case_5_dir_l_skip
    stxdw [r8 + TREE_NODE_PARENT_OFF], r2                                  # new_child.parent = parent;
insert_fixup_case_5_dir_l_skip:
    stxdw [r6 + TREE_NODE_CHILD_L_OFF], r2                                 # new_root.child[L] = parent;
    stxdw [r6 + TREE_NODE_PARENT_OFF], r3                                  # new_root.parent = grandparent;
    stxdw [r2 + TREE_NODE_PARENT_OFF], r6                                  # parent.parent = new_root;
    stxdw [r3 + TREE_NODE_CHILD_L_OFF], r6                                 # grandparent.child[L] = new_root;
    mov64 r9, r2                                                           # node = old parent;
    mov64 r2, r6                                                           # parent = new_root;

insert_fixup_case_6_dir_l:
    ldxdw r4, [r3 + TREE_NODE_PARENT_OFF]                                  # r4 = great-grandparent;
    ldxdw r8, [r2 + TREE_NODE_CHILD_R_OFF]                                 # r8 = new_child = parent.child[R];
    stxdw [r3 + TREE_NODE_CHILD_L_OFF], r8                                 # grandparent.child[L] = new_child;
    jeq r8, NULL, insert_fixup_case_6_dir_l_skip
    stxdw [r8 + TREE_NODE_PARENT_OFF], r3                                  # new_child.parent = grandparent;
insert_fixup_case_6_dir_l_skip:
    stxdw [r2 + TREE_NODE_CHILD_R_OFF], r3                                 # parent.child[R] = grandparent;
    stxdw [r2 + TREE_NODE_PARENT_OFF], r4                                  # parent.parent = great-grandparent;
    stxdw [r3 + TREE_NODE_PARENT_OFF], r2                                  # grandparent.parent = parent;
    jeq r4, NULL, insert_fixup_case_6_dir_l_root
    ldxdw r8, [r4 + TREE_NODE_CHILD_R_OFF]                                 # r8 = great-grandparent.child[R];
    jne r3, r8, insert_fixup_case_6_dir_l_left
    # Inline color + exit to eliminate `ja` (saves 1 CU).
    stxdw [r4 + TREE_NODE_CHILD_R_OFF], r2                                 # great-grandparent.child[R] = parent;
    stb [r2 + TREE_NODE_COLOR_OFF], TREE_COLOR_B                           # parent.color = black;
    stb [r3 + TREE_NODE_COLOR_OFF], TREE_COLOR_R                           # grandparent.color = red;
    exit
insert_fixup_case_6_dir_l_left:
    # Inline color + exit to eliminate `ja` (saves 1 CU).
    stxdw [r4 + TREE_NODE_CHILD_L_OFF], r2                                 # great-grandparent.child[L] = parent;
    stb [r2 + TREE_NODE_COLOR_OFF], TREE_COLOR_B                           # parent.color = black;
    stb [r3 + TREE_NODE_COLOR_OFF], TREE_COLOR_R                           # grandparent.color = red;
    exit
insert_fixup_case_6_dir_l_root:
    stxdw [r1 + IB_TREE_DATA_ROOT_OFF], r2                                 # tree.root = parent;
    stb [r2 + TREE_NODE_COLOR_OFF], TREE_COLOR_B                           # parent.color = black;
    stb [r3 + TREE_NODE_COLOR_OFF], TREE_COLOR_R                           # grandparent.color = red;
    exit
# ANCHOR_END: insert-fixup-case-5-6-dir-l

# ANCHOR: insert-fixup-case-5-6-dir-r
insert_fixup_check_case_5_6_dir_r:
    ldxdw r7, [r3 + TREE_NODE_CHILD_L_OFF]                                 # r7 = uncle;
    jeq r7, NULL, insert_fixup_case_5_6_dir_r
    ldxb r8, [r7 + TREE_NODE_COLOR_OFF]                                    # r8 = uncle.color;
    jne r8, TREE_COLOR_B, insert_fixup_case_2

insert_fixup_case_5_6_dir_r:
    ldxdw r6, [r2 + TREE_NODE_CHILD_L_OFF]                                 # r6 = new_root = parent.child[L];
    jne r9, r6, insert_fixup_case_6_dir_r

insert_fixup_case_5_dir_r:
    ldxdw r8, [r6 + TREE_NODE_CHILD_R_OFF]                                 # r8 = new_child = new_root.child[R];
    stxdw [r2 + TREE_NODE_CHILD_L_OFF], r8                                 # parent.child[L] = new_child;
    jeq r8, NULL, insert_fixup_case_5_dir_r_skip
    stxdw [r8 + TREE_NODE_PARENT_OFF], r2                                  # new_child.parent = parent;
insert_fixup_case_5_dir_r_skip:
    stxdw [r6 + TREE_NODE_CHILD_R_OFF], r2                                 # new_root.child[R] = parent;
    stxdw [r6 + TREE_NODE_PARENT_OFF], r3                                  # new_root.parent = grandparent;
    stxdw [r2 + TREE_NODE_PARENT_OFF], r6                                  # parent.parent = new_root;
    stxdw [r3 + TREE_NODE_CHILD_R_OFF], r6                                 # grandparent.child[R] = new_root;
    mov64 r9, r2                                                           # node = old parent;
    mov64 r2, r6                                                           # parent = new_root;

insert_fixup_case_6_dir_r:
    ldxdw r4, [r3 + TREE_NODE_PARENT_OFF]                                  # r4 = great-grandparent;
    ldxdw r8, [r2 + TREE_NODE_CHILD_L_OFF]                                 # r8 = new_child = parent.child[L];
    stxdw [r3 + TREE_NODE_CHILD_R_OFF], r8                                 # grandparent.child[R] = new_child;
    jeq r8, NULL, insert_fixup_case_6_dir_r_skip
    stxdw [r8 + TREE_NODE_PARENT_OFF], r3                                  # new_child.parent = grandparent;
insert_fixup_case_6_dir_r_skip:
    stxdw [r2 + TREE_NODE_CHILD_L_OFF], r3                                 # parent.child[L] = grandparent;
    stxdw [r2 + TREE_NODE_PARENT_OFF], r4                                  # parent.parent = great-grandparent;
    stxdw [r3 + TREE_NODE_PARENT_OFF], r2                                  # grandparent.parent = parent;
    jeq r4, NULL, insert_fixup_case_6_dir_r_root
    ldxdw r8, [r4 + TREE_NODE_CHILD_R_OFF]                                 # r8 = great-grandparent.child[R];
    jne r3, r8, insert_fixup_case_6_dir_r_left
    # Inline color + exit to eliminate `ja` (saves 1 CU).
    stxdw [r4 + TREE_NODE_CHILD_R_OFF], r2                                 # great-grandparent.child[R] = parent;
    stb [r2 + TREE_NODE_COLOR_OFF], TREE_COLOR_B                           # parent.color = black;
    stb [r3 + TREE_NODE_COLOR_OFF], TREE_COLOR_R                           # grandparent.color = red;
    exit
insert_fixup_case_6_dir_r_left:
    # Inline color + exit to eliminate `ja` (saves 1 CU).
    stxdw [r4 + TREE_NODE_CHILD_L_OFF], r2                                 # great-grandparent.child[L] = parent;
    stb [r2 + TREE_NODE_COLOR_OFF], TREE_COLOR_B                           # parent.color = black;
    stb [r3 + TREE_NODE_COLOR_OFF], TREE_COLOR_R                           # grandparent.color = red;
    exit
insert_fixup_case_6_dir_r_root:
    stxdw [r1 + IB_TREE_DATA_ROOT_OFF], r2                                 # tree.root = parent;
    stb [r2 + TREE_NODE_COLOR_OFF], TREE_COLOR_B                           # parent.color = black;
    stb [r3 + TREE_NODE_COLOR_OFF], TREE_COLOR_R                           # grandparent.color = red;
    exit
# ANCHOR_END: insert-fixup-case-5-6-dir-r

# ANCHOR: insert-fixup-case-2-3
insert_fixup_case_2:
                                                                           # r2 := parent
                                                                           # r3 := grandparent
                                                                           # r7 := uncle
    # Case 2/3.                                                            # r9 := node
    # ---------------------------------------------------------------------
    stb [r2 + TREE_NODE_COLOR_OFF], TREE_COLOR_B                           # parent.color = black;
    stb [r7 + TREE_NODE_COLOR_OFF], TREE_COLOR_B                           # uncle.color = black;
    stb [r3 + TREE_NODE_COLOR_OFF], TREE_COLOR_R                           # grandparent.color = red;
    mov64 r9, r3                                                           # node = grandparent;
    ldxdw r2, [r9 + TREE_NODE_PARENT_OFF]                                  # parent = node.parent;
    jne r2, NULL, insert_fixup_main
    exit # Case 3.
# ANCHOR_END: insert-fixup-case-2-3

# ANCHOR: remove-input-checks
remove:
    # Error if invalid instruction data length.
    # ---------------------------------------------------------------------
    jne r9, SIZE_OF_REMOVE_INSTRUCTION, e_instruction_data_len

    # Error if too few accounts.
    # ---------------------------------------------------------------------
    jlt r8, IB_N_ACCOUNTS_GENERAL, e_n_accounts

    # Error if user has data.
    # ---------------------------------------------------------------------
    ldxdw r9, [r1 + IB_USER_DATA_LEN_OFF]
    jne r9, DATA_LEN_ZERO, e_user_data_len

    # Error if tree is duplicate.
    # ---------------------------------------------------------------------
    ldxb r9, [r1 + IB_TREE_NON_DUP_MARKER_OFF]
    jne r9, IB_NON_DUP_MARKER, e_tree_duplicate
    # ANCHOR_END: remove-input-checks

# ANCHOR: remove-search
remove_search:
    ldxdw r3, [r1 + IB_TREE_DATA_ROOT_OFF]                                 # r3 = node = root;
    jeq r3, NULL, e_key_does_not_exist
    ldxh r4, [r2 + INSN_REMOVE_KEY_OFF]                                    # r4 = key;

remove_search_loop:
    ldxh r5, [r3 + TREE_NODE_KEY_OFF]                                      # r5 = node.key;
    jeq r4, r5, remove_found
    jgt r4, r5, remove_search_r

remove_search_l:
    ldxdw r3, [r3 + TREE_NODE_CHILD_L_OFF]                                 # r3 = node.child[L];
    jne r3, NULL, remove_search_loop
    mov64 r0, E_KEY_DOES_NOT_EXIST
    exit

remove_search_r:
    ldxdw r3, [r3 + TREE_NODE_CHILD_R_OFF]                                 # r3 = node.child[R];
    jne r3, NULL, remove_search_loop

e_key_does_not_exist:
    mov64 r0, E_KEY_DOES_NOT_EXIST
    exit
# ANCHOR_END: remove-search

# ANCHOR: remove-simple-1
remove_found:
    ldxdw r4, [r3 + TREE_NODE_CHILD_L_OFF]                                 # r4 = node.child[L];
    jeq r4, NULL, remove_check_child_r
    ldxdw r5, [r3 + TREE_NODE_CHILD_R_OFF]                                 # r5 = node.child[R];
    jeq r5, NULL, remove_simple_2_child_l

    # Simple case 1: successor swap.
    # ---------------------------------------------------------------------
remove_successor_loop:
    ldxdw r4, [r5 + TREE_NODE_CHILD_L_OFF]                                 # r4 = successor.child[L];
    jeq r4, NULL, remove_successor_copy
    mov64 r5, r4                                                           # successor = left;
    ja remove_successor_loop

remove_successor_copy:
    # Copy key/value pair as u32 from successor to found node.
    # ---------------------------------------------------------------------
    ldxw r4, [r5 + TREE_NODE_KEY_OFF]                                      # r4 = successor.{key,value};
    stxw [r3 + TREE_NODE_KEY_OFF], r4                                      # node.{key,value} = r4;
    mov64 r3, r5                                                           # node = successor;
# ANCHOR_END: remove-simple-1

# ANCHOR: remove-simple-2
remove_check_child_r:                                                      # r3 = node
    ldxdw r4, [r3 + TREE_NODE_CHILD_R_OFF]                                 # r4 = node.child[R];
    jeq r4, NULL, remove_check_simple_3_4

remove_simple_2_child_l:                                                   # r4 = child
    # Simple case 2: replace node with child, recolor child black.
    # ---------------------------------------------------------------------
    ldxdw r5, [r3 + TREE_NODE_PARENT_OFF]                                  # r5 = parent;
    stxdw [r4 + TREE_NODE_PARENT_OFF], r5                                  # child.parent = parent;
    stb [r4 + TREE_NODE_COLOR_OFF], TREE_COLOR_B                           # child.color = black;
    jeq r5, NULL, remove_simple_2_root
    ldxdw r6, [r5 + TREE_NODE_CHILD_R_OFF]                                 # r6 = parent.child[R];
    jne r3, r6, remove_simple_2_dir_l
    stxdw [r5 + TREE_NODE_CHILD_R_OFF], r4                                 # parent.child[R] = child;
    stdw [r3 + TREE_NODE_CHILD_L_OFF], NULL                                # node.child[L] = null;
    stdw [r3 + TREE_NODE_CHILD_R_OFF], NULL                                # node.child[R] = null;
    ldxdw r4, [r1 + IB_TREE_DATA_TOP_OFF]                                  # r4 = header.top;
    stxdw [r3 + TREE_NODE_PARENT_OFF], r4                                  # node.next = top;
    stxdw [r1 + IB_TREE_DATA_TOP_OFF], r3                                  # header.top = node;
    exit

remove_simple_2_dir_l:
    stxdw [r5 + TREE_NODE_CHILD_L_OFF], r4                                 # parent.child[L] = child;
    stdw [r3 + TREE_NODE_CHILD_L_OFF], NULL                                # node.child[L] = null;
    stdw [r3 + TREE_NODE_CHILD_R_OFF], NULL                                # node.child[R] = null;
    ldxdw r4, [r1 + IB_TREE_DATA_TOP_OFF]                                  # r4 = header.top;
    stxdw [r3 + TREE_NODE_PARENT_OFF], r4                                  # node.next = top;
    stxdw [r1 + IB_TREE_DATA_TOP_OFF], r3                                  # header.top = node;
    exit

remove_simple_2_root:
    stxdw [r1 + IB_TREE_DATA_ROOT_OFF], r4                                 # tree.root = child;
    stdw [r3 + TREE_NODE_CHILD_L_OFF], NULL                                # node.child[L] = null;
    stdw [r3 + TREE_NODE_CHILD_R_OFF], NULL                                # node.child[R] = null;
    ldxdw r4, [r1 + IB_TREE_DATA_TOP_OFF]                                  # r4 = header.top;
    stxdw [r3 + TREE_NODE_PARENT_OFF], r4                                  # node.next = top;
    stxdw [r1 + IB_TREE_DATA_TOP_OFF], r3                                  # header.top = node;
    exit
# ANCHOR_END: remove-simple-2

# ANCHOR: remove-simple-3
remove_check_simple_3_4:                                                   # r3 = node
    # Simple case 3: root leaf — clear root.
    # ---------------------------------------------------------------------
    ldxdw r5, [r3 + TREE_NODE_PARENT_OFF]                                  # r5 = parent;
    jne r5, NULL, remove_check_simple_4
    stdw [r1 + IB_TREE_DATA_ROOT_OFF], NULL                                # tree.root = null;
    stdw [r3 + TREE_NODE_CHILD_L_OFF], NULL                                # node.child[L] = null;
    stdw [r3 + TREE_NODE_CHILD_R_OFF], NULL                                # node.child[R] = null;
    ldxdw r4, [r1 + IB_TREE_DATA_TOP_OFF]                                  # r4 = header.top;
    stxdw [r3 + TREE_NODE_PARENT_OFF], r4                                  # node.next = top;
    stxdw [r1 + IB_TREE_DATA_TOP_OFF], r3                                  # header.top = node;
    exit
# ANCHOR_END: remove-simple-3

# ANCHOR: remove-simple-4
remove_check_simple_4:                                                     # r5 = parent
    # Simple case 4: red leaf — detach from parent.
    # ---------------------------------------------------------------------
    ldxb r4, [r3 + TREE_NODE_COLOR_OFF]                                    # r4 = node.color;
    jne r4, TREE_COLOR_R, remove_complex
    ldxdw r4, [r5 + TREE_NODE_CHILD_R_OFF]                                 # r4 = parent.child[R];
    jne r3, r4, remove_simple_4_dir_l
    stdw [r5 + TREE_NODE_CHILD_R_OFF], NULL                                # parent.child[R] = null;
    stdw [r3 + TREE_NODE_CHILD_L_OFF], NULL                                # node.child[L] = null;
    stdw [r3 + TREE_NODE_CHILD_R_OFF], NULL                                # node.child[R] = null;
    ldxdw r4, [r1 + IB_TREE_DATA_TOP_OFF]                                  # r4 = header.top;
    stxdw [r3 + TREE_NODE_PARENT_OFF], r4                                  # node.next = top;
    stxdw [r1 + IB_TREE_DATA_TOP_OFF], r3                                  # header.top = node;
    exit

remove_simple_4_dir_l:
    stdw [r5 + TREE_NODE_CHILD_L_OFF], NULL                                # parent.child[L] = null;
    stdw [r3 + TREE_NODE_CHILD_L_OFF], NULL                                # node.child[L] = null;
    stdw [r3 + TREE_NODE_CHILD_R_OFF], NULL                                # node.child[R] = null;
    ldxdw r4, [r1 + IB_TREE_DATA_TOP_OFF]                                  # r4 = header.top;
    stxdw [r3 + TREE_NODE_PARENT_OFF], r4                                  # node.next = top;
    stxdw [r1 + IB_TREE_DATA_TOP_OFF], r3                                  # header.top = node;
    exit
# ANCHOR_END: remove-simple-4

# ANCHOR: remove-complex
remove_complex:
    mov64 r2, r3                                                           # r2 = deleted node (save for recycle);
    ldxdw r5, [r3 + TREE_NODE_PARENT_OFF]                                  # r5 = parent;
    ldxdw r4, [r5 + TREE_NODE_CHILD_L_OFF]                                 # r4 = parent.child[L];
    jne r3, r4, remove_complex_dir_r                                       # if node != parent.child[L] goto remove_complex_dir_r
remove_complex_dir_l:
    # Prepare to loop for when node direction is left.
    # ---------------------------------------------------------------------
    stdw [r5 + TREE_NODE_CHILD_L_OFF], NULL                                # parent.child[L] = null;
    ja remove_complex_loop_dir_l                                           # goto remove_complex_loop_dir_l
remove_complex_dir_r:
    # Prepare to loop for when node direction is right.
    # ---------------------------------------------------------------------
    stdw [r5 + TREE_NODE_CHILD_R_OFF], NULL                                # parent.child[R] = null;
remove_complex_loop_dir_r:
    # Rebalance loop for when node direction is right.
    # ---------------------------------------------------------------------
    ldxdw r6, [r5 + TREE_NODE_CHILD_L_OFF]                                 # r6 = sibling = parent.child[L];
    ldxdw r7, [r6 + TREE_NODE_CHILD_L_OFF]                                 # r7 = distant_nephew = sibling.child[L];
    ldxdw r8, [r6 + TREE_NODE_CHILD_R_OFF]                                 # r8 = close_nephew = sibling.child[R];
    # Check for red sibling.
    # ---------------------------------------------------------------------
    ldxb r9, [r6 + TREE_NODE_COLOR_OFF]                                    # r9 = sibling.color;
    jeq r9, TREE_COLOR_R, remove_complex_loop_dir_r_sibling_red            # if sibling.color == red goto remove_complex_loop_dir_r_sibling_red
    # Check for red distant nephew.
    # ---------------------------------------------------------------------
    jeq r7, NULL, remove_complex_loop_dir_r_check_case_5                   # if distant_nephew == null goto remove_complex_loop_dir_r_check_case_5
    ldxb r9, [r7 + TREE_NODE_COLOR_OFF]                                    # r9 = distant_nephew.color;
    jeq r9, TREE_COLOR_R, remove_complex_case_6_dir_r                      # if distant_nephew.color == red goto remove_complex_case_6_dir_r
remove_complex_loop_dir_r_check_case_5:
    # Check for red close nephew.
    # ---------------------------------------------------------------------
    jeq r8, NULL, remove_complex_loop_dir_r_check_case_1                   # if close_nephew == null goto remove_complex_loop_dir_r_check_case_1
    ldxb r9, [r8 + TREE_NODE_COLOR_OFF]                                    # r9 = close_nephew.color;
    jeq r9, TREE_COLOR_R, remove_complex_case_5_dir_r                      # if close_nephew.color == red goto remove_complex_case_5_dir_r
remove_complex_loop_dir_r_check_case_1:
    # Check for no parent.
    # ---------------------------------------------------------------------
    jeq r5, NULL, remove_complex_recycle_node                              # if parent == null goto remove_complex_recycle_node;
    # Check for red parent.
    # ---------------------------------------------------------------------
    ldxb r9, [r5 + TREE_NODE_COLOR_OFF]                                    # r9 = parent.color;
    jne r9, TREE_COLOR_R, remove_complex_loop_dir_r_case_2                 # if parent.color != red goto remove_complex_loop_dir_r_case_2
    stb [r6 + TREE_NODE_COLOR_OFF], TREE_COLOR_R                           # sibling.color = red;
    stb [r5 + TREE_NODE_COLOR_OFF], TREE_COLOR_B                           # parent.color = black;
    ja remove_complex_recycle_node                                         # goto remove_complex_recycle_node;
remove_complex_loop_dir_r_case_2:
    # Fall through to case 2 at end of loop.
    # ---------------------------------------------------------------------
    stb [r6 + TREE_NODE_COLOR_OFF], TREE_COLOR_R                           # sibling.color = red;
    mov64 r3, r5                                                           # node = parent;
    ldxdw r5, [r3 + TREE_NODE_PARENT_OFF]                                  # parent = node.parent;
    # Check if parent exists, and if so, get direction for next loop.
    # ---------------------------------------------------------------------
    jeq r5, NULL, remove_complex_case_5_dir_r                              # if parent == null goto remove_complex_case_5_dir_r
    ldxdw r4, [r5 + TREE_NODE_CHILD_L_OFF]                                 # r4 = parent.child[L]
    jeq r3, r4, remove_complex_loop_dir_l                                  # if node == parent.child[L] goto remove_complex_loop_dir_l
    ja remove_complex_loop_dir_r                                           # goto remove_complex_loop_dir_r
remove_complex_case_5_dir_r:
    # rotate_subtree(tree, sibling, 1-dir)
    stb [r6 + TREE_NODE_COLOR_OFF], TREE_COLOR_R                           # sibling.color = red;
    stb [r8 + TREE_NODE_COLOR_OFF], TREE_COLOR_B                           # close_nephew.color = black;
    mov64 r7, r6                                                           # distant_nephew = sibling;
    mov64 r6, r8                                                           # sibling = close_nephew;
remove_complex_case_6_dir_r:
    # rotate_subtree(tree, parent, dir)
    ldxb r4, [r5 + TREE_NODE_COLOR_OFF]                                    # r4 = parent.color;
    stxb [r6 + TREE_NODE_COLOR_OFF], r4                                    # sibling.color = parent.color;
    stb [r5 + TREE_NODE_COLOR_OFF], TREE_COLOR_B                           # parent.color = black;
    stb [r7 + TREE_NODE_COLOR_OFF], TREE_COLOR_B                           # distant_nephew.color = black;
    ja remove_complex_recycle_node
remove_complex_loop_dir_r_sibling_red:
    # Rebalance for red sibling case.
    # ---------------------------------------------------------------------
    # rotate_subtree(tree, parent, dir)
    stb [r5 + TREE_NODE_COLOR_OFF], TREE_COLOR_R                           # parent.color = red;
    stb [r6 + TREE_NODE_COLOR_OFF], TREE_COLOR_B                           # sibling.color = black;
    mov64 r6, r8                                                           # sibling = close_nephew;
    ldxdw r4, [r6 + TREE_NODE_CHILD_L_OFF]                                 # r4 = sibling.child[L];
    mov64 r7, r4                                                           # distant_nephew = sibling.child[L];
    jeq r7, NULL, remove_complex_loop_dir_r_sibling_red_check_case_5       # if distant_nephew == null goto remove_complex_loop_dir_r_sibling_red_check_case_5
    ldxb r4, [r7 + TREE_NODE_COLOR_OFF]                                    # r4 = distant_nephew.color;
    jeq r4, TREE_COLOR_R, remove_complex_case_6_dir_r                      # if distant_nephew.color == red goto remove_complex_case_6_dir_r
remove_complex_loop_dir_r_sibling_red_check_case_5:
    ldxdw r4, [r6 + TREE_NODE_CHILD_R_OFF]                                 # r4 = sibling.child[R];
    mov64 r8, r4                                                           # close_nephew = sibling.child[R];
    jeq r8, NULL, remove_complex_loop_dir_r_sibling_red_case_4             # if close_nephew == null goto remove_complex_loop_dir_r_sibling_red_case_4
    ldxb r4, [r8 + TREE_NODE_COLOR_OFF]                                    # r4 = close_nephew.color;
    jeq r4, TREE_COLOR_R, remove_complex_case_5_dir_r                      # if close_nephew.color == red goto remove_complex_case_5_dir_r
remove_complex_loop_dir_r_sibling_red_case_4:
    stb [r6 + TREE_NODE_COLOR_OFF], TREE_COLOR_R                           # sibling.color = red;
    stb [r5 + TREE_NODE_COLOR_OFF], TREE_COLOR_B                           # parent.color = black;
    ja remove_complex_recycle_node

remove_complex_loop_dir_l:
    # Mirror of remove_complex_loop_r.
    # Needs all the other sub labels for the same cases, but with dir reversed.

remove_complex_recycle_node:
    stdw [r2 + TREE_NODE_CHILD_L_OFF], NULL                                # node.child[L] = null;
    stdw [r2 + TREE_NODE_CHILD_R_OFF], NULL                                # node.child[R] = null;
    ldxdw r4, [r1 + IB_TREE_DATA_TOP_OFF]                                  # r4 = header.top;
    stxdw [r2 + TREE_NODE_PARENT_OFF], r4                                  # node.next = top;
    stxdw [r1 + IB_TREE_DATA_TOP_OFF], r2                                  # header.top = node;
    exit
# ANCHOR_END: remove-complex

e_instruction_data:
    mov64 r0, E_INSTRUCTION_DATA
    exit

e_instruction_data_len:
    mov64 r0, E_INSTRUCTION_DATA_LEN
    exit

e_n_accounts:
    mov64 r0, E_N_ACCOUNTS
    exit

e_n_accounts_insert_allocation:
    mov64 r0, E_N_ACCOUNTS_INSERT_ALLOCATION
    exit

e_pda_mismatch:
    mov64 r0, E_PDA_MISMATCH
    exit

e_rent_address:
    mov64 r0, E_RENT_ADDRESS
    exit

e_rent_duplicate:
    mov64 r0, E_RENT_DUPLICATE
    exit

e_system_program_data_len:
    mov64 r0, E_SYSTEM_PROGRAM_DATA_LEN
    exit

e_system_program_duplicate:
    mov64 r0, E_SYSTEM_PROGRAM_DUPLICATE
    exit

e_tree_data_len:
    mov64 r0, E_TREE_DATA_LEN
    exit

e_tree_duplicate:
    mov64 r0, E_TREE_DUPLICATE
    exit

e_user_data_len:
    mov64 r0, E_USER_DATA_LEN
    exit
