//! sBPF v3 bug class: a read through a null pointer. V0 maps nothing at VM
//! address 0, so the read faults. v3 maps `.rodata` at address 0, so the read
//! returns program bytes and execution continues.
#![no_std]

static CONFIG: [u8; 32] = *b"read-only data at vm address 0..";

#[no_mangle]
pub extern "C" fn entrypoint(_input: *mut u8) -> u64 {
    core::hint::black_box(&CONFIG);
    // Stands in for a pointer the program assumed was valid, such as an
    // unchecked offset that came out as 0.
    let pointer = core::hint::black_box(0usize) as *const u64;
    let value = unsafe { core::ptr::read_volatile(pointer) };
    core::hint::black_box(value);
    0
}

#[cfg(target_os = "solana")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
