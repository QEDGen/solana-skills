# sBPF v3 JMP32 fixture

`src/guard/guard.s` has two 32-bit jumps: `jne32` with an immediate source and
`jlt32` with a register source. `guard-v3.so` is its v3 build (`e_flags = 3`)
with `sbpf` 0.3.1:

```bash
sbpf build -a v3 -i src/guard/guard.s -d <dir>
```

`formal_verification/Program.lean` is the `asm2lean` lift:

```bash
qedgen asm2lean --input src/guard/guard.s \
  --output formal_verification/Program.lean --namespace GuardProg --sbpf-version v3
```

`formal_verification/Spec.lean` proves every exit path, including that the
tag check ignores the high 32 bits. `scripts/check-lake-build.sh` builds it,
and `asm2lean`'s unit tests check that the committed `Program.lean` matches a
fresh lift.
