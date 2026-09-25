# sBPF v3 runtime fixtures

Three small `no_std` programs with no dependencies. Each is built from the
same source as sBPF V0 and as v3, then run in Mollusk. They back the auditor
catalog entries for sBPF v3 (#427). The gate is
`crates/qedgen-sandbox/tests/sbpf_v3_runtime.rs`.

| Program | V0, SIMD-0460 off | V0, SIMD-0460 on | v3 |
|---|---|---|---|
| `low_address`: read through a null pointer | faults | faults | succeeds, reads `.rodata` |
| `stack_overrun`: overrun past the caller's frame into the callee's (`n = 4160`) | faults | corrupts the callee's locals | corrupts the callee's locals |
| `extern_syscall`: hand-declared `extern "C"` syscall | builds, resolves at load | builds, resolves at load | link fails: `undefined symbol` |

SIMD-0460 (Virtual Address Space Adjustments) turns off V0 stack frame gaps
(`enable_stack_frame_gaps: !feature_set.virtual_address_space_adjustments` in
Agave). v3 never has frame gaps.

`stack_overrun` takes `n: u64` as instruction data, the number of bytes
written into `outer`'s 64-byte buffer. Frames are 4 KiB and a callee's frame
sits above its caller's, so an overrun runs into the callee, not the caller.
It returns 1 when the callee's guard bytes changed.

Checked with cargo-build-sbf 4.3.0, platform-tools v1.57, and
`mollusk-svm 0.12.1-agave-4.0`.
