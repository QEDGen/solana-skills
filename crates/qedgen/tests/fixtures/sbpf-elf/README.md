# sBPF ELF version fixtures

Two builds of `examples/sbpf/counter/src/counter.s`, made with `sbpf` 0.3.1:

```bash
sbpf build -a v0 -i examples/sbpf/counter/src/counter.s -d <dir>   # counter-v0.so, e_flags = 0
sbpf build -a v3 -i examples/sbpf/counter/src/counter.s -d <dir>   # counter-v3.so, e_flags = 3
```

`readiness --so` and `check-upgrade --new-so` read only the ELF header, so these
small programs are enough to test the sBPF v3 rule (#426).

`counter-v3.so` has no section headers, so it has no `.text` to compare. The
build self-check (`qedgen build`, #449) uses a `cargo build-sbf` program instead:

- `anchor-v3.so`: the deploy `.so` of an `anchor init` template program (Anchor
  2.0.0-rc.1), built with `cargo build-sbf --arch v3` and platform-tools v1.57.
- `anchor-v3-debug.so`: the same file with two small dummy debug sections added.
  Its `.text` is unchanged:

```bash
printf 'dbg' > dbg.bin
llvm-objcopy --add-section .debug_info=dbg.bin --add-section .debug_line=dbg.bin \
  anchor-v3.so anchor-v3-debug.so
```
