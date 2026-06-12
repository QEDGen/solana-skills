---
description: Lean proof gotchas — auto-loaded when editing Lean files or the Lean codegens
paths:
  - "**/*.lean"
  - "crates/qedgen/src/codegen/lean_gen_mir.rs"
  - "crates/qedgen/src/codegen/asm2lean.rs"
---

# Lean proof gotchas

Full patterns: [references/proof-patterns.md](../../references/proof-patterns.md). For sBPF: [references/sbpf.md](../../references/sbpf.md). These are the non-obvious traps that cause silent failures or runaway build times:

- **Unfold named predicates in BOTH hypothesis and goal.** Conservation/predicate proofs: `unfold pred at h ⊢` then `omega`. Unfolding only one side leaves `omega` stuck.
- **`unfold` before `split_ifs`, never `simp`.** `simp [transition] at h` eliminates the if-structure and `split_ifs` then errors. Use `unfold transition at h; split_ifs at h with h_eq`.
- **sBPF offset constants MUST be `Int`, not `Nat`.** `effectiveAddr` takes `(off : Int)`; a `Nat` offset inserts a coercion `simp` can't process → timeout (seconds → hours).
- **sBPF: named constants in `prog` must match the hypothesis names**, and `prog` needs `@[simp]`. A mismatch forces `simp` to unfold the constant at every subterm at every step. `qedgen asm2lean` emits offsets/constants/`@[simp]` correctly — prefer it over hand-transcription.
- **`simp` (not `simp only`) to normalize `wrapAdd`/`toU64` hypotheses** for address disjointness — `simp only` misses modular identities like `(a % m + b) % m = (a + b) % m`.
- When a goal is genuinely hard, emit `sorry` with a comment documenting the obligation — never close it with tactics that might spuriously succeed.

When `lake build` fails, read the error directly; the common-error → fix table is in [references/proof-patterns.md](../../references/proof-patterns.md).
