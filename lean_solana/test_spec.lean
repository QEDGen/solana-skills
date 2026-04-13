import QEDGen.Solana.Spec

open QEDGen.Solana.SpecDSL
open QEDGen.Solana

/-!
# qedspec DSL tests

Run: `cd lean_solana && lake build && lake env lean test_spec.lean`

These are behavioral tests — they prove properties of the generated code,
not just that definitions exist. A regression in code generation will cause
a proof to fail, not silently pass.

Tests cover:
  1. Escrow — SPL Token CPI, lifecycle, U64 bounds, Lean keyword (`initialize`)
  2. Vault — System Program CPI, signer_writable flag
  3. Staking — mixed discriminators (transfer, burn, mint_to)
  4. Governance — no CPI, pure lifecycle + access control
  5. Error: invalid account flag
-/

-- ============================================================================
-- 1. Escrow — SPL Token CPI
-- ============================================================================

qedspec Escrow where
  state
    maker : Pubkey
    taker : Pubkey
    offered : U64
    wanted : U64

  operation initialize
    who: maker
    when: Uninitialized
    then: Open

  operation exchange
    who: taker
    when: Open
    then: Complete
    calls: TOKEN_PROGRAM_ID DISC_TRANSFER(escrow_token_account writable, taker_token_account writable, maker signer)

  operation cancel
    who: maker
    when: Open
    then: Cancelled
    calls: TOKEN_PROGRAM_ID DISC_TRANSFER(escrow_token_account writable, maker_token_account writable, maker signer)

  invariant conservation "total tokens preserved"

-- 1a. Transition rejects wrong signer
example (s : Escrow.State) (p : Pubkey)
    (h_ne : p ≠ s.maker) (h_st : s.status = .Uninitialized) :
    Escrow.initializeTransition s p = none := by
  simp [Escrow.initializeTransition, h_ne]

-- 1b. Transition rejects wrong lifecycle state
example (s : Escrow.State) (h_st : s.status ≠ .Uninitialized) :
    Escrow.initializeTransition s s.maker = none := by
  simp [Escrow.initializeTransition, h_st]

-- 1c. Transition succeeds with correct signer + state
example (s : Escrow.State) (h_st : s.status = .Uninitialized) :
    Escrow.initializeTransition s s.maker = some { s with status := .Open } := by
  simp [Escrow.initializeTransition, h_st]

-- 1d. Access control is provable (not just declared)
example (s : Escrow.State) (p : Pubkey)
    (h : Escrow.initializeTransition s p ≠ none) : p = s.maker := by
  simp [Escrow.initializeTransition] at h
  exact h.1

-- 1e. State machine is provable
example (s s' : Escrow.State) (p : Pubkey)
    (h : Escrow.exchangeTransition s p = some s') :
    s.status = .Open ∧ s'.status = .Complete := by
  simp [Escrow.exchangeTransition] at h
  obtain ⟨⟨_, h_pre⟩, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

-- 1f. CPI build_cpi targets correct program
example (ctx : Escrow.exchangeCpiContext) :
    (Escrow.exchange_build_cpi ctx).programId = TOKEN_PROGRAM_ID := by
  rfl

-- 1g. CPI build_cpi has correct number of accounts
example (ctx : Escrow.exchangeCpiContext) :
    (Escrow.exchange_build_cpi ctx).accounts.length = 3 := by
  rfl

-- 1h. CPI build_cpi has correct discriminator
example (ctx : Escrow.exchangeCpiContext) :
    (Escrow.exchange_build_cpi ctx).data = DISC_TRANSFER := by
  rfl

-- 1i. CPI account flags are correct (index 0: writable, not signer)
example (ctx : Escrow.exchangeCpiContext) :
    let cpi := Escrow.exchange_build_cpi ctx
    accountAt cpi 0 ctx.escrow_token_account false true := by
  rfl

-- 1j. CPI account flags are correct (index 2: signer, not writable)
example (ctx : Escrow.exchangeCpiContext) :
    let cpi := Escrow.exchange_build_cpi ctx
    accountAt cpi 2 ctx.maker true false := by
  rfl

-- 1k. Cancel CPI also targets TOKEN_PROGRAM_ID
example (ctx : Escrow.cancelCpiContext) :
    (Escrow.cancel_build_cpi ctx).programId = TOKEN_PROGRAM_ID := by
  rfl

-- ============================================================================
-- 2. Vault — System Program CPI
-- ============================================================================

qedspec Vault where
  state
    authority : Pubkey
    balance : U64

  operation initialize
    who: authority
    when: Uninitialized
    then: Active
    calls: SYSTEM_PROGRAM_ID DISC_SYS_CREATE_ACCOUNT(payer signer_writable, vault signer_writable)

  operation withdraw
    who: authority
    when: Active
    then: Active
    calls: SYSTEM_PROGRAM_ID DISC_SYS_TRANSFER(vault signer_writable, recipient writable)

  operation close
    who: authority
    when: Active
    then: Closed

-- 2a. System Program CPI targets correct program
example (ctx : Vault.initializeCpiContext) :
    (Vault.initialize_build_cpi ctx).programId = SYSTEM_PROGRAM_ID := by
  rfl

-- 2b. System Program discriminator (4-byte LE)
example (ctx : Vault.initializeCpiContext) :
    (Vault.initialize_build_cpi ctx).data = DISC_SYS_CREATE_ACCOUNT := by
  rfl

-- 2c. signer_writable flag: both isSigner=true and isWritable=true
example (ctx : Vault.initializeCpiContext) :
    let cpi := Vault.initialize_build_cpi ctx
    accountAt cpi 0 ctx.payer true true := by
  rfl

-- 2d. withdraw CPI: vault is signer_writable, recipient is writable-only
example (ctx : Vault.withdrawCpiContext) :
    let cpi := Vault.withdraw_build_cpi ctx
    accountAt cpi 0 ctx.vault true true ∧
    accountAt cpi 1 ctx.recipient false true := by
  exact ⟨rfl, rfl⟩

-- 2e. close has no CPI — transition still works
example (s : Vault.State) (h_st : s.status = .Active) :
    Vault.closeTransition s s.authority = some { s with status := .Closed } := by
  simp [Vault.closeTransition, h_st]

-- ============================================================================
-- 3. Staking — mixed discriminators
-- ============================================================================

qedspec Staking where
  state
    authority : Pubkey
    staker : Pubkey
    staked : U64
    reward : U64

  operation initialize
    who: authority
    when: Uninitialized
    then: Active

  operation stake
    who: staker
    when: Active
    then: Active
    calls: TOKEN_PROGRAM_ID DISC_TRANSFER(staker_token writable, pool_token writable, staker signer)

  operation unstake
    who: staker
    when: Active
    then: Active
    calls: TOKEN_PROGRAM_ID DISC_BURN(pool_token writable, mint writable, staker signer)

  operation claim
    who: staker
    when: Active
    then: Active
    calls: TOKEN_PROGRAM_ID DISC_MINT_TO(reward_mint writable, staker_reward writable, mint_authority signer)

  invariant stake_conservation "staked tokens preserved"

-- 3a. Each operation gets the right discriminator
example (ctx : Staking.stakeCpiContext) :
    (Staking.stake_build_cpi ctx).data = DISC_TRANSFER := by rfl
example (ctx : Staking.unstakeCpiContext) :
    (Staking.unstake_build_cpi ctx).data = DISC_BURN := by rfl
example (ctx : Staking.claimCpiContext) :
    (Staking.claim_build_cpi ctx).data = DISC_MINT_TO := by rfl

-- 3b. All three CPIs target TOKEN_PROGRAM_ID
example (ctx : Staking.stakeCpiContext) :
    (Staking.stake_build_cpi ctx).programId = TOKEN_PROGRAM_ID := by rfl
example (ctx : Staking.unstakeCpiContext) :
    (Staking.unstake_build_cpi ctx).programId = TOKEN_PROGRAM_ID := by rfl
example (ctx : Staking.claimCpiContext) :
    (Staking.claim_build_cpi ctx).programId = TOKEN_PROGRAM_ID := by rfl

-- 3c. Self-loop lifecycle (Active → Active)
example (s s' : Staking.State) (p : Pubkey)
    (h : Staking.stakeTransition s p = some s') :
    s.status = .Active ∧ s'.status = .Active := by
  simp [Staking.stakeTransition] at h
  obtain ⟨⟨_, h_pre⟩, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

-- ============================================================================
-- 4. Governance — no CPI at all
-- ============================================================================

qedspec Governance where
  state
    admin : Pubkey
    proposer : Pubkey
    votes : U64

  operation createProposal
    who: proposer
    when: Idle
    then: Voting

  operation vote
    who: admin
    when: Voting
    then: Voting

  operation finalize
    who: admin
    when: Voting
    then: Executed

  invariant quorum "votes meet threshold"

-- 4a. Lifecycle: Idle → Voting → Executed
example (s : Governance.State) (h : s.status = .Idle) :
    Governance.createProposalTransition s s.proposer =
      some { s with status := .Voting } := by
  simp [Governance.createProposalTransition, h]

example (s : Governance.State) (h : s.status = .Voting) :
    Governance.finalizeTransition s s.admin =
      some { s with status := .Executed } := by
  simp [Governance.finalizeTransition, h]

-- 4b. Self-loop: vote stays in Voting
example (s : Governance.State) (h : s.status = .Voting) :
    Governance.voteTransition s s.admin =
      some { s with status := .Voting } := by
  simp [Governance.voteTransition, h]

-- 4c. Access control: only admin can vote
example (s : Governance.State) (p : Pubkey)
    (h : Governance.voteTransition s p ≠ none) : p = s.admin := by
  simp [Governance.voteTransition] at h
  exact h.1

-- ============================================================================
-- 5. Optional who:/when:/then:
-- ============================================================================

-- 5a. No who: — anyone can call (no access_control theorem generated)
qedspec OpenPool where
  state
    total : U64

  operation contribute
    when: Active
    then: Active
    takes: amount U64
    guard: "s.total + amount ≤ U64_MAX"
    effect: total add amount

-- Transition succeeds for any signer
example (s : OpenPool.State) (p : Pubkey)
    (h_st : s.status = .Active) (h_g : s.total + 42 ≤ U64_MAX) :
    OpenPool.contributeTransition s p 42 =
      some { s with total := s.total + 42, status := .Active } := by
  simp [OpenPool.contributeTransition, h_st, h_g]

-- Operation inductive and applyOp exist
#check @OpenPool.Operation
#check @OpenPool.applyOp

-- 5b. No when:/then: — no lifecycle at all
qedspec Counter where
  state
    admin : Pubkey
    count : U64

  operation increment
    who: admin
    takes: n U64
    guard: "s.count + n ≤ U64_MAX"
    effect: count add n

-- No Status type generated — State has no status field
example : Counter.State := { admin := ⟨0,0,0,0⟩, count := 0 }

-- Transition checks only signer + guard
example (s : Counter.State) (h_g : s.count + 5 ≤ U64_MAX) :
    Counter.incrementTransition s s.admin 5 =
      some { s with count := s.count + 5 } := by
  simp [Counter.incrementTransition, h_g]

-- Access control still works
example (s : Counter.State) (p : Pubkey) (n : U64)
    (h : Counter.incrementTransition s p n ≠ none) : p = s.admin := by
  simp [Counter.incrementTransition] at h
  exact h.1

-- 5c. No who:, no when:, no then: — pure arithmetic operation
qedspec Accumulator where
  state
    value : U64

  operation add_value
    takes: amount U64
    guard: "s.value + amount ≤ U64_MAX"
    effect: value add amount

-- No Status, no signer check — transition always succeeds if guard holds
example (s : Accumulator.State) (p : Pubkey) (h_g : s.value + 10 ≤ U64_MAX) :
    Accumulator.add_valueTransition s p 10 =
      some { s with value := s.value + 10 } := by
  simp [Accumulator.add_valueTransition, h_g]

-- No access_control or state_machine theorems generated (nothing to assert)

-- ============================================================================
-- 6. Error: invalid account flag
-- ============================================================================

/--
error: qedspec: unknown account flag 'mutable' for account 'source'. Use: readonly, writable, signer, signer_writable
-/
#guard_msgs in
qedspec BadFlag where
  state
    owner : Pubkey

  operation transfer
    who: owner
    when: Active
    then: Active
    calls: TOKEN_PROGRAM_ID DISC_TRANSFER(source mutable, dest writable, auth signer)

-- ============================================================================
-- 7. Error: bad field reference in guard
-- ============================================================================

/--
error: qedspec: guard in operation 'deposit' references unknown field 's.balancee'. Available: [owner, balance]
-/
#guard_msgs in
qedspec BadGuard where
  state
    owner : Pubkey
    balance : U64

  operation deposit
    who: owner
    when: Active
    then: Active
    takes: amount U64
    guard: "s.balancee + amount ≤ U64_MAX"

-- ============================================================================
-- 8. Error: bad field reference in property
-- ============================================================================

/--
error: qedspec: property 'bounded' references unknown field 's.balnce'. Available: [owner, balance]
-/
#guard_msgs in
qedspec BadProp where
  state
    owner : Pubkey
    balance : U64

  operation deposit
    who: owner
    when: Active
    then: Active

  property bounded "s.balnce ≤ U64_MAX"
    preserved_by: deposit

-- ============================================================================
-- 9. doc: clause — intent annotations
-- ============================================================================

qedspec Documented where
  state
    owner : Pubkey
    balance : U64

  operation withdraw
    doc: "Only the vault owner can withdraw funds"
    who: owner
    takes: amount U64
    guard: "amount ≤ s.balance"
    effect: balance sub amount

-- Verify Operation inductive and applyOp exist
#check @Documented.Operation
#check @Documented.applyOp

-- Transition function still works directly (access control provable from transition)
example (s : Documented.State) (p : Pubkey) (amount : Nat)
    (h : Documented.withdrawTransition s p amount ≠ none) : p = s.owner := by
  simp [Documented.withdrawTransition] at h; exact h.1

-- ============================================================================
-- 10. Codegen-oriented syntax (events, errors, PDAs, context, emits, program_id)
--     These clauses are accepted by the parser but ignored by the elaborator.
--     The Lean proof system works unchanged — Rust `qedgen codegen` extracts
--     the extra data for Quasar skeleton generation.
-- ============================================================================

qedspec TokenEscrow where
  program_id: "22222222222222222222222222222222222222222222"

  state
    maker : Pubkey
    mint_a : Pubkey
    mint_b : Pubkey
    receive : U64
    bump : U8

  pda escrow "escrow", maker

  event MakeEvent { maker : Pubkey, deposit : U64, receive : U64 }
  event TakeEvent { escrow : Pubkey }

  errors: InvalidAmount, Unauthorized, Expired

  operation make
    doc: "Maker creates escrow and deposits token A"
    who: maker
    when: Uninitialized
    then: Open
    takes: deposit U64
    guard: "deposit > 0"
    emits: MakeEvent
    context: {
      maker : Signer, mut
      escrow : Account, Escrow, init, payer(maker), seeds(escrow), bump
      mint_a : Account, Mint
      mint_b : Account, Mint
      maker_ta_a : Account, Token, mut
      vault_ta_a : Account, Token, mut, init_if_needed, payer(maker), token_mint(mint_a), token_authority(escrow)
      token_program : Program, Token
      system_program : Program, System
    }

  operation take
    doc: "Taker completes the escrow swap"
    when: Open
    then: Closed
    emits: TakeEvent
    context: {
      taker : Signer, mut
    }

  property conservation "s.receive ≤ U64_MAX"
    preserved_by: make

-- 10a. Existing proof generation still works with new clauses present
-- Access control
example (s : TokenEscrow.State) (p : Pubkey) (d : Nat)
    (h : TokenEscrow.makeTransition s p d ≠ none) : p = s.maker := by
  simp [TokenEscrow.makeTransition] at h; exact h.1

-- 10b. State machine
example (s s' : TokenEscrow.State) (p : Pubkey) (d : Nat)
    (h : TokenEscrow.makeTransition s p d = some s') :
    s.status = .Uninitialized ∧ s'.status = .Open := by
  simp [TokenEscrow.makeTransition] at h
  obtain ⟨⟨_, h_pre, _⟩, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

-- 10c. take transition (no who: = no signer check)
example (s : TokenEscrow.State) (p : Pubkey) (h_st : s.status = .Open) :
    TokenEscrow.takeTransition s p = some { s with status := .Closed } := by
  simp [TokenEscrow.takeTransition, h_st]

-- 10d. Property preservation: inductive theorem exists (one sorry for all ops)
#check @TokenEscrow.conservation_inductive

-- 10e. Operation inductive has both constructors
#check @TokenEscrow.Operation.make
#check @TokenEscrow.Operation.take

-- 10f. applyOp dispatches correctly
example (s : TokenEscrow.State) (p : Pubkey) (d : Nat) :
    TokenEscrow.applyOp s p (.make d) = TokenEscrow.makeTransition s p d := by
  rfl
example (s : TokenEscrow.State) (p : Pubkey) :
    TokenEscrow.applyOp s p .take = TokenEscrow.takeTransition s p := by
  rfl
