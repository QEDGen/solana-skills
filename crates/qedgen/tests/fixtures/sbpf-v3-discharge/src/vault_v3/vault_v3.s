# qedsvm's vault.so program, assembled as sBPF v3 (#429).
.globl entrypoint
entrypoint:
    ldxdw r2, [r1 + 32]
    add64 r2, 1
    stxdw [r1 + 32], r2
    mov64 r0, 0
    exit
