---
name: leanstral
description: Use Mistral's Leanstral model to generate formal Lean 4 proofs for programs, especially Solana/Rust code. Trigger this skill whenever the user wants to formally verify code, generate Lean 4 proofs, prove properties about algorithms or smart contracts, verify invariants, convert program logic into formal specifications, or anything involving Lean 4 and formal verification. Also trigger when the user mentions "leanstral", "lean proof", "formal proof", "verify my code", "prove correctness", "formal verification", or wants mathematical guarantees about their implementation. Even if the user just says something like "prove this works" or "can you verify this function is correct", use this skill.
---

# Leanstral — Lean 4 Proof Generation via Mistral API

This skill calls Mistral's **Leanstral** model (`labs-leanstral-2603`) to generate formal Lean 4 proofs for Solana/Rust programs. The API is free during Mistral's feedback period.

## When to use this skill

- User wants to formally verify, prove properties about, or generate Lean 4 specifications for code
- User mentions "leanstral", "lean proof", "formal proof", "verify my code", "prove correctness"

## Workflow

### Step 1: Determine what's available

Check for existing artifacts in this priority order:

1. **spec.md exists** (any casing, in repo root, `formal_verification/`, or `docs/`) → Read it. An existing spec is the best input — it captures the author's intent, state model, invariants, and operations in a way no static analysis can match. Extract security goals, state model, and formal properties from the spec. Skip the scoping quiz and go directly to Step 3 (write/update SPEC.md) to map spec properties to verifiable categories, then Step 4.
2. **IDL exists** (`target/idl/<program>.json`) → Run `leanstral spec --idl <path>` to generate a draft SPEC.md with TODO markers, then refine interactively in Step 2.
3. **Neither exists** → The IDL requires compiling the program (`anchor build`), which may need toolchain setup. If the user can build it, ask them to. If not, read the source code directly (Rust, TypeScript, etc.) to understand the program structure and ask broader scoping questions. The agent has LSP support and can navigate the codebase effectively without an IDL.

### Step 2: Build the verification scope interactively

Run a short interactive quiz — one question at a time, each with checkbox options the user can select. Derive the options from the IDL (translate instructions/accounts into functional language). Ask about **functionality and risks**, not implementation details.

**Question 1: "What does this program need to guarantee above all else?"**

Generate options from the IDL's instruction structure. Map to property categories:
- Authorization / access control (derived from signers and `has_one` relations)
- Tokens are never lost / correct routing (derived from token accounts and transfers)
- One-shot safety / no replay (derived from accounts that get closed)
- All of the above

Let the user select multiple. This determines which property categories to include.

**Question 2: "Which scenario worries you most?"**

Generate concrete risk scenarios from the IDL. For example:
- Two-way swap gets accounts mixed up (if instruction has multiple writable token accounts)
- Cancellation returns tokens to wrong account (if cancel has a transfer)
- Someone replays a completed operation (if accounts are closed)
- Amounts overflow silently (if instruction has numeric args)

Let the user select multiple. This determines priority ordering within categories.

**Question 3: "Does the program make any assumptions that aren't enforced on-chain?"**

Options like:
- Token account ownership is correct
- Mint/token types match
- External accounts exist and are initialized
- No assumptions — Anchor handles everything
- Not sure

This determines the trust boundary section of the spec.

Ask questions **one at a time**. Wait for the user's answer before presenting the next question. Don't ask about instructions, signers, or PDA seeds directly — you already have that from the IDL. Translate between the user's functional language and the technical structure yourself.

### Step 3: Write SPEC.md

Based on the conversation, finalize SPEC.md in the project's `formal_verification/` directory. If you ran `leanstral spec` in Step 1, you already have a draft — resolve the `<!-- TODO -->` markers using the user's answers. Otherwise, create SPEC.md from scratch. Use normative language (MUST, MUST NOT, MAY) throughout.

See `example/escrow/formal_verification/SPEC.md` for a complete example.

Structure:

```markdown
# <Program Name> Verification Spec v1.0

<1-2 sentences describing what the program does, in the user's language>

## 0. Security Goals

The program MUST provide the following properties:
1. **<Goal name>**: <normative statement using MUST/MUST NOT/MAY>
2. ...

## 1. State Model

<State struct with field names, types, and comments>
<PDA derivation>
<Lifecycle diagram if applicable>

## 2. Operations

### 2.1 <Operation name>
**Signers**: <who MUST sign>
**Preconditions**: <what MUST be true before>
**Effects**: <numbered steps with normative transfer descriptions>
**Postconditions**: <what MUST be true after>

## 3. Formal Properties

### 3.1 <Category>
**<property_id>**: For all <quantified variables>,
if <transition predicate> then <conclusion>.

## 4. Trust Boundary
<What is axiomatic and why>

## 5. Verification Results
| Property | Status | Proof |
|---|---|---|
| ... | **Verified** / **Open** | namespace.theorem_name |
```

Present SPEC.md to the user and get confirmation before proceeding. This is the most important step — wrong scope means wasted proofs.

### Step 4: Run analysis

```bash
leanstral verify \
  --idl path/to/target/idl/my_program.json \
  --validate \
  --analysis-only
```

Cross-reference `analysis.json` candidates against SPEC.md. Confirm the tool's candidates cover the user's stated properties. Adjust `--top-k` to match the number of properties in the spec.

Optional flags: `--input` (Rust source, passed to the LLM as context) and `--tests` (test files, hint signals for ranking).

### Step 5: Generate proofs

```bash
leanstral verify \
  --idl path/to/target/idl/my_program.json \
  --validate \
  --repair-rounds 1
```

### Step 6: Consolidate and verify

```bash
leanstral consolidate \
  --input-dir /tmp/leanstral-solana-proofs \
  --output-dir path/to/formal_verification
```

```bash
cd path/to/formal_verification
lake build
```

### Step 7: Report results against the spec

Map each generated proof back to the SPEC.md properties. Present results as:

- **Verified**: Theorem compiles, property proven
- **Partial**: Proof has `sorry` markers — sub-goals remain
- **Failed**: No compiling proof generated

For failures, iterate:
- **`sorry` filling**: Ask Leanstral to fill specific `sorry` markers
- **Specification refinement**: Adjust theorem statements and re-prove
- **Property splitting**: One property per prompt works better than many
- **Manual prompt**: Use `leanstral generate --prompt-file <file> --passes 4 --validate`

Update SPEC.md with results (which properties are proven, which remain open).

## Property categories

1. **Access control** — signer checks, authority constraints
2. **CPI correctness** — correct parameters passed to each transfer (axiomatic, pure `rfl`)
3. **State machines** — lifecycle correctness, one-shot safety
4. **Arithmetic safety** — overflow/underflow for fixed-width integers

## Environment

- **`MISTRAL_API_KEY`** — required. Free from [console.mistral.ai](https://console.mistral.ai)
- **`LEANSTRAL_VALIDATION_WORKSPACE`** — optional override for the global Mathlib cache location

## Error handling

- **`MISTRAL_API_KEY` not set**: Direct user to console.mistral.ai
- **Rate limiting (429)**: Built-in exponential backoff handles this
- **Poor output**: Lower temperature to 0.3 or rephrase with more context
- **Long first build**: First Mathlib build takes 15-45 min; subsequent builds reuse the cache
- **Corrupt Mathlib**: Remove `.lake/packages/mathlib` and rerun
