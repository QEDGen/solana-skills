---
name: leanstral
description: Use Mistral's Leanstral model to generate formal Lean 4 proofs for programs, especially Solana/Rust code. Trigger this skill whenever the user wants to formally verify code, generate Lean 4 proofs, prove properties about algorithms or smart contracts, verify invariants, convert program logic into formal specifications, or anything involving Lean 4 and formal verification. Also trigger when the user mentions "leanstral", "lean proof", "formal proof", "verify my code", "prove correctness", "formal verification", or wants mathematical guarantees about their implementation. Even if the user just says something like "prove this works" or "can you verify this function is correct", use this skill.
---

# Leanstral — Lean 4 Proof Generation via Mistral API

This skill calls Mistral's **Leanstral** model (`labs-leanstral-2603`) to generate formal Lean 4 proofs. Leanstral is a 119B-parameter sparse model (6.5B active) specifically trained for proof engineering in realistic Lean 4 repositories. It excels at generating proofs, defining mathematical structures, diagnosing Lean compilation issues, and reasoning about program correctness.

The API endpoint is currently free during Mistral's feedback period.

## When to use this skill

- User wants to **formally verify** a function, algorithm, or smart contract
- User wants to **prove properties** about Solana programs, Rust logic, or general algorithms
- User wants to **generate Lean 4 code** that models and proves correctness of an implementation
- User wants to **convert** proof assistant code (Rocq/Coq) to Lean 4
- User wants to **debug** existing Lean 4 proofs or definitions
- User says anything like "prove this", "verify this is correct", "formal proof", "lean proof"

## Workflow

### Step 1: Understand what the user wants to prove

Before calling Leanstral, figure out what the user actually needs verified. This is the most important step — Leanstral is a proof engine, not a mind reader. You need to translate the user's intent into a clear formal specification.

Common patterns for Solana/Rust programs:

- **Arithmetic safety**: overflow/underflow cannot occur, token balances are conserved
- **State machine correctness**: valid state transitions, no invalid states reachable
- **Access control**: only authorized signers can mutate specific accounts
- **Invariant preservation**: some property holds before and after every instruction
- **Algorithmic correctness**: a function computes what it claims to compute

Ask clarifying questions if the user's intent is ambiguous. A bad specification leads to a correct but useless proof.

### Step 2: Prepare the prompt for Leanstral

Construct a clear prompt that includes:

1. **The code to verify** (Rust/Solana source, or a pseudocode description)
2. **The property to prove** (stated in plain English, which Leanstral will formalize)
3. **Any relevant context** (e.g., Solana account model, token program semantics)

Structure the prompt like this:

```
I need to formally verify the following program in Lean 4.

## Source Code
<paste the relevant code here>

## Property to Prove
<state the property clearly>

## Context
<any domain-specific context Leanstral needs>

Please:
1. Define the relevant types and functions in Lean 4 that model this program
2. State the theorem formally
3. Prove the theorem
4. Explain the proof strategy
```

For Solana-specific work, include context about the account model, CPIs, PDAs, etc. Leanstral was trained on realistic repositories and handles domain modeling well.

### Step 3: Call the Leanstral API

Run the script at `scripts/call_leanstral.py`. It handles:

- Sending the prompt to `labs-leanstral-2603` via the Mistral chat completions API
- Running **pass@4** by default (4 independent completions) for higher proof success rates
- Returning all completions so you can pick the best one

```bash
python3 /path/to/skill/scripts/call_leanstral.py \
  --prompt-file /tmp/leanstral_prompt.txt \
  --output-dir /tmp/leanstral_output \
  --passes 4 \
  --temperature 0.6
```

The script requires `MISTRAL_API_KEY` as an environment variable. If it's not set, tell the user to:
1. Go to https://console.mistral.ai
2. Create an API key (Leanstral is free/near-free during the labs period)
3. Run: `export MISTRAL_API_KEY=your_key_here`

**API Details** (in case you need to call it directly via curl):
- Endpoint: `https://api.mistral.ai/v1/chat/completions`
- Model: `labs-leanstral-2603`
- Auth: `Authorization: Bearer $MISTRAL_API_KEY`
- Context window: 256k tokens
- The `n` parameter controls number of completions per request

### Step 4: Evaluate the results

Leanstral returns Lean 4 code. For each completion:

1. **Read the proof** — does it define types that faithfully model the original program?
2. **Check the theorem statement** — does it actually capture the property the user wanted?
3. **Review the proof strategy** — is the proof approach sound? (induction, case analysis, simplification, etc.)
4. **Look for `sorry`** — any `sorry` in the proof means that part is unfinished. This is a known pattern with proof models; the structure may be correct but some lemmas need filling in.

Present the best completion to the user. If multiple completions succeed, pick the one with the clearest structure and fewest `sorry` markers.

If the user has Lean 4 installed locally, they can verify the proof by saving it as a `.lean` file and running `lake build`. Offer to help set up a minimal Lean 4 project if needed.

### Step 5: Iterate

Formal proofs rarely come out perfect on the first try. Common iteration patterns:

- **`sorry` filling**: Take the proof with `sorry` markers and ask Leanstral to fill them in specifically. Provide the full context of the proof so far.
- **Specification refinement**: The user realizes the property they stated isn't quite right. Refine the theorem statement and re-prove.
- **Auxiliary lemmas**: Sometimes Leanstral needs helper lemmas broken out separately. If a proof is struggling, try decomposing it.
- **Tactic debugging**: If a specific tactic fails, ask Leanstral to diagnose why and suggest alternatives. It's particularly good at this (see the StackExchange case study — it diagnosed a `def` vs `abbrev` issue in Lean 4.29.0).

## Tips for Solana/Rust verification

Formal verification of Solana programs typically involves modeling the program at a higher level of abstraction rather than verifying raw Rust bytecode. Here's the practical approach:

- **Model the state**: Define Lean 4 structures that mirror your Solana account layouts (token balances, authority fields, bump seeds, etc.)
- **Model the instructions**: Each Solana instruction becomes a Lean function that transforms state
- **State the invariants**: "Total supply is conserved", "Only the authority can withdraw", etc.
- **Prove preservation**: Show that each instruction preserves the invariants

You don't need to model every byte of the Rust code. Focus on the **semantic properties** that matter for security and correctness.

Example: For a token vault program, you might prove:
- `deposit` increases vault balance by exactly the deposited amount
- `withdraw` decreases vault balance and increases user balance by the same amount
- `withdraw` reverts if caller is not the authority
- Total token supply across all accounts is constant

## Error handling

- **`MISTRAL_API_KEY` not set**: Instruct user to get a key from console.mistral.ai
- **Rate limiting**: The labs endpoint may have rate limits. If you get 429 errors, wait and retry. The script has built-in exponential backoff.
- **Empty or nonsensical output**: Try lowering temperature to 0.3, or rephrasing the prompt with more explicit Lean 4 context.
- **Timeout**: Leanstral can take 30-90 seconds for complex proofs. The script has a 120-second timeout per completion.
