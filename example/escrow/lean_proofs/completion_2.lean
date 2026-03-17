import Mathlib
import Aesop
open BigOperators
open Real
open Nat
open Topology
open Rat
theorem EscrowProperties
    (u64 : Type)
    (TokenAccount : Type)
    (EscrowAccount : Type)
    (PubKey : Type)
    (TokenTransfer : Type)
    (u64_zero : u64)
    (u64_max : u64)
    (token_balance : TokenAccount → u64)
    (escrow_initializer : EscrowAccount → PubKey)
    (escrow_taker_amount : EscrowAccount → u64)
    (escrow_initializer_amount : EscrowAccount → u64)
    (escrow_bump : EscrowAccount → u64)
    (is_pda : EscrowAccount → Bool)
    (transfer : TokenTransfer → u64 → Bool)
    (transfer_from : TokenTransfer → TokenAccount)
    (transfer_to : TokenTransfer → TokenAccount)
    (transfer_amount : TokenTransfer → u64)
    (transfer_authority : TokenTransfer → PubKey)
    (exchange_successful : EscrowAccount → Bool)
    (cancel_successful : EscrowAccount → Bool)
    (escrow_exists : EscrowAccount → Bool)
    : Prop


theorem token_conservation
    (initializer_account : TokenAccount)
    (escrow_account : EscrowAccount)
    (taker_account : TokenAccount)
    (other_accounts : List TokenAccount)
    (transfers : List TokenTransfer)
    (h_initial : token_balance initializer_account = u64_zero)
    (h_transfers : ∀ t ∈ transfers, transfer t (transfer_amount t))
    : token_balance initializer_account + List.sum (List.map token_balance other_accounts) =
      List.sum (List.map (fun t => if transfer_from t = initializer_account then 0 else token_balance (transfer_from t)) transfers) +
      List.sum (List.map (fun t => if transfer_to t = initializer_account then token_balance (transfer_to t) else 0) transfers) := by


theorem access_control
    (escrow_account : EscrowAccount)
    (signer : PubKey)
    (h_cancel : cancel_successful escrow_account)
    : signer = escrow_initializer escrow_account := by


theorem exchange_correctness
    (escrow_account : EscrowAccount)
    (initializer_account : TokenAccount)
    (taker_account : TokenAccount)
    (escrow_token_account : TokenAccount)
    (h_exchange : exchange_successful escrow_account)
    : token_balance taker_account = escrow_initializer_amount escrow_account ∧
      token_balance initializer_account = escrow_taker_amount escrow_account ∧
      token_balance escrow_token_account = u64_zero := by


theorem state_machine_safety
    (escrow_account : EscrowAccount)
    (h_closed : ¬ escrow_exists escrow_account)
    : ∀ (op : Unit), ¬ escrow_exists escrow_account := by


theorem arithmetic_safety
    (amount : u64)
    (taker_amount : u64)
    (h_amount : amount ≤ u64_max)
    (h_taker : taker_amount ≤ u64_max)
    (h_transfers : ∀ t ∈ transfers, transfer_amount t ≤ u64_max)
    : amount + taker_amount ≤ u64_max := by


theorem token_conservation
    (initializer_account : TokenAccount)
    (escrow_account : EscrowAccount)
    (taker_account : TokenAccount)
    (other_accounts : List TokenAccount)
    (transfers : List TokenTransfer)
    (h_initial : token_balance initializer_account = u64_zero)
    (h_transfers : ∀ t ∈ transfers, transfer t (transfer_amount t))
    : token_balance initializer_account + List.sum (List.map token_balance other_accounts) =
      List.sum (List.map (fun t => if transfer_from t = initializer_account then 0 else token_balance (transfer_from t)) transfers) +
      List.sum (List.map (fun t => if transfer_to t = initializer_account then token_balance (transfer_to t) else 0) transfers) := by
  sorry


theorem access_control
    (escrow_account : EscrowAccount)
    (signer : PubKey)
    (h_cancel : cancel_successful escrow_account)
    : signer = escrow_initializer escrow_account := by
  sorry


theorem exchange_correctness
    (escrow_account : EscrowAccount)
    (initializer_account : TokenAccount)
    (taker_account : TokenAccount)
    (escrow_token_account : TokenAccount)
    (h_exchange : exchange_successful escrow_account)
    : token_balance taker_account = escrow_initializer_amount escrow_account ∧
      token_balance initializer_account = escrow_taker_amount escrow_account ∧
      token_balance escrow_token_account = u64_zero := by
  sorry


theorem state_machine_safety
    (escrow_account : EscrowAccount)
    (h_closed : ¬ escrow_exists escrow_account)
    : ∀ (op : Unit), ¬ escrow_exists escrow_account := by
  intro op
  exact h_closed


theorem arithmetic_safety
    (amount : u64)
    (taker_amount : u64)
    (h_amount : amount ≤ u64_max)
    (h_taker : taker_amount ≤ u64_max)
    (h_transfers : ∀ t ∈ transfers, transfer_amount t ≤ u64_max)
    : amount + taker_amount ≤ u64_max := by
  sorry
