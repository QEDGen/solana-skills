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
npx skills add abishekk92/leanstral-solana-skill
```

### Manual Installation

1. Clone or download this repository
2. Install the entire skill directory, not just `SKILL.md`
3. Ensure `scripts/templates/`, `lean_support/`, and the compiled binary stay adjacent to `SKILL.md`
4. Install a Rust toolchain (required to build the CLI binary)
5. Run `npm install` or `cargo build --release` to build the binary

## Setup

1. Get a free Mistral API key from [console.mistral.ai](https://console.mistral.ai)
2. Export the API key:
   ```bash
   export MISTRAL_API_KEY=your_key_here
   ```

That's it! The Leanstral API endpoint is currently free during Mistral's feedback period.

## How It Works

When you ask your AI coding agent to verify code or generate proofs, the skill:

1. **Analyzes the Solana program** - Extracts instructions, Anchor account constraints, PDA seeds, transfer patterns, and optional test signals
2. **Ranks candidate properties** - Access control, conservation, state-machine safety, arithmetic bounds, and related invariants
3. **Prepares one prompt per property** - Keeps each Lean task small and compilable
4. **Calls the API** - Uses the `leanstral` CLI to generate proofs (pass@N supported)
5. **Evaluates and validates results** - Prefers locally-checkable output when `--validate` is enabled

## Solana Analysis

The CLI includes an analyzer that treats Anchor IDL as the first-class structural input and uses `anchor-syn` source analysis as an enrichment layer.

Example:

```bash
leanstral analyze \
  --idl path/to/target/idl/my_program.json \
  --input example/escrow/programs/escrow/src/lib.rs \
  --tests example/escrow/tests/escrow.ts \
  --output-dir /tmp/anchor-ir-escrow
```

This emits:
- `analysis.json` with extracted instructions, accounts, constraints, test signals, and ranked property candidates
- one prompt template per candidate property

To run the full analyzer-to-Leanstral flow in one command:

```bash
leanstral verify \
  --idl path/to/target/idl/my_program.json \
  --input path/to/programs/my_program/src/lib.rs \
  --tests path/to/tests/my_program.ts \
  --analysis-dir /tmp/anchor-ir-escrow \
  --output-dir /tmp/leanstral-proofs \
  --top-k 3 \
  --repair-rounds 1 \
  --validate
```

Use `--analysis-only` when you want ranked property candidates and prompt files without calling Leanstral yet.
Use `--repair-rounds` with `--validate` to feed Lean build errors back into a bounded retry loop when the first proof artifact does not compile.

Recommended precedence:
- IDL for instructions, args, signer/writable flags, and PDA seed metadata
- Rust source for CPI semantics, transfer effects, close behavior, and custom checks
- tests for property ranking hints

## Prompting Guidance

For best results, ask for one property at a time and constrain the model to return a single compilable Lean module. A reusable template is available at [scripts/templates/PROMPT_TEMPLATE.md](/Users/abishek/code/leanstral-solana-skill/scripts/templates/PROMPT_TEMPLATE.md).

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

## Output Structure

The script generates a Lean 4 project scaffold ready for local verification:

```
output_dir/
├── Best.lean           # The best proof (fewest sorry markers)
├── lakefile.lean       # Lean build configuration
├── lean-toolchain      # Lean version specifier
├── Main.lean           # Entry point
├── README.md           # Verification instructions
├── .gitignore          # Ignores build artifacts
├── metadata.json       # Timing, token usage, and rankings
├── prompt.txt          # The original verification prompt
└── attempts/           # All completion attempts
    ├── completion_0.lean
    ├── completion_0_raw.txt
    ├── completion_1.lean
    ├── completion_1_raw.txt
    └── ...
```

**To verify the proofs:**
```bash
cd output_dir
lake build   # Build and verify - if this succeeds, the proof is valid!
```

For stricter selection, run the generator with `--validate` so it prefers a completion that passes `lake build Best` locally.

Zero `sorry` markers do not guarantee that the file elaborates. Lean compilation is the real check.

Validation now uses Lake's built-in cache path:
- `lake --try-cache build Best` to fetch supported package artifacts
- `LAKE_ARTIFACT_CACHE=true` to enable Lake's shared local artifact cache across workspaces
- a persistent validation workspace, so repeated runs reuse the same `.lake/packages` checkout instead of recloning dependencies for every generated proof

You can override the persistent workspace location with:

```bash
export LEANSTRAL_VALIDATION_WORKSPACE=/path/to/leanstral-validation-workspace
```

## What's Included

- **SKILL.md** - Complete instructions for AI agents
- **leanstral binary** - Single compiled binary that does everything end-to-end
  - Built-in Anchor program analyzer using `anchor-syn`
  - Leanstral API client with pass@N support
  - Automatic retry with exponential backoff
  - Proof validation using Lake builds
  - Compiler-guided repair with bounded retry loops
  - Clean output organization with best proof highlighted
- **scripts/templates/** - Lean project templates and prompt templates
- **lean_support/** - Support library modules for Solana semantics
- **example/escrow/** - Solana escrow program plus generated Lean proof artifacts
  - Full Anchor 0.32.1 implementation with passing tests
  - Example verification prompt and proof workflow
  - Generated Lean output that may require repair unless revalidated

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

- Rust toolchain (recommended - most Solana developers already have this)
  - Install from https://rustup.rs/ if needed
  - If Rust is not available, the pre-built binary may work on macOS ARM64
- `MISTRAL_API_KEY` environment variable
- Internet connection for API calls
- Lean 4 + Lake if you want to validate generated proofs locally

## Notes on Lean Builds

- The first `mathlib` build is expensive. On a typical laptop, expect roughly 15 to 45 minutes.
- Later builds are much faster because Lake reuses compiled artifacts.
- The validator reuses a persistent Lean workspace plus Lake's remote/local caches, so repeated proof checks should not reclone or recompile Mathlib from scratch once the workspace is warm.
- If `lake build` fails with a corrupt `mathlib` checkout such as `could not resolve 'HEAD' to a commit`, remove `.lake/packages/mathlib` and rerun the build.

## License

MIT

## Contributing

Issues and pull requests welcome! This skill is designed to help developers bring formal verification into their everyday workflow.

## Learn More

- [Mistral Leanstral Documentation](https://docs.mistral.ai/capabilities/leanstral/)
- [Lean 4 Documentation](https://lean-lang.org/)
- [Agent Skills Specification](https://agentskills.io)
