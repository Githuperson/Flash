// Rust code linked statically into a Flash program.
//
// Linking *two* Rust static libraries (this one and Flash's own runtime) into one executable
// normally produces duplicate-symbol errors, so this library is `#![no_std]` and built with
// `panic=abort`: it contains only the code below and `core`. Its panic handler still clashes with
// the one in Flash's runtime, so link with ``-Wl,--allow-multiple-definition` (macOS: `-Wl,-allow_duplicate_symbols`; the first definition,
// the runtime's, wins). For anything that needs `std`, build the library as a dynamic plugin
// instead (see PLUGIN_ABI.md).
#![no_std]

#[no_mangle]
pub extern "C" fn rs_square(x: i64) -> i64 {
    x.wrapping_mul(x)
}

#[no_mangle]
pub extern "C" fn rs_clamp(x: i64, lo: i64, hi: i64) -> i64 {
    if x < lo {
        lo
    } else if x > hi {
        hi
    } else {
        x
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // Panics must not unwind into Flash; abort the process instead.
    loop {}
}
