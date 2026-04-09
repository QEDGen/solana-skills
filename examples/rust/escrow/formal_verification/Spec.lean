import QEDGen.Solana.Spec

open QEDGen.Solana.SpecDSL

/-!
# Escrow Verification Spec

A token escrow that lets two parties trade safely. The initializer deposits
tokens and sets terms, a taker completes the trade, or the initializer
cancels and reclaims their deposit.

Lifecycle: initialize → [Open] → cancel | exchange → [Closed]

Note: exchange makes TWO SPL Token transfers (taker→initializer and
escrow→taker). The `calls:` clause models the primary taker-side transfer.
The PDA-signed escrow→taker transfer is verified in the hand-written proofs.
-/

qedspec Escrow where
  state
    initializer : Pubkey
    taker : Pubkey
    initializer_amount : U64
    taker_amount : U64
    escrow_token_account : Pubkey

  operation initialize
    who: initializer
    when: Uninitialized
    then: Open
    calls: TOKEN_PROGRAM_ID DISC_TRANSFER(initializer_deposit writable, escrow_token writable, initializer signer)

  operation exchange
    who: taker
    when: Open
    then: Closed
    calls: TOKEN_PROGRAM_ID DISC_TRANSFER(taker_deposit writable, initializer_receive writable, taker signer)

  operation cancel
    who: initializer
    when: Open
    then: Closed
    calls: TOKEN_PROGRAM_ID DISC_TRANSFER(escrow_token writable, initializer_deposit writable, escrow_pda signer)

  invariant conservation "total tokens preserved across initialize, exchange, cancel"
