//! SDK for writing Flash native plugins in Rust (plugin ABI version 1).
//!
//! The ABI is plain C: no Rust `String`/`Vec`/panics cross the boundary. See PLUGIN_ABI.md.
//!
//! ```ignore
//! flash_plugin_sdk::flash_plugin! {
//!     name = "fast_math", version = "0.1.0",
//!     functions {
//!         fn add(a: i64, b: i64) -> i64 { a + b }
//!         fn checked_div(a: i64, b: i64) -> Result<i64, String> {
//!             if b == 0 { Err("division by zero".to_string()) } else { Ok(a / b) }
//!         }
//!     }
//! }
//! ```

use std::os::raw::c_char;

pub const ABI_VERSION: u32 = 1;
pub const ENTRY_SYMBOL: &str = "flash_plugin_entry_v1";

/// Type tags of [`FlashValue`].
pub mod tag {
    pub const NONE: u32 = 0;
    pub const BOOL: u32 = 1;
    pub const I64: u32 = 2;
    pub const U64: u32 = 3;
    pub const F64: u32 = 4;
    /// Pointer + length byte string (UTF-8). Borrowed when passed *to* a plugin; allocated by
    /// the plugin (and released with `free_buf`) when returned *from* it.
    pub const STR: u32 = 5;
}

/// A tagged value crossing the plugin boundary (24 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FlashValue {
    pub tag: u32,
    pub reserved: u32,
    /// i64 / u64 / f64 bits / bool (0 or 1) / string pointer
    pub a: u64,
    /// string length
    pub b: u64,
}

impl FlashValue {
    pub const NONE: FlashValue = FlashValue { tag: tag::NONE, reserved: 0, a: 0, b: 0 };
}

pub type FlashFn = unsafe extern "C" fn(args: *const FlashValue, nargs: u32, ret: *mut FlashValue) -> i32;
pub type FreeBufFn = unsafe extern "C" fn(ptr: *mut u8, len: u64);

#[repr(C)]
pub struct FlashFuncInfo {
    /// NUL-terminated function name.
    pub name: *const c_char,
    pub nparams: u32,
    pub ret_tag: u32,
    pub param_tags: *const u32,
    pub func: FlashFn,
}

#[repr(C)]
pub struct FlashPluginInfo {
    pub abi_version: u32,
    pub struct_size: u32,
    pub name: *const c_char,
    pub version: *const c_char,
    pub nfuncs: u32,
    pub funcs: *const FlashFuncInfo,
    pub free_buf: FreeBufFn,
}

/// Wrapper that lets plugin metadata live in a `static`.
pub struct SyncCell<T>(pub T);
unsafe impl<T> Sync for SyncCell<T> {}

/// Free a buffer previously returned by a plugin function (see `FlashPluginInfo::free_buf`).
///
/// # Safety
/// `ptr`/`len` must come from a string returned by this plugin and not be freed twice.
pub unsafe extern "C" fn free_buf(ptr: *mut u8, len: u64) {
    if !ptr.is_null() {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len as usize)));
    }
}

/// Conversion of an incoming argument.
pub trait FromFlash<'a>: Sized {
    const TAG: u32;
    fn from_flash(v: &'a FlashValue) -> Result<Self, String>;
}

impl<'a> FromFlash<'a> for i64 {
    const TAG: u32 = tag::I64;
    fn from_flash(v: &'a FlashValue) -> Result<Self, String> {
        if v.tag == tag::I64 { Ok(v.a as i64) } else { Err("expected an Int argument".into()) }
    }
}
impl<'a> FromFlash<'a> for u64 {
    const TAG: u32 = tag::U64;
    fn from_flash(v: &'a FlashValue) -> Result<Self, String> {
        if v.tag == tag::U64 { Ok(v.a) } else { Err("expected a u64 argument".into()) }
    }
}
impl<'a> FromFlash<'a> for f64 {
    const TAG: u32 = tag::F64;
    fn from_flash(v: &'a FlashValue) -> Result<Self, String> {
        if v.tag == tag::F64 { Ok(f64::from_bits(v.a)) } else { Err("expected a Float argument".into()) }
    }
}
impl<'a> FromFlash<'a> for bool {
    const TAG: u32 = tag::BOOL;
    fn from_flash(v: &'a FlashValue) -> Result<Self, String> {
        if v.tag == tag::BOOL { Ok(v.a != 0) } else { Err("expected a Bool argument".into()) }
    }
}
impl<'a> FromFlash<'a> for &'a str {
    const TAG: u32 = tag::STR;
    fn from_flash(v: &'a FlashValue) -> Result<Self, String> {
        if v.tag != tag::STR {
            return Err("expected a String argument".into());
        }
        // SAFETY: the host guarantees ptr/len describe a live byte buffer for the call.
        let bytes = unsafe { std::slice::from_raw_parts(v.a as *const u8, v.b as usize) };
        std::str::from_utf8(bytes).map_err(|_| "String argument is not valid UTF-8".to_string())
    }
}

fn write_string(s: String, out: &mut FlashValue) {
    let b = s.into_bytes().into_boxed_slice();
    let len = b.len() as u64;
    let ptr = Box::into_raw(b) as *mut u8;
    *out = FlashValue { tag: tag::STR, reserved: 0, a: ptr as u64, b: len };
}

/// Conversion of a successful return value.
pub trait IntoFlash {
    const TAG: u32;
    fn into_flash(self, out: &mut FlashValue);
}
impl IntoFlash for i64 {
    const TAG: u32 = tag::I64;
    fn into_flash(self, out: &mut FlashValue) {
        *out = FlashValue { tag: tag::I64, reserved: 0, a: self as u64, b: 0 };
    }
}
impl IntoFlash for u64 {
    const TAG: u32 = tag::U64;
    fn into_flash(self, out: &mut FlashValue) {
        *out = FlashValue { tag: tag::U64, reserved: 0, a: self, b: 0 };
    }
}
impl IntoFlash for f64 {
    const TAG: u32 = tag::F64;
    fn into_flash(self, out: &mut FlashValue) {
        *out = FlashValue { tag: tag::F64, reserved: 0, a: self.to_bits(), b: 0 };
    }
}
impl IntoFlash for bool {
    const TAG: u32 = tag::BOOL;
    fn into_flash(self, out: &mut FlashValue) {
        *out = FlashValue { tag: tag::BOOL, reserved: 0, a: self as u64, b: 0 };
    }
}
impl IntoFlash for () {
    const TAG: u32 = tag::NONE;
    fn into_flash(self, out: &mut FlashValue) {
        *out = FlashValue::NONE;
    }
}
impl IntoFlash for String {
    const TAG: u32 = tag::STR;
    fn into_flash(self, out: &mut FlashValue) {
        write_string(self, out);
    }
}

/// Return type of a plugin function: plain value or `Result<T, String>`.
pub trait Ret {
    const TAG: u32;
    fn write(self, out: &mut FlashValue) -> i32;
}
impl<T: IntoFlash> Ret for T {
    const TAG: u32 = T::TAG;
    fn write(self, out: &mut FlashValue) -> i32 {
        self.into_flash(out);
        0
    }
}
impl<T: IntoFlash> Ret for Result<T, String> {
    const TAG: u32 = T::TAG;
    fn write(self, out: &mut FlashValue) -> i32 {
        match self {
            Ok(v) => {
                v.into_flash(out);
                0
            }
            Err(e) => {
                write_string(e, out);
                1
            }
        }
    }
}

#[doc(hidden)]
pub fn report_error(msg: &str, out: &mut FlashValue) -> i32 {
    write_string(msg.to_string(), out);
    2
}

#[doc(hidden)]
pub const fn count(xs: &[&str]) -> usize {
    xs.len()
}

/// Declare a Flash plugin. See the crate documentation.
#[macro_export]
macro_rules! flash_plugin {
    (
        name = $name:literal, version = $ver:literal,
        functions { $( fn $fname:ident ( $( $arg:ident : $aty:ty ),* ) -> $ret:ty $body:block )* }
    ) => {
        $(
            #[allow(non_snake_case, unused_variables, unused_mut, unused_assignments)]
            mod $fname {
                use super::*;
                pub fn call<'a>( $( $arg : $aty ),* ) -> $ret $body
                pub static PARAMS: [u32; $crate::count(&[$( stringify!($arg) ),*])] =
                    [ $( <$aty as $crate::FromFlash<'static>>::TAG ),* ];
                pub unsafe extern "C" fn wrapper(
                    args: *const $crate::FlashValue, nargs: u32, ret: *mut $crate::FlashValue,
                ) -> i32 {
                    let ret = &mut *ret;
                    *ret = $crate::FlashValue::NONE;
                    if nargs as usize != PARAMS.len() {
                        return $crate::report_error("wrong number of arguments", ret);
                    }
                    let args = std::slice::from_raw_parts(args, nargs as usize);
                    // A panic must never unwind into the host: turn it into an error status.
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<$ret, String> {
                        let mut i = 0usize;
                        $(
                            let $arg: $aty = $crate::FromFlash::from_flash(&args[i])?;
                            i += 1;
                        )*
                        Ok(call( $( $arg ),* ))
                    }));
                    match outcome {
                        Ok(Ok(v)) => $crate::Ret::write(v, ret),
                        Ok(Err(msg)) => $crate::report_error(&msg, ret),
                        Err(_) => $crate::report_error("the plugin function panicked", ret),
                    }
                }
            }
        )*

        static __FLASH_FUNCS: $crate::SyncCell<[$crate::FlashFuncInfo; [$( stringify!($fname) ),*].len()]> =
            $crate::SyncCell([
                $(
                    $crate::FlashFuncInfo {
                        name: concat!(stringify!($fname), "\0").as_ptr() as *const std::os::raw::c_char,
                        nparams: $fname::PARAMS.len() as u32,
                        ret_tag: <$ret as $crate::Ret>::TAG,
                        param_tags: &$fname::PARAMS as *const [u32] as *const u32,
                        func: $fname::wrapper,
                    }
                ),*
            ]);

        static __FLASH_INFO: $crate::SyncCell<$crate::FlashPluginInfo> = $crate::SyncCell($crate::FlashPluginInfo {
            abi_version: $crate::ABI_VERSION,
            struct_size: std::mem::size_of::<$crate::FlashPluginInfo>() as u32,
            name: concat!($name, "\0").as_ptr() as *const std::os::raw::c_char,
            version: concat!($ver, "\0").as_ptr() as *const std::os::raw::c_char,
            nfuncs: [$( stringify!($fname) ),*].len() as u32,
            funcs: &__FLASH_FUNCS.0 as *const [$crate::FlashFuncInfo] as *const $crate::FlashFuncInfo,
            free_buf: $crate::free_buf,
        });

        /// Versioned entry point looked up by the Flash loader.
        #[no_mangle]
        pub extern "C" fn flash_plugin_entry_v1() -> *const $crate::FlashPluginInfo {
            &__FLASH_INFO.0
        }
    };
}
