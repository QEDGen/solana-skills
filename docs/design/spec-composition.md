# Spec Composition — Design Note (v2.5 target)

Status: **design** (§1 shipped on `feat/spec-composition`; §2 + §3 pending review)
Author: agent-driven exploration, 2026-04-18
Scope: how QEDGen's `.qedspec` grows from a single-file DSL into a modular,
composable specification system without losing its declarative nature.

This is not a release PRD. It is the architectural sketch that follows from
the three questions asked for the next release: **modularity**, **CPI**, and
**cross-program composition**.

---

## §1 — Modularity (shipped)

**Shape:** convention-based multi-file. A `.qedspec` "directory spec" contains
N fragment files, each starting with `spec <Name>`. The loader (`check.rs::
parse_spec_file`) detects a directory vs file and merges fragments in
sorted-path order.

**Why convention over configuration:**
- Zero new grammar. Every fragment is an independently-parseable `.qedspec`.
- Matches how Haskell / Lean / Rust handle multi-file modules: every file
  restates its owning module.
- Author intent (ordering) is expressible via filename prefixes
  (`10_initialize.qedspec`, `20_exchange.qedspec`).

**Semantics:**
- Every fragment must declare the same `spec Name`.
- Top items (records, handlers, properties, invariants, covers, pdas, events,
  errors) are concatenated. Duplicate-name detection happens in the existing
  adapter.
- `spec_hash_for_handler` reads a "virtual concatenated source" (`check::
  read_spec_source`) so `#[qed(verified, spec_hash=...)]` attributes agree
  byte-for-byte between single-file and multi-file forms.
- Fingerprinting already works: it operates on the merged `ParsedSpec`, which
  is deterministic.

**What users gain:**
- Stop pasting growing specs into one 2000-line file.
- `handlers/<name>.qedspec` per handler gives clean diffs and natural reviews.
- Shared fragments (e.g. `common/accounts.qedspec`) are trivial to copy.

**What we deliberately didn't add:**
- No `include "path"` directive. Directory membership is the include.
- No `module` or `namespace` keywords. Modularity within a program is flat.
- Cross-program sharing is a **different** problem and belongs to §3.

---

## §2 — CPI as declared effect (the `declare_program!` analog)

**The mistake to avoid:** copying anchor's `declare_program!` literally would
produce serialization stubs. QEDGen's DSL should produce **effect contracts**
instead — declarations the backends compile into whatever shape each one
needs (CPI builder for Rust, axiomatized handler for Lean, mock for
proptest).

### Proposed surface (v2.5)

**Interface declaration — axiomatizes a callee without its source:**

```qedspec
interface Token {
  handler transfer (amount : U64) {
    accounts {
      from      : writable, type token
      to        : writable, type token
      authority : signer
    }
    requires amount > 0
    ensures  from.balance = old(from.balance) - amount
    ensures  to.balance   = old(to.balance) + amount
  }
}
```

- Uses existing declarative keywords. No new expression forms. No bodies.
- An `interface` is a *contract*: preconditions, postconditions, account roles.
- Ships as a library (`qedgen/interfaces/spl_token.qedspec`) so every program
  gets SPL Token for free.

**Call site — a terminal instruction inside a handler body:**

```qedspec
handler exchange : State.Open -> State.Closed {
  auth taker
  accounts { ... }

  call Token.transfer(
    from      = taker_ta,
    to        = initializer_ta,
    amount    = taker_amount,
    authority = taker,
  )

  call Token.transfer(
    from      = escrow_ta,
    to        = taker_ta,
    amount    = initializer_amount,
    authority = escrow,
  )

  emits EscrowExchanged
}
```

- `call` is not an expression. It is a statement-level clause, like `transfers`
  and `emits`. This is deliberate — keeping the DSL from drifting into a PL.
- Exactly replaces today's `transfers { ... }` primitive. Under the hood,
  `transfers` becomes sugar for `call Token.transfer(...)`.

### Codegen mapping

| Backend    | What `call Token.transfer(...)` produces                          |
|------------|-------------------------------------------------------------------|
| Rust       | `token::transfer(CpiContext::new_with_signer(...), amount)`       |
| Lean       | callee `ensures` become hypotheses; callee `effect` rewrites state |
| proptest   | in-memory mock of callee preserving declared `ensures`            |
| Kani       | stub function with `kani::assume(ensures)` pre-state              |

### Scope boundaries

- **In:** `Token` (transfer/mint/burn/init_account/close_account) and System
  Program (create_account, transfer, assign, allocate) shipped as library
  interfaces.
- **In:** user-declared `interface Foo { ... }` in the user's own spec for
  ad-hoc callees.
- **Out (deferred to §3):** `import "path/to/other.qedspec"` to automatically
  derive an interface from a real, proven spec.

### Guardrails against DSL drift

`interface` bodies must not introduce:
- New expression forms (still only guard-expr / ensures-expr).
- Recursion or higher-order constructs.
- Anything that looks like control flow inside the interface block.

If any of those come up, the answer is "declare it as a separate handler in
your own spec" — not "extend interface."

---

## §3 — Cross-program spec composition (import + qedlock)

### The three stances a caller can take

| Stance                   | What it means                                       | Strength |
|--------------------------|-----------------------------------------------------|----------|
| 1. **Trust ensures**     | Callee's declared post-conditions = hypotheses     | Weak (same as today's axioms, but versioned) |
| 2. **Compose proofs**    | Caller's Lean imports callee's proven Lean module  | Strong (end-to-end proven) |
| 3. **Verify against impl** | Dynamic: run caller tests against real callee     | Orthogonal |

v2.5 ships **stance 1 only**. Stance 2 requires Lean-module-layout decisions
that depend on §1 settling. Stance 3 is a test-harness concern, not a spec
concern.

### Proposed syntax

```qedspec
import Token from "qedgen/interfaces/spl_token"    // library path
import Jupiter from "./specs/jupiter_v6"           // local path
```

- Resolution order: builtin library → project-relative → env-configured.
- Imported names expose their `interface` block (or the full spec, reduced to
  its public interface).
- The loader diffs imported specs against a `qedlock` file on disk.

### qedlock format

```toml
# qed.lock — pinned spec dependencies (commit to git)
[[import]]
name = "Token"
path = "qedgen/interfaces/spl_token"
version = "spl-token@6.0.0"
spec_hash = "a1b2c3d4e5f67890"   # sha256 of the resolved spec text

[[import]]
name = "Jupiter"
path = "./specs/jupiter_v6"
version = "jupiter-v6@c8d9e0f1"
spec_hash = "9fedcba098765432"
```

- Analogous to `Cargo.lock` / `package-lock.json` — deterministic closure.
- Fingerprint of a multi-file spec directory becomes
  `hash(ParsedSpec) ⊕ hash(qedlock)`, so a callee spec edit invalidates the
  caller's attestation.
- `qedgen lock` updates the file; `qedgen check` diagnoses drift.

### `#[qed(verified)]` transitive hash

Today: `spec_hash = "X"` binds the Rust handler to a single spec body.

Tomorrow: `spec_hash = "X"` still binds to the caller spec; `qedlock_hash =
"Y"` binds to the transitive closure. A callee spec change bumps Y but not X,
triggering a clear "callee drift" diagnostic instead of a silent
re-verification gap.

### Non-goals (explicit)

- **Not** shipping Jupiter / Raydium / Drift specs. Community contribution or
  official-team ownership; QEDGen only ships what it itself verifies.
- **Not** versioning specs by program ID alone. Spec version ≠ program
  version; a pinned `spec_hash` is authoritative.
- **Not** inferring interfaces from IDL. `qedgen spec --idl` already does
  scaffold generation; composition assumes the human-authored interface is
  the source of truth.

### The strategic play

Every program built on QEDGen today axiomatizes SPL Token by hand. Once SPL
Token ships a qedspec (shipped as a library interface, then progressively
strengthened toward a full proven spec), every consumer becomes strictly
stronger without any user action. This is the compounding-adoption lever.

---

## Suggested release order

1. **v2.5**: ship §1 (multi-file) + §2 (interface/call) together. §3 stays as
   this design doc plus qedlock format declaration.
2. **v2.6**: ship §3 stance 1 (trust ensures via `import`) + transitive
   `#[qed(verified)]` + `qedgen lock`.
3. **v2.7**: stance 2 (compose proofs across spec boundaries in Lean).

Anything past v2.7 (interface registry, public spec index, etc.) is
ecosystem-layer, not core QEDGen.
