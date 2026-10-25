# Crucible integration hardening

The integration must distinguish a useful counterexample from infrastructure failure, incomplete exploration, and a heuristic alert. The user authorized planning and GPT-6.1 Sol implementation agents after the integration review.

## Approach

Harden the existing generated harness and runner, retaining the supported runtime envelope. A wholesale fuzz-engine replacement would discard working replay machinery; implementing arbitrary account synthesis would require application semantics we cannot infer. Instead, make unsupported shapes explicit and block semantic coverage claims where state is not synchronized. Preserve hand-written harness changes and detect changed generation inputs rather than overwriting them.

## Required behavior

- An abnormal fuzz process exit without confirmed crash evidence, malformed crash metadata, or a timeout must not become a passed engine run.
- Crash replay remains bounded; non-reproducing inputs do not become confirmed findings. Existing crash files must not establish success for a failed new run.
- Generated harnesses expose successful/failed action execution evidence where feasible. Unknown coverage stays unknown; emitting actions or assertions is not executing them.
- Unfilled account wiring, unsupported PDA derivations, fixed/default-only complex inputs, and unsynchronized state assertions are explicit limitations. Domain execution must not silently check stale shadow state. A same-typed accepted-argument mirror only establishes an accepted-input property, not an actual account-state property.
- Protocol guards remain useful leads but cannot label legitimate withdrawals, writes into reallocated data, or arbitrary first-eight-byte changes as confirmed vulnerabilities. Retain genuinely useful detection with explicit heuristic classification and reduce unsound assumptions where feasible.
- Reusing a generated harness must validate generation-input freshness, including spec, bindings, IDL/mode where applicable. Preserve manual source edits. Give actionable remediation for legacy or stale harnesses.
- Pin compatible Crucible dependencies and test real generated artifacts in a dedicated CI lane; fail if an explicitly requested live test cannot run.
- Native/assembly support, arbitrary application account construction, and full formal correctness remain outside this change. Report those limits accurately.

## Verification

Regression tests cover failures, malformed metadata, stale inputs, manually edited harnesses, unsupported state/input shapes, valid protocol operations, and meaningful live counterexamples. Run the focused suites and a live protocol/domain fixture if available; document any external blocker exactly. No commits, pushes, PRs, or upstream edits are authorized.
