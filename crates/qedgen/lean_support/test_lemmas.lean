import QEDGen.Solana.Account
import QEDGen.Solana.Token
import QEDGen.Solana.State

open QEDGen.Solana

-- Test 1: trackedTotal_cons works
example (acc : Account) (accs : List Account) : 
    trackedTotal (acc :: accs) = acc.balance + trackedTotal accs := by
  exact trackedTotal_cons acc accs

-- Test 2: transfer_preserves_total works
example (accs : List Account) (p_from p_to : Pubkey) (amt : Nat) (h : p_from ≠ p_to) :
    let updated := accs.map (fun acc =>
      if acc.authority = p_from then { acc with balance := acc.balance - amt }
      else if acc.authority = p_to then { acc with balance := acc.balance + amt }
      else acc)
    trackedTotal updated = trackedTotal accs := by
  exact transfer_preserves_total accs p_from p_to amt h

-- Test 3: closes_is_closed works  
example (before after : Lifecycle) (h : closes before after) :
    after = Lifecycle.closed := by
  exact closes_is_closed before after h

-- Test 4: findByAuthority works
example (accs : List Account) (auth : Pubkey) :
    findByAuthority accs auth = accs.find? (fun acc => acc.authority = auth) := by
  rfl
  
-- Test 5: trackedTotal_map_id works (this one has a real proof!)
example (accs : List Account) (f : Account → Account) (h : ∀ acc, (f acc).balance = acc.balance) :
    trackedTotal (accs.map f) = trackedTotal accs := by
  exact trackedTotal_map_id accs f h
