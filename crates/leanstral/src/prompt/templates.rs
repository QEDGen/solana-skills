// Prompt templates - single source of truth for all LLM guidance

pub const SPEC_PATTERNS: &str = r#"## Specification-Driven Proof Patterns

### Defining Specification Structures

Specifications capture preconditions that must hold before an instruction executes:

```lean
structure InitializeSpec where
  preState : EscrowState
  signer : Pubkey
  amount : U64
  -- Preconditions (must be Props)
  amount_valid : amount.val <= U64_MAX
  is_initializer : signer = preState.initializer
  has_balance : preState.balance >= amount.val
```

### Transition Functions with Specs

Transitions check spec preconditions and produce postconditions:

```lean
def initializeTransition (spec : InitializeSpec) : Option EscrowState :=
  if spec.is_initializer ∧ spec.amount_valid ∧ spec.has_balance then
    some { spec.preState with
      amount := spec.amount
      lifecycle := Lifecycle.open
    }
  else none
```

### Theorem Pattern with Specs

Theorems prove: IF spec preconditions hold, THEN postconditions hold after transition:

```lean
theorem initialize_satisfies_spec (spec : InitializeSpec) (post : EscrowState)
    (h : initializeTransition spec = some post) :
    -- Postconditions
    post.amount = spec.amount ∧
    post.lifecycle = Lifecycle.open ∧
    post.amount.val <= U64_MAX := by
  unfold initializeTransition at h
  split_ifs at h with h_checks
  · cases h
    constructor
    · rfl
    constructor
    · rfl
    · exact spec.amount_valid
  · contradiction
```

### Trust Boundary

- **We prove**: Implementation satisfies spec (preconditions → postconditions)
- **Runtime checks**: Preconditions actually hold before execution
- **Framework-agnostic**: Works for any language, not just Anchor

DO NOT attempt to prove preconditions from the transition alone.
Preconditions are INPUTS that runtime must validate.

"#;

pub const COMMON_PATTERNS: &str = r#"## Common Tactic Patterns - READ CAREFULLY

### Working with Option types and cases

When working with Option types and hypotheses of form `h : someFunc(...) = some result`:

**Preferred approach**: Use `cases` after `unfold`:
```lean
-- Given: h : transition preState = some postState
unfold transition at h
cases h  -- Simplifies Option.some away and substitutes the inner value
-- Now continue with the proof
```

**Alternative (when Option.some.inj is needed)**:
```lean
apply Option.some.inj at h  -- h : inner_expression = result
-- Then use rw [← h] to substitute result in goal
```

### Proving with if-then-else: Use unfold before split_ifs

**CRITICAL**: When proving theorems about functions with if-then-else:
- Use `unfold` to expand the function definition
- Then use `split_ifs` to case-split on the condition
- Do NOT use `simp` before `split_ifs` - simp may simplify away the if-then-else structure

Example:
```lean
-- Given: transition defined with if-then-else
-- Goal: theorem about transition
unfold transition at h      -- Expand definition but preserve if-structure
split_ifs at h with h_eq    -- Case split: h_eq available in true branch
· exact h_eq                -- Use the equality from true branch
· simp at h                 -- False branch leads to contradiction
```

**DON'T**:
```lean
simp [transition] at h   -- BAD: may eliminate if-then-else before split_ifs
split_ifs at h           -- ERROR: no if-then-else to split!
```

### Opaque Values: Pubkey and Amount Preconditions

CRITICAL: `Pubkey` is `abbrev Nat` and `U64` is `abbrev Nat`. Context structure fields like `ctx.from_account` and `ctx.to_account` are OPAQUE — you cannot prove they are distinct or bounded without explicit hypotheses.

**WRONG** — trying to prove distinctness without a hypothesis:
```lean
-- This will FAIL: simp/decide/omega/rfl/injection cannot prove ctx.from ≠ ctx.to
theorem cpi_valid (ctx : Context) : ctx.from ≠ ctx.to := by simp  -- ERROR!
```

**CORRECT** — add preconditions:
```lean
theorem cpi_valid (ctx : Context)
    (h_distinct : ctx.from ≠ ctx.to)     -- Precondition for distinctness
    (h_amount : ctx.amount ≤ U64_MAX)    -- Precondition for amount bound
    : transferCpiValid (build_cpi ctx) := by
  unfold build_cpi transferCpiValid
  exact ⟨rfl, h_distinct, h_amount⟩
```

This applies to ANY property involving opaque struct fields that cannot be proven definitionally.

### If-Expressions with Proof Bindings

- Use `if h : condition then ...` ONLY when you need the proof `h` in the then/else branches
- If you don't use `h`, write `if condition then ...` without the binding
- This avoids "unused variable" warnings

Example:
```lean
-- BAD: h is never used
if h : x = y then some () else none

-- GOOD: no unused variable
if x = y then some () else none

-- GOOD: h is actually used
if h : x = y then proof_using_h h else none
```
"#;

pub const PREAMBLE: &str = r#"I need a single Lean 4 module that compiles under Lean 4.15 + Mathlib 4.15.

IMPORTANT: A theorem skeleton with correct parameter declarations is provided below.
Your task is to COMPLETE this theorem by:
1. Defining any required types and transition functions referenced in the theorem signature
2. Replacing the `sorry` placeholder with a complete proof

Return Lean code only.
Do not duplicate declarations.
Do not modify the provided theorem signature - all parameters are already correctly declared.
Do not redefine any APIs listed in the Support API section below. You may define NEW helpers not listed there.
If a proof is incomplete, use `sorry` inside the proof body.
Prefer a smaller explicit model that compiles over a larger broken one.

IMPORTANT: The Support API section below lists definitions that are ALREADY IMPORTED from the support modules.
You MUST use these existing definitions. DO NOT redefine any function, type, or lemma listed in the Support API.
If you need a definition not in the Support API, you may define it yourself.

SPECIFICATION-DRIVEN VERIFICATION: We use explicit preconditions, not framework assumptions.
- Preconditions are formal requirements that must hold BEFORE an instruction executes
- Define a Spec structure capturing all preconditions for the instruction
- Transition functions take a Spec parameter and check preconditions
- Runtime is responsible for enforcing preconditions; we prove postconditions GIVEN preconditions hold
- Example: `structure InitializeSpec where amount : U64; amount_valid : amount <= U64_MAX`

VERIFICATION SCOPE: We verify the program's business logic, NOT external dependencies.
- CPI operations (token::transfer, system_program calls) are TRUSTED via axioms
- We verify the program passes correct parameters to these operations
- We verify authorization, state transitions, and compositional properties
- Do NOT attempt to model SPL Token internals, PDA derivation, or Solana runtime
"#;

pub const OUTPUT_REQUIREMENTS: &str = r#"## Output Requirements
1. Define the model types and executable transition functions first
2. Import the listed support modules and write `open Leanstral.Solana`; use that surface consistently
3. State the theorem only after the semantics are defined
4. Use only Lean 4.15 / Mathlib 4.15 identifiers you are confident exist
5. Prefer concrete definitions over placeholders
6. Prove this one property only
7. Do not name a declaration exactly `initialize`; use names like `initializeTransition`, `exchangeTransition`, or `cancelTransition` instead
8. Do not define or use unqualified global aliases outside the `Leanstral.Solana` surface
9. Do not use tactic combinators such as `all_goals`, `try`, `repeat`, `first |`, or `admit`; prefer short direct proofs with `unfold`, `cases`, `constructor`, and `exact`
10. IMPORTANT: Use `unfold` instead of `simp` when you need to preserve if-then-else structure for `split_ifs` tactic
"#;

// Category-specific hints
pub fn hint_for_category(category: &str) -> &'static str {
    match category {
        "access_control" => ACCESS_CONTROL_HINT,
        "cpi_correctness" => CPI_CORRECTNESS_HINT,
        "state_machine" => STATE_MACHINE_HINT,
        "arithmetic_safety" => ARITHMETIC_SAFETY_HINT,
        _ => "Keep the model small and explicit.",
    }
}

const ACCESS_CONTROL_HINT: &str = r#"Model only the authorization condition that matters for this instruction. Import the relevant support modules, write `open Leanstral.Solana`, and use that surface consistently. Use one local program state structure, typically `EscrowState`, plus `Pubkey`; do not define extra local types like `AccountState`, `CancelPreState`, or helper state wrappers for this v1 access-control theorem. Define `cancelTransition : EscrowState -> Pubkey -> Option Unit` or an equally small transition. Define authorization as a direct `Prop` equality like `signer = preState.initializer`; do not define authorization as an existential over post-state reachability. In authorization predicates and theorem statements, use propositional equality `=` and never boolean equality `==`. Do not use `decide` for v1 access-control proofs. Do not mix propositional equality with boolean equality. In record updates, use Lean syntax `field := value`, never `field = value`.

CRITICAL PATTERN: When proving access control with `h : transition preState signer ≠ none`:
```lean
theorem access_control (h : transition preState signer ≠ none) :
    signer = preState.initializer := by
  unfold transition at h  -- Use unfold, NOT simp
  split_ifs at h with h_eq  -- Split on the if-condition
  · exact h_eq            -- True branch: h_eq proves the goal
  · contradiction         -- False branch: h says some ≠ none, but we have none
```

DO NOT use `simp [transition] at h` before `split_ifs` - use `unfold`.

Prefer theorem statements of the exact form `cancelTransition preState signer ≠ none -> signer = preState.initializer` or an equivalent direct authorization predicate. Avoid tactic combinators like `all_goals` and `try`."#;

const CPI_CORRECTNESS_HINT: &str = r#"Model the CPI (Cross-Program Invocation) parameters that the instruction constructs. Import Leanstral.Solana.Cpi and use the TransferCpi structure and validity predicates.

VERIFICATION SCOPE: We verify that CPI parameters are CONSTRUCTED correctly - NOT that external programs execute correctly.
- Prove: CPI has correct program ID, distinct from/to accounts, bounded amounts, correct authorities
- Trust: SPL Token implementation (external dependency)

CRITICAL: `transferCpiValid` requires `program = TOKEN_PROGRAM_ID ∧ from ≠ to ∧ amount ≤ U64_MAX`.
Since `Pubkey` is `Nat` and context fields are opaque, you CANNOT prove `from ≠ to` or `amount ≤ U64_MAX` without explicit hypotheses. These must be added as PRECONDITIONS to the theorem.

DO NOT use `simp` alone to prove CPI validity — it cannot discharge `from ≠ to` for opaque pubkeys.
DO NOT use `decide` on goals containing free variables (it requires closed terms).
DO NOT use `omega` to prove `amount ≤ U64_MAX` for opaque Nat values.
DO NOT use `rfl` or `injection` on inequality goals between opaque pubkeys.

CRITICAL PATTERN for single-transfer CPI:
```lean
structure CancelContext where
  escrow_token_account : Pubkey
  initializer_deposit : Pubkey
  authority : Pubkey
  amount : U64

def cancel_build_transfer_cpi (ctx : CancelContext) : TransferCpi :=
  { program := TOKEN_PROGRAM_ID
  , «from» := ctx.escrow_token_account
  , «to» := ctx.initializer_deposit
  , authority := ctx.authority
  , amount := ctx.amount }

-- NOTE: h_distinct and h_amount are REQUIRED preconditions
theorem cancel_cpi_valid (ctx : CancelContext)
    (h_distinct : ctx.escrow_token_account ≠ ctx.initializer_deposit)
    (h_amount : ctx.amount ≤ U64_MAX) :
    let cpi := cancel_build_transfer_cpi ctx
    transferCpiValid cpi ∧
    cpi.authority = ctx.authority ∧
    cpi.«from» ≠ cpi.«to» := by
  unfold cancel_build_transfer_cpi transferCpiValid
  exact ⟨⟨rfl, h_distinct, h_amount⟩, rfl, h_distinct⟩
```

CRITICAL PATTERN for multi-transfer CPI:
```lean
-- For multiple CPIs, add distinctness + amount preconditions for EACH transfer
theorem exchange_cpis_valid (ctx : ExchangeContext)
    (h_distinct1 : ctx.taker_deposit ≠ ctx.initializer_receive)
    (h_distinct2 : ctx.escrow_token_account ≠ ctx.taker_receive)
    (h_amount1 : ctx.taker_amount ≤ U64_MAX)
    (h_amount2 : ctx.initializer_amount ≤ U64_MAX) :
    let cpis := exchange_build_transfer_cpis ctx
    multipleTransfersValid cpis ∧
    (∀ cpi ∈ cpis, cpi.program = TOKEN_PROGRAM_ID) := by
  unfold exchange_build_transfer_cpis
  unfold multipleTransfersValid
  simp only [Leanstral.Solana.transferCpiValid, Leanstral.Solana.Cpi.transferCpiValid]
  constructor
  · constructor
    · intro cpi h
      simp [List.mem_cons, List.mem_singleton] at h
      rcases h with rfl | rfl
      · exact ⟨rfl, h_distinct1, h_amount1⟩
      · exact ⟨rfl, h_distinct2, h_amount2⟩
    · intro cpi h
      simp [List.mem_cons, List.mem_singleton] at h
      rcases h with rfl | rfl
      · exact h_distinct1
      · exact h_distinct2
  · intro cpi h
    simp [List.mem_cons, List.mem_singleton] at h
    rcases h with rfl | rfl <;> rfl
```

NOTE on `unfold transferCpiValid`: After `open Leanstral.Solana`, the abbreviation may not unfold directly.
Use `simp only [Leanstral.Solana.transferCpiValid, Leanstral.Solana.Cpi.transferCpiValid]` to reduce through abbreviation layers, or use `unfold Leanstral.Solana.Cpi.transferCpiValid` with the fully qualified name.

DO NOT model token balances or state changes - we only verify parameter construction.
DO NOT use axioms like transfer_preserves_total - this is about CPI interface correctness.

Keep the context structure minimal - only fields needed for CPI construction.
In record creation, use Lean syntax `field := value`, never `field = value`."#;

const STATE_MACHINE_HINT: &str = r#"Model only the lifecycle flag or closed/open state that matters. Import the relevant support modules, write `open Leanstral.Solana`, and use that surface consistently. Use the `Lifecycle` type and lemmas from the support library: `closes_is_closed`, `closes_was_open`, `closed_irreversible`.

CRITICAL: Use `structure EscrowState where` NOT type aliases like `def EscrowState : Type := { ... }`.
Example:
```lean
structure EscrowState where
  lifecycle : Lifecycle
  initializer : Pubkey
  ...
```

CRITICAL PATTERN: When proving state machine properties with `h : transition preState = some postState`:
```lean
theorem closes_escrow (h : transition preState = some postState) :
    postState.lifecycle = Lifecycle.closed := by
  unfold transition at h  -- Preserve structure
  cases h                 -- Simplify Option.some
  rfl                     -- Both sides are definitionally equal
```

Do not define a custom local `AccountState` when the theorem is really about lifecycle. Prefer a direct theorem shape like `postState.lifecycle = Lifecycle.closed` or `closes st.lifecycle (cancelTransition st).lifecycle`. Apply the support library lemmas to simplify the proof. Do not write theorem statements using placeholders like `some _`; introduce any post-state explicitly if needed."#;

const ARITHMETIC_SAFETY_HINT: &str = r#"Model only the numeric parameters and bounds that matter for this obligation. Import the relevant support modules, write `open Leanstral.Solana`, and use that surface consistently.

SPECIFICATION-DRIVEN APPROACH:
Use a ValidState predicate to capture all type bounds as preconditions:

```lean
def ValidState (s : ProgramState) : Prop :=
  s.amount <= U64_MAX ∧
  s.taker_amount <= U64_MAX ∧
  s.bump <= U8_MAX

theorem transition_preserves_validity (spec : TransitionSpec)
    (pre post : ProgramState)
    (h_valid : ValidState pre)
    (h : transition spec pre = some post) :
    ValidState post := by
  unfold ValidState at *
  unfold transition at h
  cases h
  -- Prove each component of ValidState for post
  constructor
  · -- prove post.amount <= U64_MAX using h_valid
  constructor
  · -- prove post.taker_amount <= U64_MAX using h_valid
  · -- prove post.bump <= U8_MAX using h_valid
```

DO NOT try to prove `pre.amount <= U64_MAX` from the transition alone - this is a PRECONDITION.
Use ValidState as a precondition and prove it's PRESERVED by the transition.

Avoid unrelated account/token semantics. Do not write theorem statements using placeholders like `some _`."#;

// Support API documentation
pub fn support_api_for_modules(modules: &[String]) -> String {
    let mut lines = vec!["open Leanstral.Solana".to_string()];

    if modules.iter().any(|m| m == "Leanstral.Solana.Account") {
        lines.extend([
            "-- Account surface".to_string(),
            "Pubkey : Type".to_string(),
            "U64 : Type".to_string(),
            "U8 : Type".to_string(),
            "Account : Type".to_string(),
            "AccountState := Account".to_string(),
            "Account.key : Pubkey".to_string(),
            "Account.authority : Pubkey".to_string(),
            "Account.balance : Nat".to_string(),
            "Account.writable : Bool".to_string(),
            "canWrite : Pubkey -> Account -> Prop".to_string(),
            "findByKey : List Account -> Pubkey -> Option Account".to_string(),
            "findByAuthority : List Account -> Pubkey -> Option Account".to_string(),
            "-- Lemmas:".to_string(),
            "find_map_update_other : find by authority after updating different account is unchanged".to_string(),
            "find_map_update_same : find by authority after updating target account returns updated account".to_string(),
            "find_by_key_map_update_other : find by key after updating different account is unchanged".to_string(),
            "find_by_key_map_update_same : find by key after updating target account returns updated account".to_string(),
        ]);
    }

    if modules.iter().any(|m| m == "Leanstral.Solana.Authority") {
        lines.extend([
            "-- Authority surface".to_string(),
            "Authorized : Pubkey -> Pubkey -> Prop".to_string(),
        ]);
    }

    if modules.iter().any(|m| m == "Leanstral.Solana.Cpi") {
        lines.extend([
            "-- CPI surface".to_string(),
            "TransferCpi : Type  -- structure with program, from, to, authority, amount fields".to_string(),
            "MintToCpi : Type".to_string(),
            "BurnCpi : Type".to_string(),
            "CloseCpi : Type".to_string(),
            "TOKEN_PROGRAM_ID : Pubkey".to_string(),
            "SYSTEM_PROGRAM_ID : Pubkey".to_string(),
            "transferCpiValid : TransferCpi -> Prop  -- defined as: cpi.program = TOKEN_PROGRAM_ID ∧ cpi.from ≠ cpi.to ∧ cpi.amount ≤ U64_MAX. REQUIRES explicit hypotheses for from ≠ to and amount ≤ U64_MAX since Pubkey/U64 fields are opaque Nat values.".to_string(),
            "mintToCpiValid : MintToCpi -> Prop".to_string(),
            "burnCpiValid : BurnCpi -> Prop".to_string(),
            "closeCpiValid : CloseCpi -> Prop".to_string(),
            "multipleTransfersValid : List TransferCpi -> Prop  -- defined as: (∀ cpi ∈ transfers, transferCpiValid cpi) ∧ (∀ cpi ∈ transfers, cpi.from ≠ cpi.to). Same precondition requirements as transferCpiValid for each element.".to_string(),
        ]);
    }

    if modules.iter().any(|m| m == "Leanstral.Solana.Token") {
        lines.extend([
            "-- Token surface (legacy - prefer Cpi for new proofs)".to_string(),
            "TokenAccount := Account".to_string(),
            "Mint : Type".to_string(),
            "Program : Type".to_string(),
        ]);
    }

    if modules.iter().any(|m| m == "Leanstral.Solana.State") {
        lines.extend([
            "-- State surface".to_string(),
            "Lifecycle : Type".to_string(),
            "closes : Lifecycle -> Lifecycle -> Prop".to_string(),
            "-- Lemmas:".to_string(),
            "closed_irreversible : closed cannot transition to open".to_string(),
            "closes_is_closed : closes implies result is closed".to_string(),
            "closes_was_open : closes implies original was open".to_string(),
        ]);
    }

    if modules.iter().any(|m| m == "Leanstral.Solana.Valid") {
        lines.extend([
            "-- Validity surface".to_string(),
            "U8_MAX : Nat := 255".to_string(),
            "U16_MAX : Nat := 65535".to_string(),
            "U32_MAX : Nat := 4294967295".to_string(),
            "U64_MAX : Nat := 18446744073709551615".to_string(),
            "U128_MAX : Nat := 340282366920938463463374607431768211455".to_string(),
            "valid_u8 : Nat -> Prop".to_string(),
            "valid_u16 : Nat -> Prop".to_string(),
            "valid_u32 : Nat -> Prop".to_string(),
            "valid_u64 : Nat -> Prop".to_string(),
            "valid_u128 : Nat -> Prop".to_string(),
            "-- Lemmas:".to_string(),
            "valid_u64_preserved_by_zero : validity preserved when setting to zero".to_string(),
            "valid_u64_preserved_by_same : validity preserved when unchanged".to_string(),
        ]);
    }

    lines.join("\n")
}
