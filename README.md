# Leanstral Solana Skill

An agent skill for generating formal Lean 4 proofs using Mistral's **Leanstral** model (`labs-leanstral-2603`). This skill enables AI coding agents to formally verify code, prove properties about algorithms and smart contracts, and convert implementations into mathematical specifications.

## What is Leanstral?

Leanstral is a 119B-parameter sparse model (6.5B active) specifically trained for proof engineering in Lean 4. It excels at:

- Generating formal proofs for programs and algorithms
- Defining mathematical structures and properties
- Diagnosing Lean compilation issues
- Reasoning about program correctness and security properties

## Use Cases

- **Formally verify** Solana programs, Rust code, and smart contracts
- **Prove properties** like arithmetic safety, state machine correctness, and invariant preservation
- **Generate Lean 4 code** that models and proves implementation correctness
- **Convert** proof assistant code (Rocq/Coq) to Lean 4
- **Debug** existing Lean 4 proofs or definitions

## Installation

### Using npx skills (recommended)

```bash
npx skills add leanstral-solana-skill
```

### Manual Installation

1. Clone or download this repository
2. Copy `SKILL.md` to your agent's skills directory
3. Install Python 3.6+ (required for the API script)

## Setup

1. Get a free Mistral API key from [console.mistral.ai](https://console.mistral.ai)
2. Export the API key:
   ```bash
   export MISTRAL_API_KEY=your_key_here
   ```

That's it! The Leanstral API endpoint is currently free during Mistral's feedback period.

## How It Works

When you ask your AI coding agent to verify code or generate proofs, the skill:

1. **Understands your verification goal** - What property needs to be proven?
2. **Prepares a prompt** - Structures your code and specification for Leanstral
3. **Calls the API** - Uses `scripts/call_leanstral.py` to generate proofs (pass@4 by default)
4. **Evaluates results** - Presents the best proof with explanations
5. **Iterates if needed** - Fills in `sorry` markers, refines specifications, or adds helper lemmas

## Examples

### Verify a Solana Token Vault

```
Prove that my token vault's withdraw function:
1. Only allows the authority to withdraw
2. Decreases vault balance by the exact withdrawal amount
3. Preserves total token supply across all accounts
```

### Verify Arithmetic Safety

```
Prove that this Rust function cannot overflow for any valid input.
```

### Generate Formal Specifications

```
Convert this Solana program into a Lean 4 model and prove that all state transitions are valid.
```

## What's Included

- **SKILL.md** - Complete instructions for AI agents
- **scripts/call_leanstral.py** - Python script for calling the Leanstral API
  - Supports pass@N (multiple completions for higher success rates)
  - Automatic retry with exponential backoff
  - Extracts and ranks proofs by completeness

## Supported Agents

This skill works with any agent that implements the [Agent Skills spec](https://agentskills.io), including:

- Claude Code
- GitHub Copilot
- Cursor
- Windsurf
- And 38+ more

## API Details

- **Endpoint**: `https://api.mistral.ai/v1/chat/completions`
- **Model**: `labs-leanstral-2603`
- **Context Window**: 256k tokens
- **Cost**: Free during labs period

## Requirements

- Python 3.6 or higher (for the API script)
- `MISTRAL_API_KEY` environment variable
- Internet connection for API calls

## License

MIT

## Contributing

Issues and pull requests welcome! This skill is designed to help developers bring formal verification into their everyday workflow.

## Learn More

- [Mistral Leanstral Documentation](https://docs.mistral.ai/capabilities/leanstral/)
- [Lean 4 Documentation](https://lean-lang.org/)
- [Agent Skills Specification](https://agentskills.io)
