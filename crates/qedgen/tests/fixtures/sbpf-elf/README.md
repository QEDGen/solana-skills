# sBPF ELF version fixtures

Two builds of `examples/sbpf/counter/src/counter.s`, made with `sbpf` 0.3.1:

```bash
sbpf build -a v0 -i examples/sbpf/counter/src/counter.s -d <dir>   # counter-v0.so, e_flags = 0
sbpf build -a v3 -i examples/sbpf/counter/src/counter.s -d <dir>   # counter-v3.so, e_flags = 3
```

`readiness --so` and `check-upgrade --new-so` read only the ELF header, so these
small programs are enough to test the sBPF v3 rule (#426).
