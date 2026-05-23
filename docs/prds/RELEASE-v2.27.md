# Release v2.27.0 — from trust to proof, end to end

Minor release. v2.26 closed the CPI loop on the consumer side: the
caller's Lean theorem applies `<Iface>.<handler>.ensures_axiom_<i>`
instead of carrying `by sorry`, and the bundled SPL Token + System
Program + Metaplex stdlib resolves `import X from "spl"` /
`"system"` / `"metaplex"` without a `qed.toml` entry. The
contracts that those axioms encoded were placeholder tautologies
(`amount > 0` → `amount > 0`), and the proof package itself didn't
exist — every imported interface was Stance 1 (binary_hash pin as
sole warrant). v2.27 closes both gaps: the bundled stdlib's `ensures`
clauses now pin substantive state-aware contracts (balance
preservation, lamport conservation, verified-flag updates), and the
bundled stdlib ships its own Lake-buildable proof package so callers
get Stance 2 (imported callee theorem) end to end — without authoring
a single line of Lean themselves.

Six threads compose:

1. **State-aware ensures DSL** — interface `ensures` clauses can
   reference abstract callee-state (`state.from_balance`,
   `old(state.from_balance)`), lowered to polymorphic
   `(State → T)` accessors in the bundled axiom signature; callers
   bind them to concrete State fields via per-call-site
   `state_binders { ... }`.
2. **Verified-callee composition (Stance 2)** — when an imported
   spec ships `.qed/proofs/<Iface>.lean + lakefile.lean` alongside,
   the consumer's codegen drops the local sibling axiom module and
   injects a `require <pkg>Proofs from <path>` directive in the
   consumer's lakefile. The Spec.lean theorem application string is
   identical to Stance 1 (per the v2.27 Phase 2 spike) — discharge
   is private to the provider.
3. **Type-generic axiom accessors (Phase 0)** — `(X : State → T)`
   per declared field, with `T` chosen by an optional interface-level
   `state { name : Type, ... }` block. `Nat` for the `U*` family
   (back-compat default), `Int` for `I*`, `Bool` for `Bool`,
   `Pubkey` for `Pubkey`. Unlocks Metaplex's `creator_verified` /
   `collection_verified` (Bool-typed) bundled contracts.
4. **Substantive bundled-stdlib contracts** — SPL Token's
   `transfer` / `mint_to` / `burn` pin balance preservation + supply
   delta; System Program's `transfer` / `create_account` pin lamport
   conservation; Metaplex's verification handlers pin verified-flag
   flips and collection-size deltas. The placeholder
   `amount > 0` tautologies are gone.
5. **Bundled proof packages (`crates/qedgen/data/proofs/`)** —
   SPL Token + Metaplex ship Stance-2 Lake packages alongside the
   .qedspec fixtures. The qedgen resolver materializes them on
   demand to the per-builtin cache; Track B detection picks them
   up automatically. System Program stays Stance 1 in v2.27 (the
   `(owner : Pubkey)` params on `create_account` / `assign` would
   force the bundled module to depend on `QEDGen.Solana.Account`,
   defeating the self-contained-distribution goal).
6. **Resolver + lockfile + CLI gates (Track D)** — `qedgen verify`
   gains `--recursive` (DFS-walk transitive proof packages,
   per-layer `lake build`) and `--require-verified` (CRIT on any
   Tier-1+ import that didn't ship proofs); `qedgen check --frozen`
   routes proof_hash drift as P2 by default, escalated to CRIT with
   `--strict`.

Plus `binary_hash` pins flipped from `sha256:0000…` placeholders to
real on-chain content hashes (SPL Token + Metaplex), with the cache-
refresh path that picks up bundled-source updates on every qedgen
release.

## What's in

### Track A — state-aware ensures + `state_binders` DSL

Interface `ensures` clauses can reference abstract callee state via
`state.X` (lowered to `s'.X` ⇒ post) and `old(state.X)` (lowered to
`s.X` ⇒ pre). The bundled axiom signature gains
`{State : Type} [Inhabited State] (pre post : State) (params...)
(accessors...)` for any ensures that references abstract state.

Callers consume the contract via per-call-site `state_binders`:

```
call Token.transfer(
  amount = amount,
  state_binders {
    from_balance = state.alice_balance,
    to_balance = state.bob_balance,
  },
)
```

The caller's generated Lean theorem statement uses
`pre.alice_balance` / `post.alice_balance` projections and applies
`Token.transfer.ensures_axiom_0 pre post amount (·.alice_balance)`
to discharge. Pass-through default when a callee field isn't bound:
`(·.<callee_field>)`. The skip-comment path (introduced this
release) covers the case where the caller binds *no* state for an
ensures — the per-ensures theorem isn't emitted, and a one-line
comment explains why. This preserves back-compat for callers that
opt out of consuming the new state-aware contracts.

### Track B — verified-callee composition (Stance 2)

The resolver detects `<source_dir>/.qed/proofs/<bound_name>.lean +
lakefile.lean` alongside any imported spec (path / github / now
also bundled). `verified_callees` populates `(interface_name,
proof_pkg_root)`. Codegen reacts:

- For each verified callee, the local Stance-1 sibling axiom module
  (`<Iface>.lean`) is **not** emitted, and its entry is **removed**
  from the consumer's lakefile `roots := #[...]` if it was there.
- A `require <pkg>Proofs from "<rel-path>"` directive is **injected**
  into the consumer's lakefile, pointing at the resolved proof
  package root. The naming convention is `<lowercase-first-char +
  rest>Proofs` (e.g. `Token` → `tokenProofs`, `Metadata` →
  `metadataProofs`).

The Spec.lean theorem application is **byte-identical** between
Stance 1 and Stance 2 — the consumer's symbol path
`<Iface>.<handler>.ensures_axiom_<i>` resolves to either the local
axiom or the imported theorem depending on which lakefile entry
wins.

### Phase 0 — type-generic axiom accessors

The Track A accessor slot was hardcoded `(X : State → Nat)`. v2.27
Phase 0 extends it to `(X : State → T)` with `T` chosen by an
optional interface-level `state { name : Type, ... }` block.

```
interface Metadata {
  state {
    creator_verified    : Bool
    collection_verified : Bool
    collection_size     : U64
  }
  handler sign_metadata {
    ensures state.creator_verified == true
  }
}
```

Type map: `Nat` for the `U*` family (also the back-compat default
when a field isn't declared), `Int` for the `I*` family, `Bool`
for `Bool`, `Pubkey` for `Pubkey`. Implementation: ~40 LOC across
`ast.rs`, `chumsky_parser.rs`, `chumsky_adapter.rs`, `check.rs`,
`lean_gen.rs`. Existing Track A specs continue to lower to
`State → Nat` accessors unchanged (covered by
`track_a_axiom_extends_signature_and_theorem_applies_accessor`).

### Track C1 — substantive bundled-stdlib contracts

The three bundled qedspecs gain state-aware `ensures`:

| Interface / Handler          | New ensures                                                                                                  |
|------------------------------|--------------------------------------------------------------------------------------------------------------|
| `Token.transfer`             | `state.from_balance == old(...) - amount` ∧ `state.to_balance == old(...) + amount`                          |
| `Token.mint_to`              | `state.total_supply == old(...) + amount` ∧ `state.to_balance == old(...) + amount`                          |
| `Token.burn`                 | `state.total_supply == old(...) - amount` ∧ `state.from_balance == old(...) - amount`                        |
| `System.create_account`      | `state.payer_lamports == old(...) - lamports` ∧ `state.new_account_lamports == lamports`                     |
| `System.transfer`            | `state.from_lamports == old(...) - amount` ∧ `state.to_lamports == old(...) + amount`                        |
| `Metadata.sign_metadata`     | `state.creator_verified == true`                                                                             |
| `Metadata.set_and_verify_collection` | `state.collection_verified == true` ∧ `state.collection_size == old(...) + 1`                        |
| `Metadata.verify_collection` | `state.collection_verified == true`                                                                          |

Pre-state semantics: the success-path framing — the bundled axiom
documents what holds *after* a successful invocation; the runtime
checks (insufficient balance, wrong authority, etc.) cause the call
to abort before the post-state is reached, and the caller's proof
doesn't need to discharge that path.

Lifecycle handlers (initialize_account, close_account, assign,
update_metadata_account_v2, create_metadata_account_v3) stay
shape-only — their post-states are payload bytes (metadata buffers,
account-init) that don't decompose into single abstract-accessor
slots. Adding per-field accessors (or a per-byte payload type) is
v2.28+ scope.

### Track C2 + C3 — bundled proof packages + real binary pins

Two new directories ship with the qedgen binary:

- `crates/qedgen/data/proofs/spl/{lakefile.lean, Token.lean}`
- `crates/qedgen/data/proofs/metaplex/{lakefile.lean, Metadata.lean}`

Each lakefile declares `package <iface>Proofs` + a default-target
`lean_lib`. Each module mirrors codegen's axiom signatures with
`theorem` declarations whose body appeals to one named
`<handler>.runtime_trust_<binder>` axiom. Net trust surface:
unchanged from Stance 1 (the runtime-trust axioms are the contract
boundary, just like `ensures_axiom_<i>` was), but the structure is
documented and consolidated — one axiom per `(handler,
abstract_field)` pair, named for what it asserts.

The resolver's `resolve_builtin_dep` materializes the proof package
alongside the qedspec at `<cache>/builtin/<key>/.qed/proofs/`. The
existing Track B detection then sees both files and populates
`verified_callees` automatically. No special builtin-aware code
path in the codegen or detector.

System Program intentionally has **no** bundled proof package in
v2.27. Its `create_account` (`(owner : Pubkey)` param) and `assign`
(`(new_owner : Pubkey)` param) handlers would require the bundled
module to import `QEDGen.Solana.Account` for the `Pubkey` type. A
self-contained bundled package is the v2.27 goal — adding a
`require qedgenSupport` to the bundled lakefile (and shipping
`qedgenSupport` alongside in the binary, with a transitive Lake
graph that resolves cleanly) is v2.28 scope. `import System from
"system"` therefore stays Stance 1 in v2.27, identical to v2.26.

**Real binary_hash pins** (C3): SPL Token + Metaplex now ship with
sha256 pins captured from mainnet payload dumps on 2026-05-23. The
System Program stays at the all-zero sentinel (it's a native
program; no deployed binary to hash). The cache materializer
refreshes the on-disk copy whenever the bundled `include_str!`
source differs, so a qedgen-version upgrade is visible to
consumers without a manual `rm -rf ~/.qedgen/cache/builtin`.

### Track D — `--recursive` / `--require-verified` / proof_hash strict escalation

Three new CLI flags:

- **`qedgen verify --recursive`** — walks the transitive proof-pkg
  closure (DFS-pre-order, dedup by path) and runs `lake build` per
  layer. Per-layer pass/fail reported; exits non-zero on any
  failure. Failed layers show the first ~10 lines of stderr/stdout.
- **`qedgen verify --require-verified`** — exits non-zero on any
  imported Tier-1+ interface (binary_hash + ensures) that did **not**
  ship a `.qed/proofs/` package alongside. Tier-0 (no ensures) and
  sentinel-pinned natives (all-zero binary_hash) are exempt; the
  former is the `cpi_no_callee_ensures` P1 lint's territory and
  the latter is documented runtime trust.
- **`qedgen check --frozen --strict`** — escalates proof_hash drift
  from P2 advisory (default) to CRIT, exits 1. Plain `--frozen`
  routes proof_hash drift through soft findings and stays exit-0
  (mirrors v2.26's binary_hash routing).

`--require-verified` is default-off in v2.27 because the bundled
stdlib still ships Stance 1 for System Program; default-on would
always fail on `import System from "system"`. Re-evaluate for
v2.28 if System gains a bundled proof package.

### Bundled-stdlib end-to-end example

New `examples/rust/bundled-stdlib-demo/` exercises the complete
Stance-2 path: caller's `deposit` handler imports `Token from
"spl"`, supplies `state_binders { to_balance = state.pool_balance
}`, and the generated Spec.lean applies
`Token.transfer.ensures_axiom_1 pre post amount (·.pool_balance)`
against the bundled `tokenProofs` package's imported theorem.
`lake build` succeeds; the bundled proof package materializes from
the qedgen cache; the lakefile rewrites correctly. Joins the
`scripts/check-lake-build.sh` sweep (now 11 / 11).

## CLI surface added

| Flag                                        | Subcommand            | Effect                                                                              |
|---------------------------------------------|-----------------------|-------------------------------------------------------------------------------------|
| `--recursive`                               | `verify`              | DFS-walk transitive proof packages; `lake build` per layer; exit non-zero on any failure |
| `--require-verified`                        | `verify`              | CRIT on any Tier-1+ import without a bundled proof package; default-off in v2.27   |
| `--strict` (proof_hash escalation)          | `check --frozen`      | Promotes proof_hash drift from P2 to CRIT; exit non-zero                            |

## DSL surface added

| Construct                                    | Where it appears                                  | What it does                                                                                |
|----------------------------------------------|---------------------------------------------------|---------------------------------------------------------------------------------------------|
| `state { name : Type, ... }`                 | Top-level inside `interface { ... }`              | Declares the abstract callee-state vocabulary; types pick the bundled axiom accessor codomain. |
| `state.X` / `old(state.X)` in interface `ensures` | Per-handler `ensures` body                     | Lowers to `s'.X` / `s.X` in Lean, then to `(X post)` / `(X pre)` in the bundled axiom body. |
| `state_binders { callee_field = state.caller_field, ... }` | Per `call Foo.handler(...)` arg block | Maps the callee's abstract accessors onto caller-state fields at the application site.       |

## Test surface

+18 unit tests across this release (908 → 926), spanning:

- Phase 0 typed-accessor codomain (`phase_0_typed_state_block_drives_axiom_accessor_codomain`) — Bool / Pubkey accessor slots emit correctly; no Nat default when type is declared.
- Phase 0 caller-skip path (`phase_0_caller_with_no_state_binders_skips_state_aware_ensures`) — un-engaged callers get a one-line comment, no broken `(·.<field>)` references for caller-state fields that don't exist.
- Track D D1 (4 routing + 4 detector + 1 frozen-bail companion) — proof_hash routing across Auto / Skip / Frozen / Frozen-strict.
- Track D D2 (5 cases) — `--require-verified` predicate fires on unverified+ensures; silent on Tier-0 / sentinels / inline interfaces.
- Track D D3 (2 cases) — `verified_proof_pkgs` populates when proofs ship; empty otherwise.

All 926 tests pass on release builds (`cargo test --release --bin
qedgen`); 4/4 codegen_smoke; 4/4 upstream_check_e2e.

## Pre-release gate sweep

- `cargo fmt --check` ✓
- `cargo clippy -- -D warnings` ✓
- `cargo test --release --bin qedgen` ✓ (926 / 926)
- `scripts/check-readme-drift.sh` ✓ (19 / 19 commands documented)
- `scripts/check-lake-build.sh` ✓ (11 / 11 examples lake-build clean)
- Frozen check across all bundled examples ✓ (advisory warnings only)
- Regen-drift sweep across 7 examples ✓

## What's NOT in v2.27

- **System Program bundled proof package.** Pubkey-typed params on
  `create_account` / `assign` would require a bundled
  `QEDGen.Solana.Account` import. Self-contained-distribution stays
  the v2.27 goal; v2.28 revisits.
- **Metaplex byte-payload contracts.** `create_metadata_account_v3`
  and `update_metadata_account_v2` operate on metadata buffers
  (name / symbol / uri / creator slot mutations) that don't fit a
  single abstract-accessor slot. v2.28 may introduce per-field
  decomposition or a "raw payload" accessor type.
- **Lean-model-discharged theorems.** The bundled proof packages
  derive their `theorem ensures_axiom_<i>` declarations from named
  `runtime_trust_<binder>` axioms. Replacing those axioms with
  actual proofs against a Lean model of each program's binary
  semantics is multi-month scope (formal-svm spinout) and not
  v2.27 work. The structural improvement is real — trust is now
  consolidated + named — but the trust *surface* is the same as
  Stance 1.
- **Soft-deprecation of `qedgen check --regen-drift`'s file-by-file
  drift reporting.** Untouched in v2.27. The directory-level
  reporting added in earlier releases stays the canonical surface.

## Known limitations carried forward

| Limit                                                          | Source                | Notes                                  |
|----------------------------------------------------------------|-----------------------|----------------------------------------|
| Stale `require` line on verified → unverified flip             | `lean_gen::inject_verified_callee_requires` | ~15 LOC fix queued for v2.27.x      |
| CWD-relative `--lean-output` default                           | `main.rs` codegen handler | ~10 min fix queued                  |
| Lockfile diff renderer misses `program_id` + `upstream_version` | `qed_lock::describe_lock_diff` | Minor; not blocking                |
| `is_sentinel_hash` accepts `SHA256:` prefix shorthand          | Defensive against hand-edited specs | Intentional looseness            |
| `--recursive`'s lake build runs in the host shell, not sandboxed | `main.rs` verify handler | A malicious provider's lakefile.lean could run arbitrary Lean; v2.27 trust model is "you opted into the dep when you wrote `import X from \"Y\"`"; sandboxing is v3.0+ scope |

## Upgrade notes

- **Bundled stdlib qedspec hashes change**: every caller's
  `qed.lock` will need a refresh after upgrading. Run any
  non-`--frozen` `qedgen check` to auto-update; CI gates that pin
  `qed.lock` should expect the diff.
- **Codegen now emits `instance : Inhabited <State>` for variant
  State.** Existing examples with hand-edited Lean that conflicts
  with auto-derived Inhabited will need to drop their manual
  instance.
- **`Token.lean` no longer ships in `formal_verification/`** when
  the caller imports `Token from "spl"` — the bundled tokenProofs
  package owns the symbols via the lakefile `require` directive.
  Any user code that hand-edited the local `Token.lean` will need
  to migrate (or pin to v2.26).
