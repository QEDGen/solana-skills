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
needs.

But effects require semantic understanding that an IDL alone cannot give you.
The design has to be honest about this: you pay for what you know, and you
get proof strength proportional to what you declare. That forces a **tiered**
model rather than one uniform `interface` concept.

### The three tiers

| Tier | Source                                  | Declares                       | Lean verdict                                        |
|------|-----------------------------------------|--------------------------------|-----------------------------------------------------|
| 0    | IDL (`qedgen interface --idl`)          | shape only                     | opaque axiom (no post-state info)                   |
| 1    | hand-written `interface`                | requires / ensures / effect    | hypotheses + state rewrites                         |
| 2    | imported callee `.qedspec`              | the callee's real handlers     | hypotheses now (stance 1); theorems later (stance 2)|

The **same** `call X.h(...)` surface covers all three tiers. Backends produce
weaker or stronger artifacts depending on what's available — partial
verification is automatic, and upgrading a Tier-0 callee to Tier 1 or 2 is
purely additive.

### Tier 0 — shape from IDL

```bash
qedgen interface --idl target/idl/jupiter.json --out interfaces/jupiter.qedspec
```

produces

```qedspec
interface Jupiter {
  program_id "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4"

  handler swap (amount_in : U64, min_amount_out : U64) {
    discriminant 0xE445A52E51CB9A1D
    accounts {
      input_mint      : readonly
      output_mint     : readonly
      user_input_ta   : writable, type token
      user_output_ta  : writable, type token
      user            : signer
      token_program   : program
    }
  }
}
```

No `requires`, no `ensures`, no `effect` — and that's the honest answer. An
IDL carries shape, not semantics. The caller still gets:

- **Rust**: real CPI builder with correct discriminator, account order, signer
  roles. Already a big win — today we hardcode `Token.transfer` and have
  nothing for arbitrary callees.
- **Lean**: `call Jupiter.swap(...)` compiles to an opaque transition — the
  call site commits to the accounts/args, but no post-conditions follow from
  it. Same strength as today's `True := trivial` stubs, but structured.
- **Kani / proptest**: stubbed call, no post-state assumptions.

### Tier 1 — effects hand-authored

When you know what the callee does to your state (and can't get a qedspec for
it), you add the effects yourself. Upgrade is additive — start from the
Tier-0 file and fill in clauses as you learn what you need:

```qedspec
interface Jupiter {
  program_id "JUP..."

  handler swap (amount_in : U64, min_amount_out : U64) {
    discriminant 0xE445A52E51CB9A1D
    accounts { ... }

    requires amount_in > 0
    ensures  user_input_ta.balance  = old(user_input_ta.balance)  - amount_in
    ensures  user_output_ta.balance >= old(user_output_ta.balance) + min_amount_out
  }
}
```

Lean now gets real hypotheses at the `call` site. You only write what you
rely on in the caller's proof — partial contracts are fine.

For **SPL Token** and **System Program**, QEDGen ships Tier-1 interfaces in
`qedgen/interfaces/*.qedspec` — one-time cost absorbed upstream. Today's
hardcoded `transfers { from X to Y amount N }` primitive becomes sugar for
`call Token.transfer(from=X, to=Y, amount=N)` against the library interface.

### Tier 2 — callee has its own qedspec

No `interface` keyword at all. The callee's qedspec **is** the interface —
any handler is automatically part of its public surface:

```qedspec
// caller spec
spec Escrow
import MyAmm from "../my_amm"

handler exchange : State.Open -> State.Closed {
  call MyAmm.swap(pool = amm_pool, amount_in = taker_amount, ...)
  emits EscrowExchanged
}
```

- At v2.6 (stance 1), the caller axiomatizes `MyAmm.swap`'s declared
  `ensures`. Same strength as Tier 1, but zero duplication — you don't
  re-declare what the callee already specified.
- At v2.7 (stance 2), the caller's Lean imports `MyAmm.formal_verification/
  Spec.lean` and the `ensures` become actual theorems. End-to-end proven.

This is the compounding-adoption lever. No dual maintenance; no re-declaring
an interface that already exists; every callee that adopts QEDGen strengthens
every caller without user action.

### Call-site syntax (uniform across all tiers)

```qedspec
handler exchange : State.Open -> State.Closed {
  call Token.transfer(
    from      = taker_ta,
    to        = initializer_ta,
    amount    = taker_amount,
    authority = taker,
  )

  emits EscrowExchanged
}
```

`call` is statement-level, like `transfers` and `emits`. Not an expression.
Not nestable. This is deliberate — it keeps the DSL from drifting into a PL.

### Codegen mapping per tier

| Backend    | Tier 0 (shape-only)                              | Tier 1 / Tier 2 (effectful)                         |
|------------|--------------------------------------------------|-----------------------------------------------------|
| Rust       | CPI builder: discriminator + accounts + signers  | same                                                |
| Lean       | opaque axiom (no post-state info)                | callee `ensures` → hypotheses; `effect` → rewrite   |
| proptest   | mock returns `Ok(())`, no state change           | mock enforces declared `ensures` on post-state      |
| Kani       | stub with no assumptions                         | stub with `kani::assume(ensures)`                   |

### Linting the tiers

A `call` to a Tier-0 interface is a **visible gap**, not a silent one.
`qedgen check` emits an info:

> `[shape_only_cpi]` call to `Jupiter.swap` — interface declares shape only,
> no post-state assumptions. Upgrade to Tier 1 by declaring `ensures`, or
> import a qedspec for full verification.

Users see exactly what they're leaving on the table. This is the guardrail
against mistaking "my Rust compiles" for "my program is verified."

### Scope boundaries

- **In (v2.5):**
  - `qedgen interface --idl <path>` emits Tier-0 interfaces.
  - SPL Token / System Program ship as Tier-1 library interfaces.
  - User-declared `interface Foo { ... }` at any tier.
  - `call X.h(...)` at all tiers with uniform surface.
  - `[shape_only_cpi]` lint.
- **Out (deferred to v2.6+):** `import Foo from "./other.qedspec"` (Tier 2).
  The `interface` keyword stays available; only the `import` resolution path
  is deferred.

### Guardrails against DSL drift

`interface` bodies must not introduce:
- New expression forms (still only guard-expr / ensures-expr).
- Recursion or higher-order constructs.
- Anything that looks like control flow inside the interface block.

If any of those come up, the answer is "declare it as a separate handler in
your own spec" — not "extend interface."

### Library interface pinning (upstream version binding)

Tier-1 interfaces QEDGen ships for SPL Token, System Program, etc. are
trusted transitively by every consumer. A silent runtime or program upgrade
that changes upstream behavior would invalidate every consumer's proof with
no diagnostic. Every library interface therefore carries an **`upstream`
block** declaring the exact version it was verified against:

```qedspec
interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"

  upstream {
    package      "spl-token"
    version      "4.0.3"
    source       "https://github.com/solana-program/token/tree/v4.0.3"
    binary_hash  "sha256:a1b2c3…"      # of the deployed .so on mainnet
    idl_hash     "sha256:d4e5f6…"      # of the Anchor IDL we codegen'd from
    verified_with ["proptest", "kani"]  # what QEDGen ran; "lean" only when proven, not axiomatized
    verified_at  "2026-04-18"
  }

  handler transfer (amount : U64) { ... }
  ...
}
```

Semantics:

- **`binary_hash` is authoritative.** It's the SHA-256 of the deployed
  program's `.so` bytes (what `solana program dump <program_id>` returns).
  Version strings and commit hashes are informational; the binary hash is
  what a caller's attestation chains to.
- **`verified_with`** is honest. `"proptest"` means we ran property tests
  against the real program. `"kani"` means we BMC-checked the CPI envelope.
  `"lean"` only appears when we actually have a proof of the program's Rust
  source — which we generally do **not** have for external programs. For
  SPL Token today, the Lean side remains axiomatized and `verified_with`
  omits `"lean"`. No overclaiming.
- **Not a proof of correctness.** `upstream` records what QEDGen checked, not
  that the upstream program is provably correct. The interface's `ensures`
  are still declarative contracts; `verified_with` tells consumers *which
  backend checked that the real program behaves that way* on the harness
  inputs we ran.

### Propagation into `qed.lock`

When a consumer imports a library interface, the `upstream.binary_hash` flows
into their lockfile (see §3). That's the transitive pin: the consumer's
`#[qed(verified, qedlock_hash="...")]` covers not just *what* QEDGen
declared about SPL Token, but *which bytes on chain* we declared it about.

### Drift detection against mainnet

`qedgen verify --check-upstream` (v2.6+) fetches the on-chain program for
every imported library interface and compares its SHA-256 to the pinned
`binary_hash`. A mismatch surfaces as:

> `[upstream_drift]` `Token` — pinned binary `sha256:a1b2c3…` (verified
> 2026-04-18) does not match mainnet deploy `sha256:9d8c7b…`. The program
> may have been upgraded. Re-verify with the newer version before trusting
> this interface.

This is optional per run (network dependency), not a build-time check. CI
pipelines for production programs should turn it on; local development runs
can skip it.

### Upgrade flow

When a Solana upstream program bumps (rare but not never):

1. QEDGen re-runs its harnesses against the new version.
2. Re-hashes the deployed `.so` and any changed `ensures`.
3. Publishes a new release of `qedgen/interfaces/<program>.qedspec`.
4. Consumers run `qedgen lock --upgrade Token` to accept the new pin.
5. `qedgen check` flags any caller whose `ensures` relied on behavior that
   changed between versions.

Same shape as a `Cargo.lock` upgrade — explicit, reviewable, diffable.

---

## §3 — Cross-program spec composition (import + qedlock)

### The three stances a caller can take

| Stance                   | What it means                                       | Strength |
|--------------------------|-----------------------------------------------------|----------|
| 1. **Trust ensures**     | Callee's declared post-conditions = hypotheses     | Weak (same as today's axioms, but versioned) |
| 2. **Compose proofs**    | Caller's Lean imports callee's proven Lean module  | Strong (end-to-end proven) |
| 3. **Verify against impl** | Dynamic: run caller tests against real callee     | Orthogonal |

**Stance 1** lands in v2.6 (importing a callee qedspec binds its `ensures`
as caller hypotheses). **Stance 2** lands in v2.7 (caller's Lean imports the
callee's proven module; `ensures` become theorems). **Stance 3** is a
test-harness concern, orthogonal to the spec layer.

### Proposed syntax

```qedspec
import Token from "qedgen/interfaces/spl_token"    // library path (Tier 1)
import Jupiter from "./specs/jupiter_v6"           // local path (Tier 2)
```

- Resolution order: builtin library → project-relative → env-configured.
- If the imported spec has an explicit `interface` block, that's the public
  surface. Otherwise every handler in the imported spec is automatically
  part of its interface (a qedspec is its own contract — no re-declaration).
- The loader diffs imported specs against a `qed.lock` file on disk.

### qedlock format

```toml
# qed.lock — pinned spec dependencies (commit to git)

# Library interface: pins both our spec text AND the upstream binary we
# verified against. See §2 "Library interface pinning."
[[import]]
name           = "Token"
path           = "qedgen/interfaces/spl_token"
spec_hash      = "a1b2c3d4e5f67890"   # our interface declaration
upstream_package = "spl-token"
upstream_version = "4.0.3"
upstream_binary_hash = "sha256:9f8e7d6c5b4a3210…"   # deployed .so
verified_with  = ["proptest", "kani"]
verified_at    = "2026-04-18"

# Peer qedspec: pins the callee's spec hash only — no upstream block, because
# the callee's own qed.lock covers its upstream pins transitively.
[[import]]
name      = "Jupiter"
path      = "./specs/jupiter_v6"
spec_hash = "9fedcba098765432"
```

- Analogous to `Cargo.lock` / `package-lock.json` — deterministic closure.
- Fingerprint of a multi-file spec directory becomes
  `hash(ParsedSpec) ⊕ hash(qedlock)`, so a callee spec edit invalidates the
  caller's attestation.
- Library imports carry an `upstream_binary_hash`; peer qedspec imports
  don't (their own `qed.lock` does, and we chain through `spec_hash`).
- `qedgen lock` updates the file; `qedgen check` diagnoses drift;
  `qedgen verify --check-upstream` diagnoses mainnet-vs-pin drift on demand.

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

1. **v2.5** — modularity + Tier 0/1 CPI.
   - §1 multi-file loader (shipped).
   - §2 `interface` block + `call` instruction (Tier 0 and Tier 1).
   - `qedgen interface --idl <path>` writes Tier-0 interfaces from Anchor IDL.
   - SPL Token / System Program shipped as Tier-1 library interfaces,
     each carrying an `upstream { package, version, binary_hash, … }` block.
   - `transfers { ... }` becomes sugar for `call Token.transfer(...)`.
   - `[shape_only_cpi]` lint for Tier-0 call sites.
   - §3 remains this design doc + declared qedlock format.

2. **v2.6** — Tier 2 composition (§3 stance 1) + upstream drift detection.
   - `import Foo from "./path/to/spec"` resolution (project + library paths).
   - Importing a qedspec exposes its handlers as the interface — no
     re-declaration.
   - `qed.lock` file + `qedgen lock` + `qedgen lock --upgrade <name>`
     subcommands. Library imports flow `upstream_binary_hash` into the lock;
     peer qedspec imports chain through `spec_hash`.
   - Transitive `#[qed(verified, qedlock_hash="...")]` attribute so callee
     spec edits surface as a clear drift diagnostic instead of a silent gap.
   - `qedgen verify --check-upstream` fetches on-chain programs and
     diffs against pinned `upstream_binary_hash` (CI-friendly, network-
     optional).

3. **v2.7** — proof composition (§3 stance 2).
   - Caller's generated Lean imports callee's `formal_verification/Spec.lean`
     directly; callee `ensures` become theorems, not axioms.
   - End-to-end proven across a CPI boundary.
   - Requires Lean-module-layout conventions that depend on v2.5/v2.6
     settling first.

Anything past v2.7 (public interface registry, curated spec index, etc.) is
ecosystem-layer, not core QEDGen.
