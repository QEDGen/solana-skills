//! Compile-only stand-in for the `kani` crate.
//!
//! Generated Kani harnesses (`tests/kani.rs`) are `#![cfg(kani)]`, so plain
//! `cargo test` compiles them to nothing — type errors in harness bodies
//! ship silently (#294). Real `cargo kani` needs the full toolchain, which
//! the primary CI job does not install. This stub closes the gap: a gate
//! injects it as a dev-dependency named `kani` and runs
//! `cargo rustc --test kani -- --cfg kani`, so ordinary rustc type-checks
//! the exact generated artifact.
//!
//! The API mirrors only what qedgen's codegen emits (`any`, `assume`,
//! `cover!`, `Arbitrary`, and the `proof` / `unwind` / `solver`
//! attributes). Nothing here is executable — every value-producing path
//! panics. Extend it when codegen starts emitting a new `kani::` API, and
//! keep it honest: type signatures must match the real crate so the gate
//! neither over- nor under-accepts.

pub use qedgen_kani_compile_stub_macros::{proof, solver, unwind, Arbitrary};

/// Mirror of `kani::Arbitrary` (the `any_array` half is omitted — codegen
/// never calls it directly; arrays go through the blanket impl below).
pub trait Arbitrary: Sized {
    fn any() -> Self;
}

macro_rules! impl_arbitrary_primitive {
    ($($ty:ty),* $(,)?) => {
        $(impl Arbitrary for $ty {
            fn any() -> Self {
                unimplemented!("kani compile stub is not executable")
            }
        })*
    };
}

impl_arbitrary_primitive!(
    bool, char, u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize, f32, f64
);

impl<T: Arbitrary, const N: usize> Arbitrary for [T; N] {
    fn any() -> Self {
        unimplemented!("kani compile stub is not executable")
    }
}

impl<T: Arbitrary> Arbitrary for Option<T> {
    fn any() -> Self {
        unimplemented!("kani compile stub is not executable")
    }
}

/// Mirror of `kani::any`.
pub fn any<T: Arbitrary>() -> T {
    T::any()
}

/// Mirror of `kani::assume`.
pub fn assume(_cond: bool) {}

/// Mirror of `kani::cover!`. Evaluates nothing at runtime; the arguments
/// are still type-checked (condition must be `bool`).
#[macro_export]
macro_rules! cover {
    () => {};
    ($cond:expr $(,)?) => {
        let _: bool = $cond;
    };
    ($cond:expr, $msg:expr $(,)?) => {
        let _: bool = $cond;
        let _: &str = $msg;
    };
}
