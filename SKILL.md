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

### Step 1: Analyze the Solana project and infer candidate properties

Before calling Leanstral, inspect the Solana project and infer what is worth proving. Leanstral is a proof engine, not a project analyzer. The skill should derive candidate properties from the program and tests before it asks for Lean code.

Use the Rust analyzer at `tools/anchor-ir/` when working with Anchor programs.

For Anchor projects, treat the IDL as the first-class structural source of truth and use Rust source as an enrichment layer for semantics that the IDL does not preserve.

```bash
npm run analyze-anchor -- \
  --idl path/to/target/idl/my_program.json \
  --input path/to/programs/my_program/src/lib.rs \
  --tests path/to/tests/my_program.ts \
  --output-dir /tmp/anchor-ir
```

This emits:
- `analysis.json` with instructions, account constraints, PDA seeds, transfer patterns, and test-derived hints
- one prompt template per candidate property

When you want the full pipeline, run:

```bash
npm run verify-solana -- \
  --idl path/to/target/idl/my_program.json \
  --input path/to/programs/my_program/src/lib.rs \
  --tests path/to/tests/my_program.ts \
  --analysis-dir /tmp/anchor-ir \
  --output-dir /tmp/leanstral-proofs \
  --top-k 3 \
  --repair-rounds 1 \
  --validate
```

Use `--analysis-only` to stop after `analysis.json` and prompt generation. This is useful when you want to inspect the inferred properties before spending API calls.
Use `--repair-rounds` with `--validate` when you want the workflow to retry with concrete Lean compiler errors instead of accepting the first non-compiling output.

Evidence precedence:
- `idl`: instruction/account graph, signer flags, writable flags, PDA seed metadata, type layout
- `rust`: CPI behavior, transfer direction, close semantics, arithmetic and custom checks
- `tests`: likely intended invariants and failure cases

Prioritize:
- access control from `Signer`, `has_one`, owner/address checks, and authority use
- conservation from token/system transfers
- state-machine properties from `close =`, terminal flags, and one-shot flows
- arithmetic safety from fixed-width integer parameters and arithmetic-heavy code
- account isolation from owner / seed / address constraints

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
I need a single Lean 4 module that compiles under Lean 4.15 + Mathlib 4.15.

Return Lean code only.
Do not duplicate theorem declarations.
Do not leave theorem bodies empty after `:= by`.
Do not invent helper APIs or namespaces unless you define them in the file.
If a proof is incomplete, use `sorry` inside the proof body.
Prefer a smaller explicit model that compiles over a larger broken one.

## Source Code
<paste the relevant code here>

## Property to Prove
<state the property clearly>

## Context
<any domain-specific context Leanstral needs>

## Output Requirements
1. Define the relevant types and executable state transition functions first
2. Then state the theorem formally
3. Then prove it
4. Use only Lean/Mathlib names that exist in Lean 4.15 / Mathlib 4.15
5. If several properties are hard, prove the easiest sound subset first
```

For Solana-specific work, include context about the account model, CPIs, PDAs, etc. Leanstral was trained on realistic repositories and handles domain modeling well.

Important: prefer one property at a time for nontrivial programs. Asking for 5 large theorems plus a new state model in one pass increases the chance of duplicate declarations, invented APIs, and non-compiling files.

Prefer prompts generated from the analyzer output over ad hoc prose. The analyzer already narrows the proof surface and carries forward concrete evidence from the program/tests.

### Step 3: Call the Leanstral API

Run the script at `scripts/call_leanstral.ts`. It handles:

- Sending the prompt to `labs-leanstral-2603` via the Mistral chat completions API
- Running **pass@4** by default (4 independent completions) for higher proof success rates
- Returning all completions so you can pick the best one

```bash
bun /path/to/skill/scripts/call_leanstral.ts \
  --prompt-file /tmp/leanstral_prompt.txt \
  --output-dir /tmp/leanstral_output \
  --passes 4 \
  --temperature 0.6
```

**Output structure** (Lean 4 project scaffold):
```
output_dir/
├── Best.lean           # The best proof (fewest sorry markers)
├── lakefile.lean       # Lean build configuration
├── lean-toolchain      # Lean version specifier
├── Main.lean           # Entry point
├── README.md           # Verification instructions
├── .gitignore          # Ignores build artifacts
├── metadata.json       # Timing, token usage, and rankings
├── prompt.txt          # The original prompt
└── attempts/           # All completion attempts
    ├── completion_0.lean
    ├── completion_0_raw.txt
    ├── completion_1.lean
    └── ...
```

The output is a Lean 4 project scaffold that can be checked locally. Anyone can verify the proofs by running:
```bash
cd output_dir
lake build   # Build and verify proofs
```

If `lake build` succeeds with no errors, the proof is formally verified.

If you want the script itself to prefer locally-checkable output, run it with `--validate`. In that mode it tries candidate completions with `lake build Best` and prefers the first successful build over a lower `sorry` count.
The validator uses Lake's own cache mechanisms rather than copying dependency trees around: it runs `lake --try-cache build Best`, enables the shared local artifact cache with `LAKE_ARTIFACT_CACHE=true`, and reuses a persistent validation workspace so dependencies are not recloned for every attempt. You can override that workspace with `LEANSTRAL_VALIDATION_WORKSPACE`.

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
5. **Prefer actual builds over heuristics** — a proof with zero `sorry` can still fail to elaborate. If Lean is available, run `lake build Best` or use the script's `--validate` flag.

Present the best completion to the user. If multiple completions succeed, pick the one with the clearest structure and fewest `sorry` markers.

If the user has Lean 4 installed locally, they can verify the generated project by running `lake build`. Offer to help set up a minimal Lean 4 project if needed.

### Step 5: Iterate

Formal proofs rarely come out perfect on the first try. Common iteration patterns:

- **`sorry` filling**: Take the proof with `sorry` markers and ask Leanstral to fill them in specifically. Provide the full context of the proof so far.
- **Specification refinement**: The user realizes the property they stated isn't quite right. Refine the theorem statement and re-prove.
- **Auxiliary lemmas**: Sometimes Leanstral needs helper lemmas broken out separately. If a proof is struggling, try decomposing it.
- **Tactic debugging**: If a specific tactic fails, ask Leanstral to diagnose why and suggest alternatives. It's particularly good at this (see the StackExchange case study — it diagnosed a `def` vs `abbrev` issue in Lean 4.29.0).
- **Property splitting**: If the prompt asks for many theorems, rerun with one theorem at a time against the same model. This is often better than asking for a full verification suite in one completion.

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
- **Timeout**: Leanstral can take 30-90 seconds for complex proofs. The script has a 180-second timeout per completion.
- **Long first build**: The first `mathlib` build commonly takes 15-45 minutes on a laptop. That is normal.
- **Corrupt Mathlib checkout**: If Lake reports that `.lake/packages/mathlib` cannot resolve `HEAD`, remove that directory and rerun the build.
