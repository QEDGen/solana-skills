# Spec dependencies — `import`, `qed.toml`, `qed.lock` (v2.8)

A spec that calls into another program (SPL Token, System Program, an AMM
shipping its own `.qedspec`) declares its dependencies in `qed.toml` and
binds them by name with `import`. The resolver fetches the imported sources
(GitHub or local path), merges the named `interface` blocks into the
consumer's namespace, and writes a `qed.lock` snapshot that pins exactly
which version was used. Same shape as `Cargo.toml` + `Cargo.lock`.

Before v2.8, consumers had to inline-paste the callee's interface block into
their own spec. That worked but didn't scale and lost the upstream pin.
v2.8 closes the loop.

## The `import` keyword

Top-level statement in a `.qedspec`:

```fsharp
spec Escrow

import Token   from "spl_token"
import MyAmm   from "my_amm"
import Tk      from "spl_token" as Tk      // optional `as <alias>` rename

// ... handler bodies that `call Token.transfer(...)` and `call MyAmm.swap(...)`
```

- The first identifier is the **source-side interface name** — must match
  an `interface <Name> { ... }` declared in the imported source.
- The string after `from` is a **dep key** that must match an entry in
  `qed.toml` under `[dependencies]`.
- An optional `as <alias>` clause renames the imported interface in the
  consumer's namespace. Use it when two imports would otherwise clash on
  the same source-side name, or when the source-side name doesn't fit
  your local convention. Without `as`, the source-side name is the local
  name.

`from` and `as` are contextual keywords — handlers still use `from = expr`
in CPI call arguments without colliding.

## The `qed.toml` manifest

A TOML file at the spec root, alongside the `.qedspec` (or in the directory
passed to `qedgen check --spec <dir>`). Cargo-style:

```toml
[dependencies]
spl_token      = { github = "QEDGen/solana-skills", path = "interfaces/spl_token", tag = "v2.8.0" }
system_program = { github = "QEDGen/solana-skills", path = "interfaces/system_program", branch = "main" }
my_amm         = { path = "../my_amm" }
```

Each entry is one of two source forms.

### Builtin sources (v2.26)

QEDGen ships a small interface stdlib resolved before manifest lookup:

```
import Token    from "spl"        # crates/qedgen/data/interfaces/spl_token.qedspec
import System   from "system"     # crates/qedgen/data/interfaces/system.qedspec
import Metadata from "metaplex"   # crates/qedgen/data/interfaces/metaplex.qedspec
```

The `"spl"`, `"system"`, and `"metaplex"` keys are reserved and
resolved via embedded `include_str!` fixtures — no `qed.toml` entry
needed, and a manifest that contains *only* builtin imports validates
as empty. The bundled fixtures are Tier-1 (declared `ensures` +
`upstream { binary_hash }` pin), so callers' Lean theorems discharge
via `<Interface>.<handler>.ensures_axiom_<idx>` and Kani harnesses
propagate the substituted ensures as `kani::assume` after the call
site. The Metaplex fixture covers the high-traffic Token Metadata
handlers — `create_metadata_account_v3`, `update_metadata_account_v2`,
`sign_metadata`, `verify_collection`, `set_and_verify_collection` —
the canonical CPI surface for NFT mint, metadata update, and
creator/collection verification. The pinned `binary_hash` values in
all three bundled fixtures are `sha256:0000...` placeholders pending
audit — the contract chain is structurally complete but
cryptographically un-anchored until real pins land.

v2.27 — the bundled fixtures gained substantive state-aware
contracts (balance preservation on `Token.transfer`, lamport
conservation on `System.transfer`, verified-flag flips on
`Metadata.sign_metadata`, …). SPL Token + Metaplex also ship
bundled Lake-buildable proof packages alongside the qedspec
fixtures, materialized to `<cache>/builtin/<key>/.qed/proofs/`
on demand — callers automatically get Stance 2 (imported callee
theorem) end-to-end with no manual proof authoring. System Program
intentionally stays Stance 1 in v2.27; see the "Stance 1 vs
Stance 2" section below.

Real `binary_hash` pins replace the v2.26 `sha256:0000…`
placeholders for SPL Token + Metaplex (captured 2026-05-23 from
mainnet payload dumps). The System Program stays at the all-zero
sentinel (native program; no deployed binary to hash).

User-authored interfaces (`import X from "..."` referencing a github
or path source declared in `qed.toml`) continue to work unchanged.

### Stance 1 vs Stance 2 — verified-callee composition

Two ways an imported interface's `ensures` clauses can be discharged
at the caller's `lake build`:

- **Stance 1** (default, unchanged from v2.26): codegen writes a
  local sibling `<Iface>.lean` axiom module next to `Spec.lean`.
  The caller's theorem applies `Token.transfer.ensures_axiom_<i>`
  against an `axiom` declared in that local module. Trust anchor:
  the `upstream { binary_hash }` pin on the interface.
- **Stance 2** (v2.27, opt-in via shipped proofs): when the
  provider ships `<source_dir>/.qed/proofs/<Iface>.lean +
  lakefile.lean` alongside the qedspec, the resolver detects it
  and:
  - Codegen does NOT emit the local `<Iface>.lean` axiom module.
  - The consumer's `lakefile.lean` gets a `require <pkg>Proofs
    from "<rel-path>"` directive injected (the package name is
    `<lowercase-first-char + rest>Proofs` — e.g. `tokenProofs`,
    `metadataProofs`).
  - The caller's theorem applies the same identifier — but it now
    resolves to the imported `theorem` from the provider package
    instead of the local `axiom`. Per the v2.27 Phase 2 lake-graph
    spike, the Spec.lean theorem application string is byte-
    identical between stances.

Bundled stdlib coverage in v2.27:

- `import Token from "spl"` → Stance 2 (bundled `tokenProofs` package).
- `import Metadata from "metaplex"` → Stance 2 (bundled `metadataProofs`).
- `import System from "system"` → Stance 1 (no bundled proof package
  in v2.27 — its handlers have `Pubkey` params that would require
  the bundled module to import `QEDGen.Solana.Account`, defeating
  the self-contained-distribution goal).

`qedgen verify --require-verified` is the CI gate that surfaces any
imported Tier-1+ interface that's still on Stance 1. Default-off
in v2.27 because System Program is unbundled-Stance-1; revisit when
its bundled proof package lands.

### Abstract callee-state vocabulary

An interface can declare its abstract callee-state vocabulary with an
optional `state { name : Type, ... }` block (v2.27 Phase 0). The
named fields can be referenced from per-handler `ensures` clauses via
`state.X` (post-state) and `old(state.X)` (pre-state); they lower to
polymorphic `(X : State → T)` accessors in the bundled axiom
signature. Callers map them to concrete State fields via per-call
`state_binders`:

```
interface Token {
  state {
    from_balance : U64
    to_balance   : U64
    total_supply : U64
  }
  handler transfer (amount : U64) {
    requires amount > 0
    ensures  state.from_balance == old(state.from_balance) - amount
    ensures  state.to_balance == old(state.to_balance) + amount
  }
}
```

Caller side:

```
call Token.transfer(
  amount = amount,
  state_binders {
    from_balance = state.alice_balance,
    to_balance = state.bob_balance,
  },
)
```

Type map: `Nat` for the `U*` family (also the back-compat default
when a field is not declared in `state { ... }`), `Int` for `I*`,
`Bool` for `Bool`, `Pubkey` for `Pubkey`. Skip-comment fallback:
when the caller supplies NO binders for an ensures, that per-ensures
theorem is silently dropped from the caller's Spec.lean with a
one-line explanatory comment — the contract still holds in the
callee (binary_hash is the warrant), the caller just doesn't pull
it into its own proof.

### GitHub source

`github = "org/repo"` plus exactly one of `tag = "..."`, `branch = "..."`, or
`rev = "..."`. The optional `path = "..."` is a sub-path within the repo;
omit it when the spec lives at the repo root.

The resolver shells out to `git clone --depth=1 --branch <ref>` for tags and
branches, or a full clone + `git checkout <rev>` for commit hashes. Cached
under `~/.qedgen/cache/github/<org>/<repo>/<kind>/<ref>/`. Override the cache
location via `QEDGEN_CACHE_DIR=` (used by tests).

The first `qedgen check` after declaring a dep does the network fetch.
Subsequent runs hit the cache and stay offline.

### Path source

`path = "..."` — relative to `qed.toml`'s directory, or absolute. No git
involved. Useful for monorepos where dep sources live alongside the consumer
spec.

```toml
[dependencies]
my_amm = { path = "../my_amm/spec.qedspec" }   # single file
my_amm = { path = "../my_amm" }                 # directory of fragments
my_amm = { path = "../my_amm/spec" }            # auto-extension to .qedspec
```

Path source resolution accepts a single `.qedspec` file, a directory of
fragments (multi-file specs in sorted-path order), or a path that's missing
the `.qedspec` extension (auto-resolved). v2.8 supports both single-file
and **multi-file imported deps** — every fragment must declare the same
`spec Name`, and their top items merge in sorted-path order.

## The `qed.lock` snapshot

Written by `qedgen check` whenever the resolved import set changes. Same
directory as `qed.toml`. Checked into version control (cargo / yarn
convention).

```toml
# Generated by qedgen — do not edit by hand.
version = 1

[[dependency]]
name = "spl_token"
source = "github:QEDGen/solana-skills"
ref = "v2.8.0"
resolved_commit = "a1b2c3d4e5f67890..."
path = "interfaces/spl_token"
spec_hash = "sha256:7f3a..."
upstream_binary_hash = "sha256:9c1e..."
upstream_version = "spl-token@4.0.3"

[[dependency]]
name = "my_amm"
source = "path:../my_amm"
spec_hash = "sha256:b240..."
```

Each entry pins both:

- `ref` — the human-readable tag/branch/rev value the user wrote in
  `qed.toml`.
- `resolved_commit` — the immutable git commit hash captured at fetch time.

If a tag is force-pushed, the next resolution discovers a different commit;
the lock catches the change by diffing `resolved_commit` even though `ref`
is identical.

For path-source deps, only `name`, `source`, and `spec_hash` are written —
the rest are absent (no commit to capture).

`upstream_binary_hash` and `upstream_version` are filled from the imported
interface's `upstream { ... }` block when present. They feed the
`--check-upstream` verification (below).

### `qedgen check --frozen`

CI flag. With `--frozen`, the resolver errors with a per-dep diff if the
on-disk lock differs from the freshly computed one — no auto-write. Use this
in CI to detect unbumped deps:

```bash
qedgen check --frozen --spec ./
# error: qed.lock at ./qed.lock is stale (--frozen):
#   ~ spl_token (changed):
#       resolved_commit: Some("a1b2…") → Some("def4…")
#       spec_hash: sha256:7f3a → sha256:5c11
```

Default behavior (no `--frozen`) silently auto-writes the lock when stale,
which is the right default for local development.

### `qedgen check --no-cache`

Force-refreshes the github source cache for every imported dep. Wipes
`~/.qedgen/cache/github/<org>/<repo>/<kind>/<ref>/` and re-clones from
upstream. Use when:

- An upstream tag was force-pushed and you want the new commit *now*
  rather than waiting for the TTL window to expire.
- You suspect a corrupted cache.
- You want a hermetic CI run that doesn't trust the cache.

The cache is otherwise refreshed automatically when its
`.qedgen-commit` marker is older than `QEDGEN_CACHE_TTL` seconds (default
604800 = 7 days). `QEDGEN_CACHE_TTL=0` disables the time-based check
entirely; rely solely on `--no-cache`.

## `qedgen verify --check-upstream`

Diffs every imported library's pinned `upstream_binary_hash` against the
on-chain `.so` binary. Implemented via dispatch to the user's existing
`solana` CLI:

```bash
qedgen verify --spec ./escrow.qedspec --check-upstream
# · spl_token: skipped — no upstream_binary_hash pinned
# ✓ token_metadata (mtoken…): sha256:9c1e…
# ✗ jupiter_v6 (jupv6…): MISMATCH
#     pinned:   sha256:abc…
#     on-chain: sha256:def…
```

Pass `--rpc-url <url>` to override the cluster — otherwise the Solana CLI
inherits whatever's in `~/.config/solana/cli/config.yml`. No qedgen-side RPC
config to manage.

`--offline` refuses any network fetch — useful in CI gates that should
never reach external network. Pinned deps report as Error("offline mode")
instead of shelling out; entries without a pin still skip cleanly.

The `solana` CLI must be in PATH; otherwise the command exits with an
install pointer.

Per-dep outcomes:

- **Match** — on-chain hash matches the pinned hash.
- **Mismatch** — non-zero exit; surface the pinned/on-chain values for the
  user to investigate.
- **Skipped** — dep has no `upstream_binary_hash` (path source, library
  entry that hasn't been pinned yet, or the imported interface omits a
  `program_id`).
- **Error** — `solana` CLI failed (network, auth, CLI missing) or
  `--offline` blocked a needed fetch.

### v2.26 — differentiated severity gates

`--check-upstream` is now a real verification gate with severity that
varies by surface:

| Surface | Mismatch severity | Blocking? |
|---|---|---|
| `qedgen verify --check-upstream` | **CRIT** | yes (non-zero exit) |
| `qedgen verify --check-upstream --upstream-stale-ok` | Info | no |
| `qedgen check --frozen` (default) | P2 warning | no |
| `qedgen check --frozen --strict` | **CRIT** | yes |

Auto-on when `qed.lock` declares any pinned hash; no-op otherwise.
Network/CLI errors (missing `solana`, unreachable RPC) stay P2 under
any gate — a missing toolchain does NOT false-positive CRIT. Use
`--upstream-stale-ok` for offline development; use `check --frozen
--strict` in CI when frozen-lock drift should block the build.

## End-to-end example: escrow with SPL Token

`examples/rust/escrow-split/qed.toml`:

```toml
[dependencies]
spl_token = { path = "../../../interfaces/spl_token.qedspec" }
```

`examples/rust/escrow-split/escrow.qedspec`:

```fsharp
spec Escrow

import Token from "spl_token"

type State
  | Uninitialized
  | Open of {
      initializer        : Pubkey,
      taker              : Pubkey,
      initializer_amount : U64,
      taker_amount       : U64,
    }
  | Closed

handler exchange : State.Open -> State.Closed {
  auth taker
  accounts {
    taker          : signer, writable
    escrow         : writable, pda ["escrow", initializer]
    initializer_ta : writable, type token
    taker_ta       : writable, type token
    escrow_ta      : writable, type token, authority escrow
    token_program  : program
  }
  call Token.transfer(
    from      = taker_ta,
    to        = initializer_ta,
    amount    = taker_amount,
    authority = taker,
  )
}
```

What lands downstream:

- **Lean** (v2.8 G3): the substituted `Token.transfer.ensures` is bound at
  the call site as `theorem exchange_Token_transfer_call_0_post_0 (s : State)
  : s.taker_amount > 0 := by sorry`. Stance 1 axiomatization — v3.0 stance 2
  will close the `sorry` via imported callee proofs.
- **Rust** (v2.8 G4): the `call Token.transfer(...)` site emits a real
  `anchor_spl::token::transfer(CpiContext::new(...), ...)?` body, no
  `todo!()` for that line.
- **Lock**: `qed.lock` records `name = "spl_token"`, `source =
  "path:../../../interfaces/spl_token.qedspec"`, `spec_hash = sha256:...`.
  `--frozen` enforces this in CI.

## What's not in v2.8 yet

- **Transitive resolution.** Imported specs that themselves contain
  `import` statements aren't walked. Each consumer declares its own direct
  deps. Stance-1 design choice — see `docs/design/spec-composition.md` §3.
  v2.9 will add transitive walks once version-conflict resolution is
  designed.
- **Generic Anchor CPI codegen** for non-SPL interfaces. v2.8 mechanizes
  every SPL Token handler (transfer / mint_to / burn / initialize_account
  / close_account); other interfaces still emit comment + `todo!()`.
  v2.9 (Anchor first-class release) ships generic Anchor CPI via Borsh +
  sighash + AccountMeta synthesis.
- **First-class Anchor support** — `#[qed]` on existing Anchor handlers
  (free-fn, type-associated, accounts-method, and inline shapes),
  brownfield `qedgen adapt` and `qedgen check --anchor-project`. v2.9
  headline. Anchor and Quasar are both fully supported; non-Anchor /
  raw-program (Pinocchio) reserves the CLI surface but is not yet
  implemented.
- **Stance 2 (proof composition).** `sorry` in the ensures-as-axiom theorems
  stays — v3.0 (refactor + breaking-changes release) will replace with
  imported callee proofs.
