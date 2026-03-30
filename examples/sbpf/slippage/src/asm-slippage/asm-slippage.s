# Source: https://github.com/blueshift-gg/sbpf
.equ TOKEN_ACCOUNT_BALANCE, 0x00a0
.equ MINIMUM_BALANCE, 0x2918

.globl entrypoint
entrypoint:
    ldxdw r3, [r1+MINIMUM_BALANCE]
    ldxdw r4, [r1+TOKEN_ACCOUNT_BALANCE]
    jge r3, r4, end // minimum_balance required is greater than token account balance avialable i.e r3 >= r4
    exit
end:
    lddw r1, e              // Load error message address
    lddw r2, 17             // Load length of error message
    call sol_log_           // Log out error message -- this is the pattern to use sol_log_ the syscall reads directly from the register
    lddw r0, 1              // Return error code 1
    exit
.rodata
    e: .ascii "Slippage exceeded"
