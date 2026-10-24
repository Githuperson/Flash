//! Example Flash plugin written in Rust: `use rust_plugin "fast_math"`.
//!
//! Build:  cargo build -p fast_math --release
//! The shared library lands in target/release/libfast_math.{so,dylib} (fast_math.dll on Windows).

use flash_plugin_sdk::flash_plugin;

fn gcd_impl(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

flash_plugin! {
    name = "fast_math", version = "0.1.0",
    functions {
        fn add(a: i64, b: i64) -> i64 { a.wrapping_add(b) }
        fn square(x: f64) -> f64 { x * x }
        fn gcd(a: i64, b: i64) -> i64 { gcd_impl(a.unsigned_abs(), b.unsigned_abs()) as i64 }
        fn is_prime(n: i64) -> bool {
            if n < 2 { return false; }
            let mut i = 2;
            while i * i <= n {
                if n % i == 0 { return false; }
                i += 1;
            }
            true
        }
        fn shout(text: &str) -> String { format!("{}!", text.to_uppercase()) }
        fn checked_div(a: i64, b: i64) -> Result<i64, String> {
            if b == 0 { Err("division by zero in fast_math.checked_div".to_string()) } else { Ok(a / b) }
        }
        fn always_panics() -> i64 { panic!("deliberate panic inside the plugin") }
    }
}
