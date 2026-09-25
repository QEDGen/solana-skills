//! sBPF v3 "unresolved syscall" claim: a hand-declared `extern "C"` syscall.
//!
//! V0 resolves this symbol when the program loads. v3 resolves syscalls at
//! build time, so on platform-tools v1.56+ the v3 link fails with
//! `undefined symbol: sol_log_`. It does not build and then abort at runtime
//! with `call -1`.
#![no_std]

extern "C" {
    fn sol_log_(message: *const u8, len: u64);
}

#[no_mangle]
pub extern "C" fn entrypoint(_input: *mut u8) -> u64 {
    let message = b"hello";
    unsafe { sol_log_(message.as_ptr(), message.len() as u64) };
    0
}

#[cfg(target_os = "solana")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
