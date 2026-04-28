# `qedgen readiness` / `check-upgrade` — Quasar demo

Worked example showing qedgen running ratchet's lint pipeline against a
[Quasar](https://github.com/blueshift-gg/quasar)-emitted IDL. Same
subcommands and flags as the Anchor flow — just add `--quasar` (or run
inside a directory that contains a `Quasar.toml`, in which case qedgen
autodetects).

## Files

- **`escrow.json`** — a 3-instruction (`make`, `take`, `refund`)
  escrow program shaped exactly like `quasar build` writes to
  `target/idl/<program>.json`. Hackathon-typical: no `version` field,
  no `_reserved` padding, so readiness flags it.
- **`escrow.v2.json`** — a deliberately broken upgrade of `escrow.json`
  that fires four breaking-change rules at once:
  - removes `refund` (R007 instruction-removed)
  - retypes `make.receive` from `u64` → `u32` (R008 instruction-arg-change)
  - reorders the `Escrow` account fields (R001 account-field-reorder)
  - changes the `Escrow` discriminator from `[42]` → `[99]` (R006 account-discriminator-change)

The fixtures are duplicates of the ones in
[`solana-ratchet-quasar`'s repo](https://crates.io/crates/solana-ratchet-quasar)
— Quasar's compiler isn't on qedgen's CI, so we commit JSON shaped to
match its emission rather than building it.

## Try it

```sh
# Mainnet-readiness lint (no upgrade, just "is this safe to deploy")
qedgen readiness --idl examples/quasar-readiness/escrow.json --quasar

# Upgrade-safety lint (compare two builds)
qedgen check-upgrade \
  --old examples/quasar-readiness/escrow.json \
  --new examples/quasar-readiness/escrow.v2.json \
  --quasar
```

Expect:

- `readiness` → **UNSAFE** with P001 (missing `version` field) +
  P002 (missing `_reserved` padding) on `Escrow`.
- `check-upgrade` → **BREAKING** with R001, R006, R007, R008.

## Autodetect

If you run qedgen from a directory that contains a `Quasar.toml`
(and no shadowing `Anchor.toml`), the `--quasar` flag is picked up
automatically — qedgen will print a one-line stderr banner saying so,
unless `--json` is passed (banners are suppressed under JSON output
to keep stdout machine-parseable).

## What runs vs. what's silenced

The framework-agnostic R-rules (R001–R016) and the readiness P-rules
fire regardless of which framework emitted the IDL — they operate on
ratchet's framework-neutral `ProgramSurface` IR.

Two preflight rules are intentionally silenced for Quasar IDLs:

- **P003 (discriminator-pinning)** — Quasar requires devs to assign
  account discriminators explicitly in source, so by the time the IDL
  exists the value is already pinned. There's nothing to flag.
- **P004 (sha256-anchor-default)** — the rule's whole job is to catch
  the Anchor codegen default; on a Quasar IDL it's a category error.

If you want to trace why a particular finding fired (or didn't), pass
`--list-rules` to either subcommand to see the live rule catalog.
