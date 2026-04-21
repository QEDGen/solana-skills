# Proof Patterns Reference

> **Phase 2 material — load only when writing Lean proofs.** Most
> programs finish at Phase 1 (spec + lint + adversarial probes +
> proptest + Kani). Enter Phase 2 for DeFi numerical invariants, new
> cryptographic primitives, or inductive sBPF proofs. See SKILL.md
> **Step 4** for the entry criteria and drift caveats.

## Access control

Signer must match authority:

```lean
def cancelTransition (s : ProgramState) (signer : Pubkey) : Option Unit :=
  if signer = s.authority then some () else none

theorem cancel_access_control (s : ProgramState) (signer : Pubkey)
    (h : cancelTransition s signer != none) :
    signer = s.authority := by
  unfold cancelTransition at h
  split_ifs at h with h_eq
  · exact h_eq
  · contradiction
```

## CPI correctness

Program, accounts, discriminator match (pure `rfl`):

```lean
def cancel_build_cpi (ctx : CancelContext) : CpiInstruction :=
  { programId := TOKEN_PROGRAM_ID
  , accounts := [
      ⟨ctx.escrow_token, false, true⟩,
      ⟨ctx.dest, false, true⟩,
      ⟨ctx.authority, true, false⟩
    ]
  , data := [DISC_TRANSFER]
  }

theorem cancel_cpi_correct (ctx : CancelContext) :
    let cpi := cancel_build_cpi ctx
    targetsProgram cpi TOKEN_PROGRAM_ID ∧
    accountAt cpi 0 ctx.escrow_token false true ∧
    accountAt cpi 1 ctx.dest false true ∧
    accountAt cpi 2 ctx.authority true false ∧
    hasDiscriminator cpi [DISC_TRANSFER] := by
  unfold cancel_build_cpi targetsProgram accountAt hasDiscriminator
  exact ⟨rfl, rfl, rfl, rfl, rfl⟩
```

## State machine

Lifecycle transitions:

```lean
def cancelTransition (s : ProgramState) : Option ProgramState :=
  if s.escrow.lifecycle = Lifecycle.open then
    some { escrow := { s.escrow with lifecycle := Lifecycle.closed } }
  else none

theorem cancel_closes_escrow (pre post : ProgramState)
    (h : cancelTransition pre = some post) :
    post.escrow.lifecycle = Lifecycle.closed := by
  unfold cancelTransition at h
  split_ifs at h with h_open
  cases h
  rfl
```

## Conservation

Invariant preserved across operations:

```lean
def conservation (s : EngineState) : Prop := s.V >= s.C_tot + s.I

def depositTransition (s : EngineState) (amount : Nat) : Option EngineState :=
  if s.V + amount <= MAX_VAULT_TVL then
    some { V := s.V + amount, C_tot := s.C_tot + amount, I := s.I }
  else none

theorem deposit_conservation (s s' : EngineState) (amount : Nat)
    (h_inv : conservation s)
    (h : depositTransition s amount = some s') :
    conservation s' := by
  unfold depositTransition at h
  split_ifs at h with h_le
  · cases h
    unfold conservation at h_inv ⊢  -- MUST unfold in BOTH hypothesis and goal
    omega
  · contradiction
```

## Arithmetic safety

Bounds preserved:

```lean
def initializeTransition (amount taker : Nat) : Option ProgramState :=
  if amount > 0 ∧ amount <= U64_MAX ∧ taker > 0 ∧ taker <= U64_MAX then
    some { initializer_amount := amount, taker_amount := taker }
  else none

theorem initialize_arithmetic_safety (amount taker : Nat) (post : ProgramState)
    (h : initializeTransition amount taker = some post) :
    post.initializer_amount <= U64_MAX ∧ post.taker_amount <= U64_MAX := by
  unfold initializeTransition at h
  split_ifs at h with h_bounds
  cases h
  exact ⟨h_bounds.2.1, h_bounds.2.2.2⟩
```

## Critical tactic rules

| Do | Don't |
|---|---|
| `unfold f at h` before `split_ifs` | `simp [f] at h` before `split_ifs` (kills if-structure) |
| `unfold pred at h_inv ⊢` for named predicates | `unfold pred` only in goal (omega can't see hypotheses) |
| `cases h` after `split_ifs` on `some = some` | `injection h` (unnecessary, cases handles it) |
| `omega` for linear arithmetic | `norm_num` for linear goals (omega is more reliable) |
| `exact ⟨rfl, rfl, rfl⟩` for conjunctions of rfl | `constructor` + `rfl` + `constructor` + `rfl` (verbose) |
| `if cond then ... else ...` without proof binding | `if h : cond then ...` when `h` is unused |

## Common errors and fixes

| Error | Fix |
|---|---|
| `omega could not prove the goal` | Unfold named predicates in hypotheses: `unfold pred at h ⊢` |
| `no goals to be solved` | Remove redundant tactic (e.g., `· contradiction` after auto-closed branch) |
| `unknown constant 'X'` | Check imports; add `import QEDGen.Solana.X` or `open QEDGen.Solana` |
| `tactic 'split_ifs' failed, no if-then-else` | Use `unfold` first, not `simp` |
| `unused variable 'h'` | Remove proof binding: `if h : cond` -> `if cond` |
| `omega` fails on address disjointness after stack writes | Normalize hypotheses with `simp [wrapAdd, toU64, ...]` (not `simp only`) |
| `simp` timeout on sBPF proofs | Check three performance rules (see references/sbpf.md) |
