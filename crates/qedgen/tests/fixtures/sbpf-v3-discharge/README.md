# sBPF v3 discharge fixture

`vault_v3.so` is qedsvm's `vault.so` program (add 1 to the u64 at input
offset 32) assembled as sBPF v3 (`e_flags = 3`) with `sbpf` 0.3.1:

```bash
sbpf build -a v3 -i src/vault_v3/vault_v3.s -d <dir>
```

SHA-256: `97b763b986ba642fb9d0ea6aef6700659276261689559944cc09dca5b6690f94`.
The build is reproducible.

`tests/discharge_e2e.rs` runs `qedgen discharge` on it with qedsvm's
`vault.codama.json` and expects `verified` for `total += 1` and `rejected`
for `total += 2`. The program is straight-line with no syscalls, so the
single-path discharge needs no trace.
