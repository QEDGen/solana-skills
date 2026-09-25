//! sBPF v3 bug class: a stack buffer overrun that no longer faults.
//!
//! `outer` owns a 64-byte buffer and passes it to `inner`, which writes `n`
//! bytes into it. Frames are 4 KiB and a callee's frame sits above its
//! caller's. Writing past the end of `outer`'s frame reaches `inner`'s own
//! frame. Under V0 with stack frame gaps, that write hits an unmapped gap and
//! faults. Under v3 there are no gaps, so it silently overwrites `inner`'s
//! live locals.
//!
//! Instruction data: `n: u64`. Returns 1 when `inner`'s guard was changed.
#![no_std]

#[inline(never)]
fn inner(buffer: *mut u8, n: usize) -> u64 {
    let guard = [0x41u8; 64];
    core::hint::black_box(&guard);
    for i in 0..n {
        unsafe { buffer.add(i).write_volatile(0xAA) };
    }
    if core::hint::black_box(&guard).iter().any(|b| *b != 0x41) {
        1
    } else {
        0
    }
}

#[inline(never)]
fn outer(n: usize) -> u64 {
    let mut buffer = [0u8; 64];
    let result = inner(buffer.as_mut_ptr(), n);
    core::hint::black_box(&buffer);
    result
}

#[no_mangle]
pub extern "C" fn entrypoint(input: *mut u8) -> u64 {
    // Input with no accounts: num_accounts (u64), data_len (u64), data.
    let n = unsafe { core::ptr::read_unaligned(input.add(16) as *const u64) } as usize;
    outer(n)
}

#[cfg(target_os = "solana")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
