import QEDGen.Solana.Account
import QEDGen.Solana.State
import QEDGen.Solana.Cpi
import QEDGen.Solana.Authority

open QEDGen.Solana

-- Test 1: closes_is_closed works
example (before after : Lifecycle) (h : closes before after) :
    after = Lifecycle.closed := by
  exact closes_is_closed before after h

-- Test 2: closes_was_open works
example (before after : Lifecycle) (h : closes before after) :
    before = Lifecycle.open := by
  exact closes_was_open before after h

-- Test 3: findByAuthority is find? by authority
example (accs : List Account) (auth : Pubkey) :
    findByAuthority accs auth = accs.find? (fun acc => acc.authority = auth) := by
  rfl

-- Test 4: authorized_refl works
example (auth : Pubkey) : QEDGen.Solana.Authority.Authorized auth auth := by
  exact QEDGen.Solana.Authority.authorized_refl auth

-- Test 5: targetsProgram with real TOKEN_PROGRAM_ID
example : targetsProgram
    { programId := TOKEN_PROGRAM_ID, accounts := [⟨⟨1, 0, 0, 0⟩, true, false⟩], data := [DISC_TRANSFER] }
    TOKEN_PROGRAM_ID := by
  unfold targetsProgram
  rfl

-- Test 6: accountAt on concrete list
def pk42 : Pubkey := ⟨42, 0, 0, 0⟩
def pk99 : Pubkey := ⟨99, 0, 0, 0⟩

example : accountAt
    { programId := TOKEN_PROGRAM_ID, accounts := [⟨pk42, false, true⟩, ⟨pk99, true, false⟩], data := [DISC_TRANSFER] }
    1 pk99 true false := by
  unfold accountAt
  rfl

-- Test 7: hasDiscriminator with SPL Token transfer discriminator
example : hasDiscriminator
    { programId := TOKEN_PROGRAM_ID, accounts := [], data := [DISC_TRANSFER, 0, 0, 0, 100] }
    [DISC_TRANSFER] := by
  unfold hasDiscriminator
  rfl

-- Test 8: hasDiscriminator with System Program 4-byte discriminator
example : hasDiscriminator
    { programId := SYSTEM_PROGRAM_ID, accounts := [], data := [2, 0, 0, 0, 100] }
    DISC_SYS_TRANSFER := by
  unfold hasDiscriminator
  rfl

-- Test 9: hasNAccounts
example : hasNAccounts
    { programId := TOKEN_PROGRAM_ID, accounts := [⟨⟨1, 0, 0, 0⟩, false, true⟩, ⟨⟨2, 0, 0, 0⟩, false, true⟩, ⟨⟨3, 0, 0, 0⟩, true, false⟩], data := [DISC_TRANSFER] }
    3 := by
  unfold hasNAccounts
  rfl

-- Test 10: program IDs are distinct
example : TOKEN_PROGRAM_ID ≠ SYSTEM_PROGRAM_ID := by decide
example : TOKEN_PROGRAM_ID ≠ TOKEN_2022_PROGRAM_ID := by decide
example : TOKEN_PROGRAM_ID ≠ ASSOCIATED_TOKEN_PROGRAM_ID := by decide

-- Test 11: closed_cannot_close works
example (p_after : Lifecycle) : ¬ closes Lifecycle.closed p_after := by
  exact closed_cannot_close p_after
