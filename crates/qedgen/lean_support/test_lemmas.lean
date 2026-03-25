import QEDGen.Solana.Account
import QEDGen.Solana.Token
import QEDGen.Solana.State
import QEDGen.Solana.Cpi
import QEDGen.Solana.Authority

open QEDGen.Solana

-- Test 1: trackedTotal_nil works
example : trackedTotal [] = 0 := by
  exact trackedTotal_nil

-- Test 2: closes_is_closed works
example (before after : Lifecycle) (h : closes before after) :
    after = Lifecycle.closed := by
  exact closes_is_closed before after h

-- Test 3: closes_was_open works
example (before after : Lifecycle) (h : closes before after) :
    before = Lifecycle.open := by
  exact closes_was_open before after h

-- Test 4: findByAuthority is find? by authority
example (accs : List Account) (auth : Pubkey) :
    findByAuthority accs auth = accs.find? (fun acc => acc.authority = auth) := by
  rfl

-- Test 5: authorized_refl works
example (auth : Pubkey) : QEDGen.Solana.Authority.Authorized auth auth := by
  exact QEDGen.Solana.Authority.authorized_refl auth

-- Test 6: CPI construction is pure (rfl)
example : (QEDGen.Solana.Cpi.TransferCpi.mk 0 1 2 3 100).program = TOKEN_PROGRAM_ID := by
  rfl

-- Test 7: closed_cannot_close works
example (p_after : Lifecycle) : ¬ closes Lifecycle.closed p_after := by
  exact closed_cannot_close p_after
