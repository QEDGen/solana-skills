import QEDGen.Solana.Account

namespace QEDGen.Solana.Cpi

open QEDGen.Solana.Account

/- ============================================================================
   CPI (Cross-Program Invocation) Modeling — Generic Envelope

   Models CPI at the invoke_signed level: any Solana CPI is a program ID,
   a list of account metas, and serialized instruction data. This is generic
   across ALL Solana programs — no per-instruction structs needed.

   Anchor programs CPI via CpiContext + anchor-spl wrappers, which under the
   hood call solana_program::program::invoke_signed with:
     - An Instruction { program_id, accounts: Vec<AccountMeta>, data: Vec<u8> }
     - Account infos
     - Signer seeds (for PDA signing)

   Verification scope:
   - Correct program is called (program_id)
   - Accounts are in correct order with correct pubkeys and flags
   - Instruction discriminator is correct (first N bytes of data)

   Trust boundary (not verified):
   - Parameter serialization within data bytes (SDK territory)
   - External program execution semantics
   - PDA signer seed derivation and validity
   ============================================================================ -/

/-- Solana account metadata passed to a CPI instruction -/
structure AccountMeta where
  pubkey : Pubkey
  isSigner : Bool
  isWritable : Bool
  deriving Repr, DecidableEq, BEq

/-- A CPI instruction envelope — mirrors Solana's invoke_signed arguments -/
structure CpiInstruction where
  programId : Pubkey
  accounts : List AccountMeta
  data : List Nat
  deriving Repr, DecidableEq

/- ============================================================================
   Well-known program IDs

   Pubkeys decoded from base58 to big-endian Nat.
   Source: https://github.com/solana-program
   ============================================================================ -/

/-- System Program (11111111111111111111111111111111) -/
def SYSTEM_PROGRAM_ID : Pubkey :=
  0x0000000000000000000000000000000000000000000000000000000000000000

/-- SPL Token Program (TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA) -/
def TOKEN_PROGRAM_ID : Pubkey :=
  0x06ddf6e1d765a193d9cbe146ceeb79ac1cb485ed5f5b37913a8cf5857eff00a9

/-- Token-2022 / Token Extensions (TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb) -/
def TOKEN_2022_PROGRAM_ID : Pubkey :=
  0x06ddf6e1ee758fde18425dbce46ccddab61afc4d83b90d27febdf928d8a18bfc

/-- Associated Token Account Program (ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL) -/
def ASSOCIATED_TOKEN_PROGRAM_ID : Pubkey :=
  0x8c97258f4e2489f1bb3d1029148e0d830b5a1399daff1084048e7bd8dbe9f859

/-- Memo Program v2 (MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr) -/
def MEMO_PROGRAM_ID : Pubkey :=
  0x054a535a992921064d24e87160da387c7c35b5ddbc92bb81e41fa8404105448d

/-- Compute Budget Program (ComputeBudget111111111111111111111111111111) -/
def COMPUTE_BUDGET_PROGRAM_ID : Pubkey :=
  0x0306466fe5211732ffecadba72c39be7bc8ce5bbc5f7126b2c439b3a40000000

/-- Stake Program (Stake11111111111111111111111111111111111111) -/
def STAKE_PROGRAM_ID : Pubkey :=
  0x06a1d8179137542a983437bdfe2a7ab2557f535c8a78722b68a49dc000000000

/- ============================================================================
   SPL Token instruction discriminators (single u8 byte)

   Token and Token-2022 share the same discriminators for instructions 0-24.
   These are the instructions most commonly invoked via anchor-spl CPI wrappers.

   Anchor pattern:
     token::transfer(CpiContext::new(..., Transfer { from, to, authority }), amount)?;
   maps to invoke_signed with data = [DISC_TRANSFER] ++ le_u64(amount)
   ============================================================================ -/

def DISC_INITIALIZE_MINT     : Nat := 0
def DISC_INITIALIZE_ACCOUNT  : Nat := 1
def DISC_TRANSFER            : Nat := 3
def DISC_APPROVE             : Nat := 4
def DISC_REVOKE              : Nat := 5
def DISC_SET_AUTHORITY       : Nat := 6
def DISC_MINT_TO             : Nat := 7
def DISC_BURN                : Nat := 8
def DISC_CLOSE_ACCOUNT       : Nat := 9
def DISC_FREEZE_ACCOUNT      : Nat := 10
def DISC_THAW_ACCOUNT        : Nat := 11
def DISC_TRANSFER_CHECKED    : Nat := 12
def DISC_APPROVE_CHECKED     : Nat := 13
def DISC_MINT_TO_CHECKED     : Nat := 14
def DISC_BURN_CHECKED        : Nat := 15
def DISC_SYNC_NATIVE         : Nat := 17
def DISC_INITIALIZE_ACCOUNT3 : Nat := 18
def DISC_INITIALIZE_MINT2    : Nat := 20

/- ============================================================================
   System Program instruction discriminators (4-byte LE u32)

   Anchor pattern:
     system_program::transfer(CpiContext::new(..., Transfer { from, to }), amount)?;
   maps to invoke_signed with data = [2,0,0,0] ++ le_u64(amount)
   ============================================================================ -/

def DISC_SYS_CREATE_ACCOUNT : List Nat := [0, 0, 0, 0]
def DISC_SYS_ASSIGN         : List Nat := [1, 0, 0, 0]
def DISC_SYS_TRANSFER       : List Nat := [2, 0, 0, 0]
def DISC_SYS_ALLOCATE       : List Nat := [8, 0, 0, 0]

/- ============================================================================
   Associated Token Account instruction discriminators (single u8 byte)
   ============================================================================ -/

def DISC_ATA_CREATE            : Nat := 0
def DISC_ATA_CREATE_IDEMPOTENT : Nat := 1
def DISC_ATA_RECOVER_NESTED    : Nat := 2

/- ============================================================================
   Verification predicates — all rfl-provable on concrete CpiInstruction values
   ============================================================================ -/

/-- The instruction targets the expected program -/
def targetsProgram (cpi : CpiInstruction) (expected : Pubkey) : Prop :=
  cpi.programId = expected

/-- The account at index i has the expected pubkey, signer flag, and writable flag -/
def accountAt (cpi : CpiInstruction) (i : Nat)
    (key : Pubkey) (signer writable : Bool) : Prop :=
  cpi.accounts.get? i = some ⟨key, signer, writable⟩

/-- The instruction data starts with the expected discriminator bytes -/
def hasDiscriminator (cpi : CpiInstruction) (disc : List Nat) : Prop :=
  cpi.data.take disc.length = disc

/-- The instruction passes exactly n accounts -/
def hasNAccounts (cpi : CpiInstruction) (n : Nat) : Prop :=
  cpi.accounts.length = n

/-- Basic well-formedness: non-empty accounts and data -/
def wellFormed (cpi : CpiInstruction) : Prop :=
  cpi.accounts.length > 0 ∧ cpi.data.length > 0

end QEDGen.Solana.Cpi

namespace QEDGen.Solana

-- Types
abbrev AccountMeta := QEDGen.Solana.Cpi.AccountMeta
abbrev CpiInstruction := QEDGen.Solana.Cpi.CpiInstruction

-- Program IDs
abbrev SYSTEM_PROGRAM_ID := QEDGen.Solana.Cpi.SYSTEM_PROGRAM_ID
abbrev TOKEN_PROGRAM_ID := QEDGen.Solana.Cpi.TOKEN_PROGRAM_ID
abbrev TOKEN_2022_PROGRAM_ID := QEDGen.Solana.Cpi.TOKEN_2022_PROGRAM_ID
abbrev ASSOCIATED_TOKEN_PROGRAM_ID := QEDGen.Solana.Cpi.ASSOCIATED_TOKEN_PROGRAM_ID
abbrev MEMO_PROGRAM_ID := QEDGen.Solana.Cpi.MEMO_PROGRAM_ID
abbrev COMPUTE_BUDGET_PROGRAM_ID := QEDGen.Solana.Cpi.COMPUTE_BUDGET_PROGRAM_ID
abbrev STAKE_PROGRAM_ID := QEDGen.Solana.Cpi.STAKE_PROGRAM_ID

-- SPL Token discriminators
abbrev DISC_TRANSFER := QEDGen.Solana.Cpi.DISC_TRANSFER
abbrev DISC_TRANSFER_CHECKED := QEDGen.Solana.Cpi.DISC_TRANSFER_CHECKED
abbrev DISC_MINT_TO := QEDGen.Solana.Cpi.DISC_MINT_TO
abbrev DISC_MINT_TO_CHECKED := QEDGen.Solana.Cpi.DISC_MINT_TO_CHECKED
abbrev DISC_BURN := QEDGen.Solana.Cpi.DISC_BURN
abbrev DISC_BURN_CHECKED := QEDGen.Solana.Cpi.DISC_BURN_CHECKED
abbrev DISC_CLOSE_ACCOUNT := QEDGen.Solana.Cpi.DISC_CLOSE_ACCOUNT
abbrev DISC_APPROVE := QEDGen.Solana.Cpi.DISC_APPROVE
abbrev DISC_APPROVE_CHECKED := QEDGen.Solana.Cpi.DISC_APPROVE_CHECKED
abbrev DISC_REVOKE := QEDGen.Solana.Cpi.DISC_REVOKE
abbrev DISC_SET_AUTHORITY := QEDGen.Solana.Cpi.DISC_SET_AUTHORITY
abbrev DISC_FREEZE_ACCOUNT := QEDGen.Solana.Cpi.DISC_FREEZE_ACCOUNT
abbrev DISC_THAW_ACCOUNT := QEDGen.Solana.Cpi.DISC_THAW_ACCOUNT
abbrev DISC_INITIALIZE_MINT := QEDGen.Solana.Cpi.DISC_INITIALIZE_MINT
abbrev DISC_INITIALIZE_MINT2 := QEDGen.Solana.Cpi.DISC_INITIALIZE_MINT2
abbrev DISC_INITIALIZE_ACCOUNT := QEDGen.Solana.Cpi.DISC_INITIALIZE_ACCOUNT
abbrev DISC_INITIALIZE_ACCOUNT3 := QEDGen.Solana.Cpi.DISC_INITIALIZE_ACCOUNT3
abbrev DISC_SYNC_NATIVE := QEDGen.Solana.Cpi.DISC_SYNC_NATIVE

-- System Program discriminators
abbrev DISC_SYS_CREATE_ACCOUNT := QEDGen.Solana.Cpi.DISC_SYS_CREATE_ACCOUNT
abbrev DISC_SYS_ASSIGN := QEDGen.Solana.Cpi.DISC_SYS_ASSIGN
abbrev DISC_SYS_TRANSFER := QEDGen.Solana.Cpi.DISC_SYS_TRANSFER
abbrev DISC_SYS_ALLOCATE := QEDGen.Solana.Cpi.DISC_SYS_ALLOCATE

-- ATA discriminators
abbrev DISC_ATA_CREATE := QEDGen.Solana.Cpi.DISC_ATA_CREATE
abbrev DISC_ATA_CREATE_IDEMPOTENT := QEDGen.Solana.Cpi.DISC_ATA_CREATE_IDEMPOTENT
abbrev DISC_ATA_RECOVER_NESTED := QEDGen.Solana.Cpi.DISC_ATA_RECOVER_NESTED

-- Predicates
abbrev targetsProgram := QEDGen.Solana.Cpi.targetsProgram
abbrev accountAt := QEDGen.Solana.Cpi.accountAt
abbrev hasDiscriminator := QEDGen.Solana.Cpi.hasDiscriminator
abbrev hasNAccounts := QEDGen.Solana.Cpi.hasNAccounts
abbrev cpiWellFormed := QEDGen.Solana.Cpi.wellFormed

end QEDGen.Solana
