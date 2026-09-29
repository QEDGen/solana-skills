# sBPF v3 JMP32 fixture (#429). Two guards on the first account's data:
# a tag check that compares only the low 32 bits (immediate source) and an
# order check between two fields (register source).

.equ TAG_OFF, 96 # First account data: u64 tag.
.equ LIMIT_OFF, 104 # First account data: u64 limit.
.equ TAG, 1 # Expected tag (low 32 bits).
.equ E_TAG, 1 # Tag mismatch.
.equ E_ORDER, 2 # Limit is below the tag.

.globl entrypoint
entrypoint:
    ldxdw r2, [r1 + TAG_OFF]
    ldxdw r3, [r1 + LIMIT_OFF]
    jne32 r2, TAG, e_tag
    jlt32 r3, r2, e_order
    mov64 r0, 0
    exit

e_tag:
    mov32 r0, E_TAG
    exit

e_order:
    mov32 r0, E_ORDER
    exit
