# label-vault

A small Pinocchio 0.9.3 program for the label-driven lift (#450). It has three
instructions (`initialize` 0, `increment` 1, `reset` 2), one authority check
(signer plus a key compare against `Vault.authority`), one account state struct
and several error paths.

Two choices keep it inside what the success-path analysis supports today:

- Every function is `#[inline(always)]`, so the handler logic sits in the
  entrypoint. The path conditions only see guards in the entry function. A call
  that returns is opaque to them.
- Keys are compared with `pubkey_eq` (four u64 compares), not `memcmp`.

All three handlers end in one shared success exit, so the program has one
success edge and each instruction is a route into it.

## Build

The committed pair in qedsvm (`qedsvm-rs/tests/fixtures/label_vault{,.debug}.so`)
comes from one `qedgen build` of this directory at a clean commit:

```sh
qedgen build --program crates/qedgen/tests/fixtures/pinocchio-fixtures/label-vault \
  --arch v0 --tools-version v1.57
```

The Solana 3.0.1 `solana-verify` image accepts `--arch v0` with platform-tools
v1.57. On Apple Silicon set `DOCKER_DEFAULT_PLATFORM=linux/amd64`.

| File | SHA-256 |
| --- | --- |
| `label_vault.so` | `914f0f0e58ec5c19b7eaba9a0f7d9eb978f5053fcd2383db91985c63368dfc8d` |
| `label_vault.debug.so` | `9bf8e24b541c99650017ec3546cfdc55221dc0617b2f3ff7616f35cb5525f4d3` |

Program hash (`solana-verify`): `4c9dff9951c4b9d236d31d9659248475ed10c160d77085dee4bbf87e40a53285`.

## Lift

From a clean checkout, with Docker, `solana-verify` 0.5.2+ and elan installed, and
nothing else (the lift clones the pinned qedsvm revision on first use):

```sh
cargo build --release && bin/qedgen lift \
  --program crates/qedgen/tests/fixtures/pinocchio-fixtures/label-vault
```

`qedgen build` refuses a
dirty tree, so run it from a commit where this directory is checked in. The gate
`crates/qedgen/tests/label_lift_gate.rs` does the same from a temporary repository and
checks that the rebuilt pair matches the hashes above.
