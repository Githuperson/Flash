//! The prebuilt "stub" executable used by `flash build --linker builtin`.
//!
//! A Flash program built without a system linker is this file with the compiled object code
//! appended (`[stub][object][length: u64][b"FLASHPAY"]`). At start-up the loader in `flash_rt::loader`
//! maps the object into memory, applies its relocations against the runtime functions linked into
//! this executable, and calls `flash_main`. The C `main` comes from the `flash_rt` library.
#![no_main]

#[no_mangle]
pub extern "C" fn flash_main() {
    flash_rt::loader::run_payload();
}

// Make sure the runtime's `main` and `fl_*` symbols are kept.
#[used]
static KEEP: unsafe extern "C" fn(i32, *const *const std::os::raw::c_char) -> i32 = flash_rt::main;
