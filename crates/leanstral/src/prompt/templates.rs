// Prompt templates - single source of truth for all LLM guidance

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
9. Do not use tactic combinators such as `all_goals`, `try`, `repeat`, `first |`, or `admit`; prefer short direct proofs with `simp`, `cases`, `rcases`, `constructor`, and `exact`
"#;

// Category-specific hints
pub fn hint_for_category(category: &str) -> &'static str {
    match category {
        "access_control" => ACCESS_CONTROL_HINT,
        "conservation" => CONSERVATION_HINT,
        "state_machine" => STATE_MACHINE_HINT,
        "arithmetic_safety" => ARITHMETIC_SAFETY_HINT,
        _ => "Keep the model small and explicit.",
    }
}

const ACCESS_CONTROL_HINT: &str = r#"Model only the authorization condition that matters for this instruction. Import the relevant support modules, write `open Leanstral.Solana`, and use that surface consistently. Use one local program state structure, typically `EscrowState`, plus `Pubkey`; do not define extra local types like `AccountState`, `CancelPreState`, or helper state wrappers for this v1 access-control theorem. Define `cancelTransition : EscrowState -> Pubkey -> Option Unit` or an equally small transition. Define authorization as a direct `Prop` equality like `signer = preState.initializer`; do not define authorization as an existential over post-state reachability. In authorization predicates and theorem statements, use propositional equality `=` and never boolean equality `==`. Do not use `decide` for v1 access-control proofs. Do not mix propositional equality with boolean equality. In record updates, use Lean syntax `field := value`, never `field = value`. Prefer theorem statements of the exact form `cancelTransition preState signer ≠ none -> signer = preState.initializer` or an equivalent direct authorization predicate. When proving an `if`-based theorem, unfold the transition, split on the `if`, and use the equality hypothesis from the true branch directly with `exact` or `simpa`; do not use `rfl` unless both sides are definitionally equal. Avoid tactic combinators like `all_goals` and `try`."#;

const CONSERVATION_HINT: &str = r#"You MUST use 'trackedTotal' from Leanstral.Solana.Token - DO NOT redefine it.
You MUST use conservation lemmas from the support library: 'trackedTotal_map_id', 'transfer_preserves_total', 'four_way_transfer_preserves_total', etc.
DO NOT prove your own versions of these lemmas.

IMPORTANT: Here is how to use transfer_preserves_total correctly with all required arguments:

The lemma signature is:
  transfer_preserves_total (p_accounts : List Account) (p_from_authority p_to_authority : Pubkey) (p_amount : Nat) (p_h_distinct : p_from_authority ≠ p_to_authority)

Example: If you need to prove conservation after transferring 100 tokens from authority A to authority B:
```lean
theorem example (p_accounts : List Account) (p_auth_from p_auth_to : Pubkey)
    (h_distinct : p_auth_from ≠ p_auth_to) :
    let post := p_accounts.map (fun acc =>
      if acc.authority = p_auth_from then { acc with balance := acc.balance - 100 }
      else if acc.authority = p_auth_to then { acc with balance := acc.balance + 100 }
      else acc)
    trackedTotal post = trackedTotal p_accounts := by
  exact transfer_preserves_total p_accounts p_auth_from p_auth_to 100 h_distinct
```

For FOUR-WAY transfers (like escrow exchange with two independent transfers), use four_way_transfer_preserves_total:
```lean
theorem exchange (p_accounts : List Account)
    (p_from1 p_to1 p_from2 p_to2 : Pubkey)
    (p_amount1 p_amount2 : Nat)
    (h_distinct1 : p_from1 ≠ p_to1)
    (h_distinct2 : p_from2 ≠ p_to2)
    (h_cross : p_from1 ≠ p_from2) :
    trackedTotal (p_accounts.map (fun acc =>
      if acc.authority = p_from1 then { acc with balance := acc.balance - p_amount1 }
      else if acc.authority = p_to1 then { acc with balance := acc.balance + p_amount1 }
      else if acc.authority = p_from2 then { acc with balance := acc.balance - p_amount2 }
      else if acc.authority = p_to2 then { acc with balance := acc.balance + p_amount2 }
      else acc)) = trackedTotal p_accounts := by
  exact four_way_transfer_preserves_total p_accounts p_from1 p_to1 p_from2 p_to2
    p_amount1 p_amount2 h_distinct1 h_distinct2 h_cross
```

After applying the axiom, you may need `symm` to flip the equation direction.

Model only the three or four tracked balances touched by this instruction. Import the relevant support modules, write `open Leanstral.Solana`, and use that surface consistently. Use the `trackedTotal` function and conservation lemmas from the support library: `trackedTotal_map_id` for balance-preserving updates, `transfer_preserves_total` for two-account transfers, `four_way_transfer_preserves_total` for escrow-style exchanges with two independent transfers. Do not redefine `trackedTotal` or basic lemmas. Prefer a direct theorem over numeric balances and `trackedTotal`, not a large account-state machine. Do not invent helpers like `transfer`, `transferWithSigner`, `state.accounts`, seed lists, or signer arrays unless you define them in the file. Do not wrap the conservation theorem in an `EscrowState` record update unless the theorem truly depends on a record field update. Prefer a shape like: given pre-balances and nonnegativity/precondition inequalities, define post-balances directly and prove `trackedTotal [pre accounts] = trackedTotal [post accounts]`. Apply the support library lemmas to simplify the proof. In record updates, use Lean syntax `field := value`, never `field = value`. If subtraction over `Nat` makes the goal awkward, state enough preconditions and prove the equality with a small explicit arithmetic argument rather than relying on `omega` or `ring` blindly."#;

const STATE_MACHINE_HINT: &str = r#"Model only the lifecycle flag or closed/open state that matters. Import the relevant support modules, write `open Leanstral.Solana`, and use that surface consistently. Use the `Lifecycle` type and lemmas from the support library: `closes_is_closed`, `closes_was_open`, `closed_irreversible`. Define one small local state structure, typically `EscrowState`, with a `lifecycle : Lifecycle` field. Do not define a custom local `AccountState` when the theorem is really about lifecycle. Prefer a direct theorem shape like `(cancelTransition st).lifecycle = Lifecycle.closed` or `closes st.lifecycle (cancelTransition st).lifecycle`. Apply the support library lemmas to simplify the proof. Do not write theorem statements using placeholders like `some _`; introduce any post-state explicitly if needed."#;

const ARITHMETIC_SAFETY_HINT: &str = r#"Model only the numeric parameters and bounds that matter for this obligation. Import the relevant support modules, write `open Leanstral.Solana`, and use that surface consistently. Avoid unrelated account semantics. Do not write theorem statements using placeholders like `some _`."#;

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

    if modules.iter().any(|m| m == "Leanstral.Solana.Token") {
        lines.extend([
            "-- Token surface".to_string(),
            "TokenAccount := Account".to_string(),
            "Mint : Type".to_string(),
            "Program : Type".to_string(),
            "trackedTotal : List Account -> Nat".to_string(),
            "-- Lemmas:".to_string(),
            "trackedTotal_nil : trackedTotal [] = 0".to_string(),
            "trackedTotal_cons : cons preserves total".to_string(),
            "trackedTotal_append : append distributes over total".to_string(),
            "trackedTotal_map_id : mapping preserving balance preserves total".to_string(),
            "balance_update_preserves_total : zero-delta update preserves total".to_string(),
            "transfer_preserves_total : two-account transfer preserves total".to_string(),
            "four_way_transfer_preserves_total : four-account transfer (two independent pairs) preserves total".to_string(),
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

    lines.join("\n")
}
