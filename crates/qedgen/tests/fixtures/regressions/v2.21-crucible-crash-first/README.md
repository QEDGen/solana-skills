# v2.21 §S1.2 — Crucible brownfield regression fixture

This fixture pins the v2.21 §S1.2 exit criterion: the **lamport-
conservation protocol invariant** (`assert_no_wallet_inflation`) firing on
a brownfield Anchor program **without a `.qedspec`**, end to end —
`cargo build-sbf` → `qedgen probe --fuzz` → a fired `Finding`.

It also documents, by counter-example, what crash-first does **not**
catch — see `run` / `maybe` below.

## What's in `buggy_anchor/`

A minimal Anchor program with three handlers:

1. **`drain` — FIRES.** Empties a **program-owned `vault` PDA** into the
   calling `authority` with **no check that the caller is the legitimate
   admin** — a textbook missing-authority-check withdraw. A program may
   freely debit accounts it owns, so the direct lamport move succeeds for
   any signer. `authority` is a tracked wallet; it *gains* the vault's
   lamports (which come from **outside** the tracked set), tripping the
   §S1.2 `assert_no_wallet_inflation` guard. The fuzzer surfaces it as a
   HIGH `invariant_violation` on `drain`, with no spec annotation.

2. **`run` — does NOT fire.** Divides by a runtime zero. An in-program
   **SBF fault** surfaces as a transaction *error*, not a host-process
   panic, so Crucible's intrinsic crash detector never sees it.

3. **`maybe` — does NOT fire.** `Option::unwrap()` on `None`. Same story:
   a program-side abort, not a host crash.

`run` / `maybe` are kept deliberately as controls (each takes the
`authority` signer so it actually executes and faults in-program).
Crucible's "crash-first" detector catches host-process panics +
`fuzz_assert!` invariant violations (like the §S1.2 guard) — **not** faults
inside the sandboxed `.so`. The drain path fires because it trips a
*protocol invariant the harness checks in-process*, not a program panic.

The harness stages the realistic topology with no spec: `qedgen` reads the
committed IDL, sees `vault`'s `pda` node, and emits setup that creates the
vault **program-owned and funded** (so the program can debit it) at the
same `find_program_address(&[], program_id)` address the handler derives.

> Note on `run`'s divisor: it's runtime-derived (`authority.lamports() -
> authority.lamports()`) rather than `let zero = 0`. rustc's
> `unconditional_panic` lint const-folds the literal form into a *compile*
> error, so the crate would never build and `cargo build-sbf` couldn't
> emit a `.so`.

## Running the harness

This fixture is a standalone single crate (deliberately **not** a member
of the qedgen cargo workspace — its `Cargo.toml` would otherwise drag
`anchor-lang` + the Solana toolchain into every `cargo build`). It ships a
committed **`idl.json`**, so there's no `anchor build` round-trip — only
the program `.so` is built locally.

```bash
# 1. Copy the fixture out of the repo so it's a standalone crate.
cp -r crates/qedgen/tests/fixtures/regressions/v2.21-crucible-crash-first/buggy_anchor /tmp/
cd /tmp/buggy_anchor

# 2. Build the program .so. No Anchor workspace needed — the committed
#    idl.json supplies the schema the harness macro consumes.
cargo build-sbf            # → target/deploy/buggy_anchor.so

# 3. Fuzz (needs `crucible` on PATH). qedgen emits the brownfield harness,
#    discovers the committed idl.json (auto-filling the `accounts::*`
#    literals and the §S1.2 tracked-signer guard — no agent-fill), builds
#    it, and runs Crucible.
qedgen probe --fuzz 30 --root /tmp/buggy_anchor
```

Expected output: one `Finding` with `category_tag =
"invariant_violation"`, `severity = "high"`, `handler = "drain"`, and an
`investigation_hint` pointing at `crucible show … --replay`. `run` /
`maybe` execute and fail as transaction errors, producing **no** finding
(by design — see above).

### Why a committed IDL + `cargo build-sbf` (not `anchor build`)

`anchor build` requires an Anchor *workspace* (`Anchor.toml` +
`programs/<name>/` + `overflow-checks`), produces artifacts under the
*workspace* `target/`, and refuses a bare crate. `qedgen probe --fuzz`
wants a single-crate `--root` whose `src/`, IDL, and `.so` all sit
together. Committing the IDL and building the `.so` with `cargo build-sbf`
collapses that mismatch — the same committed-IDL convention the
`buggy_pinocchio` fixture uses. `qedgen`'s `discover_idl` falls back to
`<root>/idl.json` precisely for this case.

## What this fixture validates

- **CLI gate lift** — `qedgen probe --fuzz 0 --root <path>` exits 0
  without a `.qedspec`.
- **Brownfield handler + account discovery** — `run` / `maybe` / `drain`
  appear as `action_*`, and the IDL-driven path fills their
  `accounts::*` literals (and the drain signer set) — no `todo!()`.
- **Program-owned PDA staging** — the IDL's `pda` node makes the harness
  `setup()` create the `vault` program-owned + funded, so `drain` can
  actually debit it (the realistic withdraw shape).
- **Protocol-mode header** — the emitted `main.rs` carries the
  `Mode: PROTOCOL (no spec)` banner.
- **§S1.2 guard wiring** — `assert_no_wallet_inflation` +
  `snapshot_lamports` helpers are emitted and the per-action inflation
  check wraps every `.send()` once a tracked wallet set exists.
- **`.qed/fuzz/<prog>/` location** — the emitted harness lives under the
  user's `.qed/` ephemeral namespace, not in the program crate's `src/`.

The emission criteria are covered by the unit + integration tests in
`crates/qedgen/tests/crucible_brownfield_smoke.rs`. The live fuzz finding
the bug needs `crucible` on PATH; this README documents that manual run.
