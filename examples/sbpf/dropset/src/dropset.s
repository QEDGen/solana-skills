.globl entrypoint

# Included in order of dependency.
# --- begin common/discriminant.s ---
.equ DISC_REGISTER_MARKET, 0 # Register a new market.
# --- end common/discriminant.s ---
# --- begin common/error.s ---
# The instruction's discriminant does not match any known variant.
.equ E_INVALID_DISCRIMINANT, 1
# The instruction data length is invalid.
.equ E_INVALID_INSTRUCTION_LENGTH, 2
# The number of accounts provided is invalid for the given instruction.
.equ E_INVALID_NUMBER_OF_ACCOUNTS, 3
.equ E_USER_HAS_DATA, 4 # The user account already has data.
.equ E_MARKET_ACCOUNT_IS_DUPLICATE, 5 # The market account is a duplicate.
.equ E_MARKET_HAS_DATA, 6 # The market account already has data.
.equ E_BASE_MINT_IS_DUPLICATE, 7 # The base mint account is a duplicate.
.equ E_QUOTE_MINT_IS_DUPLICATE, 8 # The quote mint account is a duplicate.
.equ E_INVALID_MARKET_PUBKEY, 9 # The market account pubkey is invalid.
# The System Program account is a duplicate.
.equ E_SYSTEM_PROGRAM_IS_DUPLICATE, 10
# The System Program account pubkey is invalid.
.equ E_INVALID_SYSTEM_PROGRAM_PUBKEY, 11
# The Rent sysvar account is a duplicate.
.equ E_RENT_SYSVAR_IS_DUPLICATE, 12
# The Rent sysvar account pubkey is invalid.
.equ E_INVALID_RENT_SYSVAR_PUBKEY, 13

e_invalid_instruction_length:
    mov32 r0, E_INVALID_INSTRUCTION_LENGTH
    exit

e_invalid_number_of_accounts:
    mov32 r0, E_INVALID_NUMBER_OF_ACCOUNTS
    exit

e_user_has_data:
    mov32 r0, E_USER_HAS_DATA
    exit

e_market_account_is_duplicate:
    mov32 r0, E_MARKET_ACCOUNT_IS_DUPLICATE
    exit

e_market_has_data:
    mov32 r0, E_MARKET_HAS_DATA
    exit

e_base_mint_is_duplicate:
    mov32 r0, E_BASE_MINT_IS_DUPLICATE
    exit

e_quote_mint_is_duplicate:
    mov32 r0, E_QUOTE_MINT_IS_DUPLICATE
    exit

e_invalid_market_pubkey:
    mov32 r0, E_INVALID_MARKET_PUBKEY
    exit

e_system_program_is_duplicate:
    mov32 r0, E_SYSTEM_PROGRAM_IS_DUPLICATE
    exit

e_invalid_system_program_pubkey:
    mov32 r0, E_INVALID_SYSTEM_PROGRAM_PUBKEY
    exit

e_rent_sysvar_is_duplicate:
    mov32 r0, E_RENT_SYSVAR_IS_DUPLICATE
    exit

e_invalid_rent_sysvar_pubkey:
    mov32 r0, E_INVALID_RENT_SYSVAR_PUBKEY
    exit
# --- end common/error.s ---
# --- begin common/memory.s ---
# Assorted runtime account constants.
# -------------------------------------------------------------------------
.equ ACCT_DUPLICATE_OFF, 0 # Borrow state / duplicate marker.
.equ ACCT_IS_SIGNER_OFF, 1 # Whether the account is a signer.
.equ ACCT_IS_WRITABLE_OFF, 2 # Whether the account is writable.
.equ ACCT_EXECUTABLE_OFF, 3 # Whether the account is executable.
.equ ACCT_RESIZE_DELTA_OFF, 4 # Resize delta.
.equ ACCT_ADDRESS_OFF, 8 # Account address.
.equ ACCT_ADDRESS_CHUNK_0_OFF, 8 # Account address (chunk 0).
.equ ACCT_ADDRESS_CHUNK_1_OFF, 16 # Account address (chunk 1).
.equ ACCT_ADDRESS_CHUNK_2_OFF, 24 # Account address (chunk 2).
.equ ACCT_ADDRESS_CHUNK_3_OFF, 32 # Account address (chunk 3).
.equ ACCT_OWNER_OFF, 40 # Account owner.
.equ ACCT_OWNER_CHUNK_0_OFF, 40 # Account owner (chunk 0).
.equ ACCT_OWNER_CHUNK_1_OFF, 48 # Account owner (chunk 1).
.equ ACCT_OWNER_CHUNK_2_OFF, 56 # Account owner (chunk 2).
.equ ACCT_OWNER_CHUNK_3_OFF, 64 # Account owner (chunk 3).
.equ ACCT_DATA_LEN_OFF, 80 # Account data length.
.equ ACCT_DATA_OFF, 88 # Account data start.
.equ ACCT_NON_DUP_MARKER, 255 # Non-dup marker for accounts.
# Account storage overhead for rent calculation.
.equ ACCT_STORAGE_OVERHEAD, 128
# -------------------------------------------------------------------------

# CPI-related constants.
# -------------------------------------------------------------------------
# Mask for writable signer (is_writable | is_signer).
.equ CPI_WRITABLE_SIGNER, 257
# -------------------------------------------------------------------------

# Common data-related constants.
# -------------------------------------------------------------------------
.equ DATA_LEN_ZERO, 0 # Data length of zero.
.equ DATA_BPF_ALIGN_OF_U128, 8 # Data alignment during runtime.
# Maximum possible data length padding for a runtime account.
.equ DATA_LEN_MAX_PAD, 7
.equ DATA_LEN_AND_MASK, -8 # And mask for data length alignment.
# -------------------------------------------------------------------------

# Input buffer constants for static header.
# -------------------------------------------------------------------------
.equ IB_USER_DATA_LEN_OFF, 88 # From input buffer to user data length.
.equ IB_USER_PUBKEY_OFF, 16 # From input buffer to user pubkey.
# From input buffer to user pubkey (chunk 0).
.equ IB_USER_PUBKEY_CHUNK_0_OFF, 16
# From input buffer to user pubkey (chunk 1).
.equ IB_USER_PUBKEY_CHUNK_1_OFF, 24
# From input buffer to user pubkey (chunk 2).
.equ IB_USER_PUBKEY_CHUNK_2_OFF, 32
# From input buffer to user pubkey (chunk 3).
.equ IB_USER_PUBKEY_CHUNK_3_OFF, 40
# From input buffer to market duplicate flag.
.equ IB_MARKET_DUPLICATE_OFF, 10344
# From input buffer to market data length.
.equ IB_MARKET_DATA_LEN_OFF, 10424
.equ IB_MARKET_PUBKEY_OFF, 10352 # From input buffer to market address.
# From input buffer to market address (chunk 0).
.equ IB_MARKET_PUBKEY_CHUNK_0_OFF, 10352
# From input buffer to market address (chunk 1).
.equ IB_MARKET_PUBKEY_CHUNK_1_OFF, 10360
# From input buffer to market address (chunk 2).
.equ IB_MARKET_PUBKEY_CHUNK_2_OFF, 10368
# From input buffer to market address (chunk 3).
.equ IB_MARKET_PUBKEY_CHUNK_3_OFF, 10376
# From address to owner in a runtime account.
.equ IB_ADDRESS_TO_OWNER_REL_OFF_IMM, 32
# From owner to lamports in a runtime account.
.equ IB_OWNER_TO_LAMPORTS_REL_OFF_IMM, 32
# From lamports to data start in a runtime account.
.equ IB_LAMPORTS_TO_DATA_REL_OFF_IMM, 16
# From user data to market address in the input buffer.
.equ IB_USER_DATA_TO_MARKET_ADDRESS_REL_OFF_IMM, 10256
# -------------------------------------------------------------------------

.equ SIZE_OF_U8, 1 # Size of u8 in bytes.
.equ SIZE_OF_ADDRESS, 32 # Size of Address in bytes.
.equ SIZE_OF_EMPTY_ACCOUNT, 10336 # Size of EmptyAccount in bytes.
.equ SIZE_OF_MARKET_HEADER, 40 # Size of MarketHeader in bytes.
.equ SIZE_OF_CREATE_ACCOUNT_DATA, 56 # Size of CreateAccountData in bytes.
# --- end common/memory.s ---
# --- begin common/pubkey.s ---
# Pubkey constants.
# -------------------------------------------------------------------------
.equ PUBKEY_CHUNK_0_OFF, 0 # Offset for the first 8 bytes.
.equ PUBKEY_CHUNK_1_OFF, 8 # Offset for the second 8 bytes.
.equ PUBKEY_CHUNK_2_OFF, 16 # Offset for the third 8 bytes.
.equ PUBKEY_CHUNK_3_OFF, 24 # Offset for the fourth 8 bytes.
.equ PUBKEY_RENT_CHUNK_0, 5862609301215225606 # Rent sysvar ID (chunk 0).
.equ PUBKEY_RENT_CHUNK_0_LO, 399877894 # Rent sysvar ID (chunk 0 lo).
.equ PUBKEY_RENT_CHUNK_0_HI, 1364995097 # Rent sysvar ID (chunk 0 hi).
.equ PUBKEY_RENT_CHUNK_1, 9219231539345853473 # Rent sysvar ID (chunk 1).
.equ PUBKEY_RENT_CHUNK_1_LO, 1288277025 # Rent sysvar ID (chunk 1 lo).
.equ PUBKEY_RENT_CHUNK_1_HI, 2146519613 # Rent sysvar ID (chunk 1 hi).
.equ PUBKEY_RENT_CHUNK_2, 4971307250928769624 # Rent sysvar ID (chunk 2).
.equ PUBKEY_RENT_CHUNK_2_LO, 149871192 # Rent sysvar ID (chunk 2 lo).
.equ PUBKEY_RENT_CHUNK_2_HI, 1157472667 # Rent sysvar ID (chunk 2 hi).
.equ PUBKEY_RENT_CHUNK_3, 2329533411 # Rent sysvar ID (chunk 3).
.equ PUBKEY_RENT_CHUNK_3_LO, -1965433885 # Rent sysvar ID (chunk 3 lo).
.equ PUBKEY_RENT_CHUNK_3_HI, 0 # Rent sysvar ID (chunk 3 hi).
# -------------------------------------------------------------------------
# --- end common/pubkey.s ---
# --- begin entrypoint.s ---
# Offset from input buffer to number of accounts, in input buffer.
.equ IB_N_ACCTS_OFF, 0
# Offset from instruction data to instruction data length, in input buffer.
.equ INSN_LEN_OFF, -8
# Offset from instruction data to discriminant, in input buffer.
.equ INSN_DISC_OFF, 0

entrypoint:
    # n_accounts = input.n_accounts
    ldxdw r3, [r1 + IB_N_ACCTS_OFF]
    # insn_len = insn.length
    ldxdw r4, [r2 + INSN_LEN_OFF]
    # insn_disc = insn.discriminant
    ldxb r5, [r2 + INSN_DISC_OFF]
    # if insn_disc == Discriminant::RegisterMarket return REGISTER-MARKET
    jeq r5, DISC_REGISTER_MARKET, register_market
    # return ErrorCode::InvalidDiscriminant
    mov32 r0, E_INVALID_DISCRIMINANT
    exit
# --- end entrypoint.s ---
# --- begin market/register.s ---
# RegisterMarketData instruction data length.
.equ REGISTER_MARKET_DATA_LEN, 1

# RegisterMarketAccounts number of accounts.
.equ REGISTER_MARKET_ACCOUNTS_LEN, 10

# Stack frame for REGISTER-MARKET.
# -------------------------------------------------------------------------
.equ RM_FM_PDA_SEEDS_OFF, -664 # Signer seeds offset.
.equ RM_FM_PDA_SEEDS_N_SEEDS, 3 # Number of signer seeds.
.equ RM_FM_PDA_SEEDS_BASE_ADDR_OFF, -664 # Base signer seed address.
.equ RM_FM_PDA_SEEDS_BASE_LEN_OFF, -656 # Base signer seed length.
.equ RM_FM_PDA_SEEDS_QUOTE_ADDR_OFF, -648 # Quote signer seed address.
.equ RM_FM_PDA_SEEDS_QUOTE_LEN_OFF, -640 # Quote signer seed length.
.equ RM_FM_PDA_SEEDS_BUMP_ADDR_OFF, -632 # Bump signer seed address.
.equ RM_FM_PDA_SEEDS_BUMP_LEN_OFF, -624 # Bump signer seed length.
.equ RM_FM_PDA_OFF, -616 # PDA address.
.equ RM_FM_PDA_CHUNK_0_OFF, -616 # PDA address (chunk 0).
.equ RM_FM_PDA_CHUNK_1_OFF, -608 # PDA address (chunk 1).
.equ RM_FM_PDA_CHUNK_2_OFF, -600 # PDA address (chunk 2).
.equ RM_FM_PDA_CHUNK_3_OFF, -592 # PDA address (chunk 3).
.equ RM_FM_SYSTEM_PROGRAM_PUBKEY_OFF, -584 # System Program pubkey.
# System Program pubkey (chunk 0).
.equ RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_0_OFF, -584
# System Program pubkey (chunk 1).
.equ RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_1_OFF, -576
# System Program pubkey (chunk 2).
.equ RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_2_OFF, -568
# System Program pubkey (chunk 3).
.equ RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_3_OFF, -560
.equ RM_FM_CREATE_ACCT_DATA_OFF, -552 # CreateAccount instruction data.
# Lamports field within CreateAccount instruction data.
.equ RM_FM_CREATE_ACCT_LAMPORTS_UOFF, -548
# Space field within CreateAccount instruction data.
.equ RM_FM_CREATE_ACCT_SPACE_UOFF, -540
# Owner field within CreateAccount instruction data.
.equ RM_FM_CREATE_ACCT_OWNER_UOFF, -532
# Owner field within CreateAccount instruction data (chunk 0).
.equ RM_FM_CREATE_ACCT_OWNER_CHUNK_0_UOFF, -532
# Owner field within CreateAccount instruction data (chunk 1).
.equ RM_FM_CREATE_ACCT_OWNER_CHUNK_1_UOFF, -524
# Owner field within CreateAccount instruction data (chunk 2).
.equ RM_FM_CREATE_ACCT_OWNER_CHUNK_2_UOFF, -516
# Owner field within CreateAccount instruction data (chunk 3).
.equ RM_FM_CREATE_ACCT_OWNER_CHUNK_3_UOFF, -508
.equ RM_FM_CPI_N_ACCOUNTS, 6 # Number of CPI accounts.
.equ RM_FM_CPI_SOL_ACCT_INFO_OFF, -496 # Start of SolAccountInfo vector.
.equ RM_FM_CPI_SOL_ACCT_META_OFF, -160 # Start of SolAccountMeta vector.
.equ RM_FM_CPI_USER_ACCT_INFO_KEY_UOFF, -496 # User account info key.
# User account info lamports.
.equ RM_FM_CPI_USER_ACCT_INFO_LAMPORTS_UOFF, -488
# User account info data length.
.equ RM_FM_CPI_USER_ACCT_INFO_DATA_LEN_UOFF, -480
.equ RM_FM_CPI_USER_ACCT_INFO_DATA_UOFF, -472 # User account info data.
.equ RM_FM_CPI_USER_ACCT_INFO_OWNER_UOFF, -464 # User account info owner.
# User account info rent epoch.
.equ RM_FM_CPI_USER_ACCT_INFO_RENT_EPOCH_UOFF, -456
# User account info is signer.
.equ RM_FM_CPI_USER_ACCT_INFO_IS_SIGNER_UOFF, -448
# User account info is writable.
.equ RM_FM_CPI_USER_ACCT_INFO_IS_WRITABLE_UOFF, -447
# User account info executable.
.equ RM_FM_CPI_USER_ACCT_INFO_EXECUTABLE_UOFF, -446
.equ RM_FM_CPI_USER_ACCT_META_PUBKEY_UOFF, -160 # User account meta pubkey.
# User account meta is writable.
.equ RM_FM_CPI_USER_ACCT_META_IS_WRITABLE_UOFF, -152
# User account meta is signer.
.equ RM_FM_CPI_USER_ACCT_META_IS_SIGNER_UOFF, -151
.equ RM_FM_CPI_TARGET_ACCT_INFO_KEY_UOFF, -440 # Target account info key.
# Target account info lamports.
.equ RM_FM_CPI_TARGET_ACCT_INFO_LAMPORTS_UOFF, -432
# Target account info data length.
.equ RM_FM_CPI_TARGET_ACCT_INFO_DATA_LEN_UOFF, -424
.equ RM_FM_CPI_TARGET_ACCT_INFO_DATA_UOFF, -416 # Target account info data.
# Target account info owner.
.equ RM_FM_CPI_TARGET_ACCT_INFO_OWNER_UOFF, -408
# Target account info rent epoch.
.equ RM_FM_CPI_TARGET_ACCT_INFO_RENT_EPOCH_UOFF, -400
# Target account info is signer.
.equ RM_FM_CPI_TARGET_ACCT_INFO_IS_SIGNER_UOFF, -392
# Target account info is writable.
.equ RM_FM_CPI_TARGET_ACCT_INFO_IS_WRITABLE_UOFF, -391
# Target account info executable.
.equ RM_FM_CPI_TARGET_ACCT_INFO_EXECUTABLE_UOFF, -390
# Target account meta pubkey.
.equ RM_FM_CPI_TARGET_ACCT_META_PUBKEY_UOFF, -144
# Target account meta is writable.
.equ RM_FM_CPI_TARGET_ACCT_META_IS_WRITABLE_UOFF, -136
# Target account meta is signer.
.equ RM_FM_CPI_TARGET_ACCT_META_IS_SIGNER_UOFF, -135
# Proprietor account info key.
.equ RM_FM_CPI_PROPRIETOR_ACCT_INFO_KEY_UOFF, -384
# Proprietor account info lamports.
.equ RM_FM_CPI_PROPRIETOR_ACCT_INFO_LAMPORTS_UOFF, -376
# Proprietor account info data length.
.equ RM_FM_CPI_PROPRIETOR_ACCT_INFO_DATA_LEN_UOFF, -368
# Proprietor account info data.
.equ RM_FM_CPI_PROPRIETOR_ACCT_INFO_DATA_UOFF, -360
# Proprietor account info owner.
.equ RM_FM_CPI_PROPRIETOR_ACCT_INFO_OWNER_UOFF, -352
# Proprietor account info rent epoch.
.equ RM_FM_CPI_PROPRIETOR_ACCT_INFO_RENT_EPOCH_UOFF, -344
# Proprietor account info is signer.
.equ RM_FM_CPI_PROPRIETOR_ACCT_INFO_IS_SIGNER_UOFF, -336
# Proprietor account info is writable.
.equ RM_FM_CPI_PROPRIETOR_ACCT_INFO_IS_WRITABLE_UOFF, -335
# Proprietor account info executable.
.equ RM_FM_CPI_PROPRIETOR_ACCT_INFO_EXECUTABLE_UOFF, -334
# Proprietor account meta pubkey.
.equ RM_FM_CPI_PROPRIETOR_ACCT_META_PUBKEY_UOFF, -128
# Proprietor account meta is writable.
.equ RM_FM_CPI_PROPRIETOR_ACCT_META_IS_WRITABLE_UOFF, -120
# Proprietor account meta is signer.
.equ RM_FM_CPI_PROPRIETOR_ACCT_META_IS_SIGNER_UOFF, -119
.equ RM_FM_CPI_MINT_ACCT_INFO_KEY_UOFF, -328 # Mint account info key.
# Mint account info lamports.
.equ RM_FM_CPI_MINT_ACCT_INFO_LAMPORTS_UOFF, -320
# Mint account info data length.
.equ RM_FM_CPI_MINT_ACCT_INFO_DATA_LEN_UOFF, -312
.equ RM_FM_CPI_MINT_ACCT_INFO_DATA_UOFF, -304 # Mint account info data.
.equ RM_FM_CPI_MINT_ACCT_INFO_OWNER_UOFF, -296 # Mint account info owner.
# Mint account info rent epoch.
.equ RM_FM_CPI_MINT_ACCT_INFO_RENT_EPOCH_UOFF, -288
# Mint account info is signer.
.equ RM_FM_CPI_MINT_ACCT_INFO_IS_SIGNER_UOFF, -280
# Mint account info is writable.
.equ RM_FM_CPI_MINT_ACCT_INFO_IS_WRITABLE_UOFF, -279
# Mint account info executable.
.equ RM_FM_CPI_MINT_ACCT_INFO_EXECUTABLE_UOFF, -278
.equ RM_FM_CPI_MINT_ACCT_META_PUBKEY_UOFF, -112 # Mint account meta pubkey.
# Mint account meta is writable.
.equ RM_FM_CPI_MINT_ACCT_META_IS_WRITABLE_UOFF, -104
# Mint account meta is signer.
.equ RM_FM_CPI_MINT_ACCT_META_IS_SIGNER_UOFF, -103
# System Program account info key.
.equ RM_FM_CPI_SYSTEM_PROGRAM_ACCT_INFO_KEY_UOFF, -272
# System Program account info lamports.
.equ RM_FM_CPI_SYSTEM_PROGRAM_ACCT_INFO_LAMPORTS_UOFF, -264
# System Program account info data length.
.equ RM_FM_CPI_SYSTEM_PROGRAM_ACCT_INFO_DATA_LEN_UOFF, -256
# System Program account info data.
.equ RM_FM_CPI_SYSTEM_PROGRAM_ACCT_INFO_DATA_UOFF, -248
# System Program account info owner.
.equ RM_FM_CPI_SYSTEM_PROGRAM_ACCT_INFO_OWNER_UOFF, -240
# System Program account info rent epoch.
.equ RM_FM_CPI_SYSTEM_PROGRAM_ACCT_INFO_RENT_EPOCH_UOFF, -232
# System Program account info is signer.
.equ RM_FM_CPI_SYSTEM_PROGRAM_ACCT_INFO_IS_SIGNER_UOFF, -224
# System Program account info is writable.
.equ RM_FM_CPI_SYSTEM_PROGRAM_ACCT_INFO_IS_WRITABLE_UOFF, -223
# System Program account info executable.
.equ RM_FM_CPI_SYSTEM_PROGRAM_ACCT_INFO_EXECUTABLE_UOFF, -222
# System Program account meta pubkey.
.equ RM_FM_CPI_SYSTEM_PROGRAM_ACCT_META_PUBKEY_UOFF, -96
# System Program account meta is writable.
.equ RM_FM_CPI_SYSTEM_PROGRAM_ACCT_META_IS_WRITABLE_UOFF, -88
# System Program account meta is signer.
.equ RM_FM_CPI_SYSTEM_PROGRAM_ACCT_META_IS_SIGNER_UOFF, -87
# Token Program account info key.
.equ RM_FM_CPI_TOKEN_PROGRAM_ACCT_INFO_KEY_UOFF, -216
# Token Program account info lamports.
.equ RM_FM_CPI_TOKEN_PROGRAM_ACCT_INFO_LAMPORTS_UOFF, -208
# Token Program account info data length.
.equ RM_FM_CPI_TOKEN_PROGRAM_ACCT_INFO_DATA_LEN_UOFF, -200
# Token Program account info data.
.equ RM_FM_CPI_TOKEN_PROGRAM_ACCT_INFO_DATA_UOFF, -192
# Token Program account info owner.
.equ RM_FM_CPI_TOKEN_PROGRAM_ACCT_INFO_OWNER_UOFF, -184
# Token Program account info rent epoch.
.equ RM_FM_CPI_TOKEN_PROGRAM_ACCT_INFO_RENT_EPOCH_UOFF, -176
# Token Program account info is signer.
.equ RM_FM_CPI_TOKEN_PROGRAM_ACCT_INFO_IS_SIGNER_UOFF, -168
# Token Program account info is writable.
.equ RM_FM_CPI_TOKEN_PROGRAM_ACCT_INFO_IS_WRITABLE_UOFF, -167
# Token Program account info executable.
.equ RM_FM_CPI_TOKEN_PROGRAM_ACCT_INFO_EXECUTABLE_UOFF, -166
# Token Program account meta pubkey.
.equ RM_FM_CPI_TOKEN_PROGRAM_ACCT_META_PUBKEY_UOFF, -80
# Token Program account meta is writable.
.equ RM_FM_CPI_TOKEN_PROGRAM_ACCT_META_IS_WRITABLE_UOFF, -72
# Token Program account meta is signer.
.equ RM_FM_CPI_TOKEN_PROGRAM_ACCT_META_IS_SIGNER_UOFF, -71
.equ RM_FM_SIGNERS_SEEDS_ADDR_UOFF, -64 # Signers seeds address.
.equ RM_FM_SIGNERS_SEEDS_LEN_UOFF, -56 # Signers seeds length.
.equ RM_FM_SOL_INSN_OFF, -48 # SolInstruction offset.
.equ RM_FM_SOL_INSN_PROGRAM_ID_UOFF, -48 # SolInstruction program ID.
.equ RM_FM_SOL_INSN_ACCOUNTS_UOFF, -40 # SolInstruction accounts pointer.
.equ RM_FM_SOL_INSN_ACCOUNT_LEN_UOFF, -32 # SolInstruction account length.
.equ RM_FM_SOL_INSN_DATA_UOFF, -24 # SolInstruction data pointer.
.equ RM_FM_SOL_INSN_DATA_LEN_UOFF, -16 # SolInstruction data length.
.equ RM_FM_BUMP_OFF, -8 # Bump seed.
# From pda_seeds to sol_instruction.
.equ RM_FM_PDA_SEEDS_TO_SOL_INSN_REL_OFF_IMM, 616
# From pda to signers_seeds.
.equ RM_FM_PDA_TO_SIGNERS_SEEDS_REL_OFF_IMM, 552
# From create_account_data to cpi account metas.
.equ RM_FM_CREATE_ACCT_DATA_TO_CPI_ACCT_METAS_REL_OFF_IMM, 392
# -------------------------------------------------------------------------

# Miscellaneous market registration constants.
# -------------------------------------------------------------------------
# From input buffer to base mint duplicate flag.
.equ RM_MISC_BASE_DUPLICATE_OFF, 20680
# From input buffer to base mint data length.
.equ RM_MISC_BASE_DATA_LEN_OFF, 20760
.equ RM_MISC_BASE_ADDR_OFF, 20688 # From input buffer to base mint address.
.equ RM_MISC_QUOTE_OFF, 31016 # From input buffer to quote mint.
# From input buffer to quote mint duplicate flag.
.equ RM_MISC_QUOTE_DUPLICATE_OFF, 31016
# From input buffer to quote mint address.
.equ RM_MISC_QUOTE_ADDR_OFF, 31024
# From input buffer to quote mint data length.
.equ RM_MISC_QUOTE_DATA_LEN_OFF, 31096
# Number of seeds for market PDA derivation (base, quote).
.equ RM_MISC_TRY_FIND_PDA_SEEDS_LEN, 2
# Number of accounts for CreateAccount CPI (user, target).
.equ RM_MISC_CREATE_ACCOUNT_N_ACCOUNTS, 2
.equ RM_MISC_N_PDA_SIGNERS, 1 # Number of PDA signers for CPI.
# -------------------------------------------------------------------------

register_market:
    # if input.n_accounts < RegisterMarketAccounts.LEN
    #     return ErrorCode::InvalidNumberOfAccounts
    jlt r3, REGISTER_MARKET_ACCOUNTS_LEN, e_invalid_number_of_accounts
    # if insn_len != RegisterMarketData.LEN
    #     return ErrorCode::InvalidInstructionLength
    jne r4, REGISTER_MARKET_DATA_LEN, e_invalid_instruction_length
    # if input.user.data_len != data.DATA_LEN_ZERO
    #     return ErrorCode::UserHasData
    ldxdw r9, [r1 + IB_USER_DATA_LEN_OFF]
    jne r9, DATA_LEN_ZERO, e_user_has_data
    # if input.market.duplicate != account.NON_DUP_MARKER
    #     return ErrorCode::MarketAccountIsDuplicate
    ldxb r9, [r1 + IB_MARKET_DUPLICATE_OFF]
    jne r9, ACCT_NON_DUP_MARKER, e_market_account_is_duplicate
    # if input.market.data_len != DATA_LEN_ZERO
    #     return ErrorCode::MarketHasData
    ldxdw r9, [r1 + IB_MARKET_DATA_LEN_OFF]
    jne r9, DATA_LEN_ZERO, e_market_has_data
    # if input.base_mint.duplicate != account.NON_DUP_MARKER
    #     return ErrorCode::BaseMintIsDuplicate
    ldxb r9, [r1 + RM_MISC_BASE_DUPLICATE_OFF]
    jne r9, ACCT_NON_DUP_MARKER, e_base_mint_is_duplicate
    # frame.pda_seeds.base.addr = input.base_mint.pubkey
    mov64 r9, r1
    add64 r9, RM_MISC_BASE_ADDR_OFF
    stxdw [r10 + RM_FM_PDA_SEEDS_BASE_ADDR_OFF], r9
    # frame.pda_seeds.base.len = Address.size
    mov64 r9, SIZE_OF_ADDRESS
    stxdw [r10 + RM_FM_PDA_SEEDS_BASE_LEN_OFF], r9
    # input_shifted = input + input.base_mint.padded_data_len
    ldxdw r9, [r1 + RM_MISC_BASE_DATA_LEN_OFF]
    add64 r9, DATA_LEN_MAX_PAD
    and64 r9, DATA_LEN_AND_MASK
    add64 r9, r1
    # if input_shifted.quote_mint.duplicate != account.NON_DUP_MARKER
    #     return ErrorCode::QuoteMintIsDuplicate
    ldxb r8, [r9 + RM_MISC_QUOTE_DUPLICATE_OFF]
    jne r8, ACCT_NON_DUP_MARKER, e_quote_mint_is_duplicate
    # frame.pda_seeds.quote.addr = input_shifted.quote_mint.pubkey
    mov64 r8, r9
    add64 r8, RM_MISC_QUOTE_ADDR_OFF
    stxdw [r10 + RM_FM_PDA_SEEDS_QUOTE_ADDR_OFF], r8
    # quote_mint_padded_data_len = input_shifted.quote_mint.padded_data_len
    ldxdw r8, [r9 + RM_MISC_QUOTE_DATA_LEN_OFF]
    add64 r8, DATA_LEN_MAX_PAD
    and64 r8, DATA_LEN_AND_MASK
    # acct = &input_shifted.quote_mint
    add64 r9, RM_MISC_QUOTE_OFF
    # acct += quote_mint_padded_data_len + EmptyAccount.size
    add64 r9, r8
    add64 r9, SIZE_OF_EMPTY_ACCOUNT
    # frame.pda_seeds.quote.len = Address.size
    mov64 r8, SIZE_OF_ADDRESS
    stxdw [r10 + RM_FM_PDA_SEEDS_QUOTE_LEN_OFF], r8
    # Store(input)
    mov64 r6, r1
    # syscall.seeds = &frame.pda_seeds
    mov64 r1, r10
    add64 r1, RM_FM_PDA_SEEDS_OFF
    # syscall.program_id = &insn.program_id
    mov64 r3, r2
    add64 r3, REGISTER_MARKET_DATA_LEN
    # syscall.seeds_len = register_misc.TRY_FIND_PDA_SEEDS_LEN
    mov64 r2, RM_MISC_TRY_FIND_PDA_SEEDS_LEN
    # syscall.program_address = &frame.pda
    mov64 r4, r10
    add64 r4, RM_FM_PDA_OFF
    # syscall.bump_seed = &frame.bump
    mov64 r5, r10
    add64 r5, RM_FM_BUMP_OFF
    call sol_try_find_program_address
    # if input.market.pubkey != frame.market_pda
    #     return ErrorCode::InvalidMarketPubkey
    ldxdw r7, [r6 + IB_MARKET_PUBKEY_CHUNK_0_OFF]
    ldxdw r8, [r10 + RM_FM_PDA_CHUNK_0_OFF]
    jne r7, r8, e_invalid_market_pubkey
    ldxdw r7, [r6 + IB_MARKET_PUBKEY_CHUNK_1_OFF]
    ldxdw r8, [r10 + RM_FM_PDA_CHUNK_1_OFF]
    jne r7, r8, e_invalid_market_pubkey
    ldxdw r7, [r6 + IB_MARKET_PUBKEY_CHUNK_2_OFF]
    ldxdw r8, [r10 + RM_FM_PDA_CHUNK_2_OFF]
    jne r7, r8, e_invalid_market_pubkey
    ldxdw r7, [r6 + IB_MARKET_PUBKEY_CHUNK_3_OFF]
    ldxdw r8, [r10 + RM_FM_PDA_CHUNK_3_OFF]
    jne r7, r8, e_invalid_market_pubkey
    # frame.pda_seeds.bump.addr = &frame.bump
    stxdw [r10 + RM_FM_PDA_SEEDS_BUMP_ADDR_OFF], r5
    # frame.pda_seeds.bump.len = u8.size
    mov64 r7, SIZE_OF_U8
    stxdw [r10 + RM_FM_PDA_SEEDS_BUMP_LEN_OFF], r7
    # frame.create_account_data.owner = syscall.program_id
    ldxdw r7, [r3 + PUBKEY_CHUNK_0_OFF]
    stxdw [r10 + RM_FM_CREATE_ACCT_OWNER_CHUNK_0_UOFF], r7
    ldxdw r7, [r3 + PUBKEY_CHUNK_1_OFF]
    stxdw [r10 + RM_FM_CREATE_ACCT_OWNER_CHUNK_1_UOFF], r7
    ldxdw r7, [r3 + PUBKEY_CHUNK_2_OFF]
    stxdw [r10 + RM_FM_CREATE_ACCT_OWNER_CHUNK_2_UOFF], r7
    ldxdw r7, [r3 + PUBKEY_CHUNK_3_OFF]
    stxdw [r10 + RM_FM_CREATE_ACCT_OWNER_CHUNK_3_UOFF], r7
    # if acct.duplicate != account.NON_DUP_MARKER
    #     return ErrorCode::SystemProgramIsDuplicate
    ldxb r7, [r9 + ACCT_DUPLICATE_OFF]
    jne r7, ACCT_NON_DUP_MARKER, e_system_program_is_duplicate
    # if acct.pubkey != frame.system_program_pubkey
    #     return ErrorCode::InvalidSystemProgramPubkey
    ldxdw r7, [r9 + ACCT_ADDRESS_CHUNK_0_OFF]
    ldxdw r8, [r10 + RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_0_OFF]
    jne r7, r8, e_invalid_system_program_pubkey
    ldxdw r7, [r9 + ACCT_ADDRESS_CHUNK_1_OFF]
    ldxdw r8, [r10 + RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_1_OFF]
    jne r7, r8, e_invalid_system_program_pubkey
    ldxdw r7, [r9 + ACCT_ADDRESS_CHUNK_2_OFF]
    ldxdw r8, [r10 + RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_2_OFF]
    jne r7, r8, e_invalid_system_program_pubkey
    ldxdw r7, [r9 + ACCT_ADDRESS_CHUNK_3_OFF]
    ldxdw r8, [r10 + RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_3_OFF]
    jne r7, r8, e_invalid_system_program_pubkey
    # frame.sol_instruction.program_id = &acct.address
    mov64 r7, r9
    add64 r7, ACCT_ADDRESS_OFF
    stxdw [r10 + RM_FM_SOL_INSN_PROGRAM_ID_UOFF], r7
    # system_program_padded_data_len = acct.padded_data_len
    ldxdw r7, [r9 + ACCT_DATA_LEN_OFF]
    add64 r7, DATA_LEN_MAX_PAD
    and64 r7, DATA_LEN_AND_MASK
    # acct += system_program_padded_data_len + EmptyAccount.size
    add64 r9, r7
    add64 r9, SIZE_OF_EMPTY_ACCOUNT
    # if acct.duplicate != account.NON_DUP_MARKER
    #     return ErrorCode::RentSysvarIsDuplicate
    ldxb r7, [r9 + ACCT_DUPLICATE_OFF]
    jne r7, ACCT_NON_DUP_MARKER, e_rent_sysvar_is_duplicate
    # if acct.pubkey != pubkey.RENT
    #     return ErrorCode::InvalidRentSysvarPubkey
    ldxdw r7, [r9 + ACCT_ADDRESS_CHUNK_0_OFF]
    lddw r8, PUBKEY_RENT_CHUNK_0
    jne r7, r8, e_invalid_rent_sysvar_pubkey
    ldxdw r7, [r9 + ACCT_ADDRESS_CHUNK_1_OFF]
    lddw r8, PUBKEY_RENT_CHUNK_1
    jne r7, r8, e_invalid_rent_sysvar_pubkey
    ldxdw r7, [r9 + ACCT_ADDRESS_CHUNK_2_OFF]
    lddw r8, PUBKEY_RENT_CHUNK_2
    jne r7, r8, e_invalid_rent_sysvar_pubkey
    ldxdw r7, [r9 + ACCT_ADDRESS_CHUNK_3_OFF]
    # Optimize: pubkey.RENT chunk 3 hi bits are zero, so mov32
    # (1 CU) replaces lddw (2 CUs).
    mov32 r8, PUBKEY_RENT_CHUNK_3_LO
    jne r7, r8, e_invalid_rent_sysvar_pubkey
    # frame.create_account_data.space = MarketHeader.size
    mov64 r7, SIZE_OF_MARKET_HEADER
    stxdw [r10 + RM_FM_CREATE_ACCT_SPACE_UOFF], r7
    # acct_size = MarketHeader.size + account.STORAGE_OVERHEAD
    add64 r7, ACCT_STORAGE_OVERHEAD
    # lamports_per_byte = acct.data.lamports_per_byte
    ldxdw r8, [r9 + ACCT_DATA_OFF]
    # frame.create_account_data.lamports = acct_size * lamports_per_byte
    mul64 r7, r8
    stxdw [r10 + RM_FM_CREATE_ACCT_LAMPORTS_UOFF], r7
    # frame.cpi.user_info.is_signer = true
    # frame.cpi.user_info.is_writable = true
    sth [r10 + RM_FM_CPI_USER_ACCT_INFO_IS_SIGNER_UOFF], CPI_WRITABLE_SIGNER
    # frame.cpi.user_meta.is_writable = true
    # frame.cpi.user_meta.is_signer = true
    sth [r10 + RM_FM_CPI_USER_ACCT_META_IS_WRITABLE_UOFF], CPI_WRITABLE_SIGNER
    # frame.cpi.target_info.is_signer = true
    # frame.cpi.target_info.is_writable = true
    sth [r10 + RM_FM_CPI_TARGET_ACCT_INFO_IS_SIGNER_UOFF], CPI_WRITABLE_SIGNER
    # frame.cpi.target_meta.is_writable = true
    # frame.cpi.target_meta.is_signer = true
    sth [r10 + RM_FM_CPI_TARGET_ACCT_META_IS_WRITABLE_UOFF], CPI_WRITABLE_SIGNER
    # frame.cpi.user_meta.pubkey = &input.user.address
    # frame.cpi.user_info.key = &input.user.address
    add64 r6, IB_USER_PUBKEY_OFF
    stxdw [r10 + RM_FM_CPI_USER_ACCT_META_PUBKEY_UOFF], r6
    stxdw [r10 + RM_FM_CPI_USER_ACCT_INFO_KEY_UOFF], r6
    # frame.cpi.user_info.owner = &input.user.owner
    add64 r6, IB_ADDRESS_TO_OWNER_REL_OFF_IMM
    stxdw [r10 + RM_FM_CPI_USER_ACCT_INFO_OWNER_UOFF], r6
    # frame.cpi.user_info.lamports = &input.user.lamports
    add64 r6, IB_OWNER_TO_LAMPORTS_REL_OFF_IMM
    stxdw [r10 + RM_FM_CPI_USER_ACCT_INFO_LAMPORTS_UOFF], r6
    # frame.cpi.user_info.data = &input.user.data
    add64 r6, IB_LAMPORTS_TO_DATA_REL_OFF_IMM
    stxdw [r10 + RM_FM_CPI_USER_ACCT_INFO_DATA_UOFF], r6
    # frame.cpi.target_meta.pubkey = &input.market.address
    # frame.cpi.target_info.key = &input.market.address
    add64 r6, IB_USER_DATA_TO_MARKET_ADDRESS_REL_OFF_IMM
    stxdw [r10 + RM_FM_CPI_TARGET_ACCT_META_PUBKEY_UOFF], r6
    stxdw [r10 + RM_FM_CPI_TARGET_ACCT_INFO_KEY_UOFF], r6
    # frame.cpi.target_info.owner = &input.market.owner
    add64 r6, IB_ADDRESS_TO_OWNER_REL_OFF_IMM
    stxdw [r10 + RM_FM_CPI_TARGET_ACCT_INFO_OWNER_UOFF], r6
    # frame.cpi.target_info.lamports = &input.market.lamports
    add64 r6, IB_OWNER_TO_LAMPORTS_REL_OFF_IMM
    stxdw [r10 + RM_FM_CPI_TARGET_ACCT_INFO_LAMPORTS_UOFF], r6
    # frame.cpi.target_info.data = &input.market.data
    add64 r6, IB_LAMPORTS_TO_DATA_REL_OFF_IMM
    stxdw [r10 + RM_FM_CPI_TARGET_ACCT_INFO_DATA_UOFF], r6
    # frame.signers_seeds.addr = &frame.pda_seeds
    stxdw [r10 + RM_FM_SIGNERS_SEEDS_ADDR_UOFF], r1
    # frame.signers_seeds.len = frame.PDA_SEEDS_N_SEEDS
    mov64 r7, RM_FM_PDA_SEEDS_N_SEEDS
    stxdw [r10 + RM_FM_SIGNERS_SEEDS_LEN_UOFF], r7
    # frame.sol_instruction.data = &frame.create_account_data
    mov64 r7, r10
    add64 r7, RM_FM_CREATE_ACCT_DATA_OFF
    stxdw [r10 + RM_FM_SOL_INSN_DATA_UOFF], r7
    # frame.sol_instruction.accounts = &frame.cpi.account_metas
    add64 r7, RM_FM_CREATE_ACCT_DATA_TO_CPI_ACCT_METAS_REL_OFF_IMM
    stxdw [r10 + RM_FM_SOL_INSN_ACCOUNTS_UOFF], r7
    # frame.sol_instruction.account_len = register_misc.CREATE_ACCOUNT_N_ACCOUNTS
    mov64 r7, RM_MISC_CREATE_ACCOUNT_N_ACCOUNTS
    stxdw [r10 + RM_FM_SOL_INSN_ACCOUNT_LEN_UOFF], r7
    # frame.sol_instruction.data_len = CreateAccountData.size
    mov64 r7, SIZE_OF_CREATE_ACCOUNT_DATA
    stxdw [r10 + RM_FM_SOL_INSN_DATA_LEN_UOFF], r7
    # syscall.instruction = &frame.sol_instruction (r1 from pda_seeds)
    add64 r1, RM_FM_PDA_SEEDS_TO_SOL_INSN_REL_OFF_IMM
    # syscall.account_infos = &frame.cpi.account_infos
    mov64 r2, r10
    add64 r2, RM_FM_CPI_SOL_ACCT_INFO_OFF
    # syscall.account_infos_len = register_misc.CREATE_ACCOUNT_N_ACCOUNTS
    mov64 r3, RM_MISC_CREATE_ACCOUNT_N_ACCOUNTS
    # syscall.seeds = &frame.signers_seeds (r4 from pda)
    add64 r4, RM_FM_PDA_TO_SIGNERS_SEEDS_REL_OFF_IMM
    # syscall.seeds_len = register_misc.N_PDA_SIGNERS
    mov64 r5, RM_MISC_N_PDA_SIGNERS
    call sol_invoke_signed_c
    exit
# --- end market/register.s ---
