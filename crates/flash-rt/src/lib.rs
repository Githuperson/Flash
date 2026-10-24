//! Flash runtime library. Linked (as a static library) into every compiled Flash program.
//!
//! Memory layouts (shared with the compiler, see ARCHITECTURE.md):
//! * String: `[len: i64][cap: i64][bytes...]` in one allocation; `cap == -1` marks static data.
//! * List:   `[len: i64][cap: i64][data: *mut u8]`, elements are `stride` bytes each.
//! * Result: `[tag: i64 (0 ok, 1 err)][value: 8 bytes][error message: String*]`.

#![allow(clippy::missing_safety_doc)]

mod plugin;

use std::alloc::{alloc, dealloc, realloc, Layout};
use std::io::{Read, Write};
use std::sync::Mutex;

// ------------------------------------------------------------------ output
static OUT: Mutex<Vec<u8>> = Mutex::new(Vec::new());

fn out_write(bytes: &[u8]) {
    let mut o = OUT.lock().unwrap_or_else(|e| e.into_inner());
    o.extend_from_slice(bytes);
    if o.len() >= 1 << 16 {
        let _ = std::io::stdout().write_all(&o);
        o.clear();
    }
}

pub fn flush_out() {
    let mut o = OUT.lock().unwrap_or_else(|e| e.into_inner());
    if !o.is_empty() {
        let _ = std::io::stdout().write_all(&o);
        o.clear();
    }
    let _ = std::io::stdout().flush();
}

pub fn die(msg: &str) -> ! {
    flush_out();
    eprintln!("Flash runtime error: {}", msg);
    std::process::exit(101);
}

// ------------------------------------------------------------------ memory
/// Number of live `fl_alloc` allocations; reported when FLASH_RT_LEAKCHECK is set.
static LIVE_ALLOCS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

#[no_mangle]
pub unsafe extern "C" fn fl_alloc(size: i64) -> *mut u8 {
    // Plain load/store: the runtime is single-threaded, and a locked read-modify-write per
    // allocation would be measurable in allocation-heavy programs.
    LIVE_ALLOCS.store(LIVE_ALLOCS.load(std::sync::atomic::Ordering::Relaxed) + 1, std::sync::atomic::Ordering::Relaxed);
    let size = (size as usize).max(8);
    let p = alloc(Layout::from_size_align_unchecked(size, 8));
    if p.is_null() {
        die("out of memory");
    }
    p
}

#[no_mangle]
pub unsafe extern "C" fn fl_free(p: *mut u8, size: i64) {
    if !p.is_null() {
        LIVE_ALLOCS.store(LIVE_ALLOCS.load(std::sync::atomic::Ordering::Relaxed) - 1, std::sync::atomic::Ordering::Relaxed);
        dealloc(p, Layout::from_size_align_unchecked((size as usize).max(8), 8));
    }
}

// ----------------------------------------------------------------- strings
unsafe fn str_len(p: *const u8) -> usize {
    *(p as *const i64) as usize
}

pub unsafe fn str_bytes<'a>(p: *const u8) -> &'a [u8] {
    std::slice::from_raw_parts(p.add(16), str_len(p))
}

pub unsafe fn str_ref<'a>(p: *const u8) -> &'a str {
    // Strings are produced from UTF-8 sources; byte-oriented operations may split characters,
    // so fall back to a lossy view only where text is required.
    std::str::from_utf8_unchecked(str_bytes(p))
}

pub fn new_str(b: &[u8]) -> *mut u8 {
    unsafe {
        let p = fl_alloc(16 + b.len() as i64);
        *(p as *mut i64) = b.len() as i64;
        *(p as *mut i64).add(1) = b.len() as i64;
        std::ptr::copy_nonoverlapping(b.as_ptr(), p.add(16), b.len());
        p
    }
}

#[no_mangle]
pub unsafe extern "C" fn fl_str_drop(p: *mut u8) {
    let cap = *(p as *const i64).add(1);
    if cap >= 0 {
        fl_free(p, 16 + cap);
    }
}

#[no_mangle]
pub unsafe extern "C" fn fl_str_clone(p: *const u8) -> *mut u8 {
    new_str(str_bytes(p))
}

#[no_mangle]
pub unsafe extern "C" fn fl_str_concat(a: *const u8, b: *const u8) -> *mut u8 {
    let (x, y) = (str_bytes(a), str_bytes(b));
    let n = x.len() + y.len();
    let p = fl_alloc(16 + n as i64);
    *(p as *mut i64) = n as i64;
    *(p as *mut i64).add(1) = n as i64;
    std::ptr::copy_nonoverlapping(x.as_ptr(), p.add(16), x.len());
    std::ptr::copy_nonoverlapping(y.as_ptr(), p.add(16 + x.len()), y.len());
    p
}

#[no_mangle]
pub unsafe extern "C" fn fl_str_eq(a: *const u8, b: *const u8) -> u8 {
    (str_bytes(a) == str_bytes(b)) as u8
}

#[no_mangle]
pub unsafe extern "C" fn fl_str_cmp(a: *const u8, b: *const u8) -> i64 {
    match str_bytes(a).cmp(str_bytes(b)) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

fn fmt_float(v: f64) -> String {
    format!("{:?}", v)
}

#[no_mangle]
pub extern "C" fn fl_str_from_int(v: i64) -> *mut u8 {
    new_str(v.to_string().as_bytes())
}
#[no_mangle]
pub extern "C" fn fl_str_from_uint(v: i64) -> *mut u8 {
    new_str((v as u64).to_string().as_bytes())
}
#[no_mangle]
pub extern "C" fn fl_str_from_float(v: f64) -> *mut u8 {
    new_str(fmt_float(v).as_bytes())
}
#[no_mangle]
pub extern "C" fn fl_str_from_bool(v: u8) -> *mut u8 {
    new_str(if v != 0 { b"True" } else { b"False" })
}
#[no_mangle]
pub extern "C" fn fl_str_from_none() -> *mut u8 {
    new_str(b"None")
}

// ------------------------------------------------------------------- print
#[no_mangle]
pub extern "C" fn fl_print_int(v: i64) {
    out_write(v.to_string().as_bytes());
}
#[no_mangle]
pub extern "C" fn fl_print_uint(v: i64) {
    out_write((v as u64).to_string().as_bytes());
}
#[no_mangle]
pub extern "C" fn fl_print_float(v: f64) {
    out_write(fmt_float(v).as_bytes());
}
#[no_mangle]
pub extern "C" fn fl_print_bool(v: u8) {
    out_write(if v != 0 { b"True" } else { b"False" });
}
#[no_mangle]
pub unsafe extern "C" fn fl_print_str(p: *const u8) {
    out_write(str_bytes(p));
}
#[no_mangle]
pub extern "C" fn fl_print_none() {
    out_write(b"None");
}
#[no_mangle]
pub extern "C" fn fl_print_space() {
    out_write(b" ");
}
#[no_mangle]
pub extern "C" fn fl_print_nl() {
    out_write(b"\n");
}

// ------------------------------------------------------------------- lists
#[repr(C)]
struct ListHdr {
    len: i64,
    cap: i64,
    data: *mut u8,
}

#[no_mangle]
pub unsafe extern "C" fn fl_list_new(cap: i64, stride: i64) -> *mut u8 {
    let h = fl_alloc(24) as *mut ListHdr;
    let data = if cap > 0 { fl_alloc(cap * stride) } else { std::ptr::null_mut() };
    *h = ListHdr { len: 0, cap: cap.max(0), data };
    h as *mut u8
}

#[no_mangle]
pub unsafe extern "C" fn fl_list_push_slot(l: *mut u8, stride: i64) -> *mut u8 {
    let h = &mut *(l as *mut ListHdr);
    if h.len == h.cap {
        let new_cap = if h.cap == 0 { 4 } else { h.cap * 2 };
        let new_size = (new_cap * stride) as usize;
        h.data = if h.data.is_null() {
            fl_alloc(new_size as i64)
        } else {
            let p = realloc(h.data, Layout::from_size_align_unchecked((h.cap * stride) as usize, 8), new_size);
            if p.is_null() {
                die("out of memory");
            }
            p
        };
        h.cap = new_cap;
    }
    let slot = h.data.add((h.len * stride) as usize);
    h.len += 1;
    slot
}

#[no_mangle]
pub unsafe extern "C" fn fl_list_pop_slot(l: *mut u8, stride: i64) -> *mut u8 {
    let h = &mut *(l as *mut ListHdr);
    if h.len == 0 {
        die("pop() from an empty list");
    }
    h.len -= 1;
    h.data.add((h.len * stride) as usize)
}

#[no_mangle]
pub unsafe extern "C" fn fl_list_free(l: *mut u8, stride: i64) {
    let h = &*(l as *mut ListHdr);
    if !h.data.is_null() {
        fl_free(h.data, h.cap * stride);
    }
    fl_free(l, 24);
}

#[no_mangle]
pub unsafe extern "C" fn fl_list_clone_shallow(l: *const u8, stride: i64) -> *mut u8 {
    let h = &*(l as *const ListHdr);
    let n = fl_list_new(h.len, stride);
    if h.len > 0 {
        std::ptr::copy_nonoverlapping(h.data, (*(n as *mut ListHdr)).data, (h.len * stride) as usize);
    }
    (*(n as *mut ListHdr)).len = h.len;
    n
}

unsafe fn list_of_strings(items: Vec<*mut u8>) -> *mut u8 {
    let l = fl_list_new(items.len() as i64, 8);
    let h = &mut *(l as *mut ListHdr);
    for (i, s) in items.iter().enumerate() {
        *(h.data as *mut *mut u8).add(i) = *s;
    }
    h.len = items.len() as i64;
    l
}

// ------------------------------------------------------------------ panics
#[no_mangle]
pub extern "C" fn fl_panic_bounds(idx: i64, len: i64) {
    die(&format!("list index {} is out of range for a list of length {}", idx, len));
}
#[no_mangle]
pub extern "C" fn fl_panic_divzero() {
    die("division by zero");
}
#[no_mangle]
pub unsafe extern "C" fn fl_unwrap_fail(r: *const u8) {
    let msg = *(r as *const *const u8).add(2);
    die(&format!("unwrap() on an error: {}", String::from_utf8_lossy(str_bytes(msg))));
}
#[no_mangle]
pub unsafe extern "C" fn fl_assert_fail(msg: *const u8) {
    flush_out();
    if msg.is_null() {
        eprintln!("Assertion failed");
    } else {
        eprintln!("Assertion failed: {}", String::from_utf8_lossy(str_bytes(msg)));
    }
    std::process::exit(101);
}
#[no_mangle]
pub unsafe extern "C" fn fl_fatal_error(msg: *const u8) {
    flush_out();
    eprintln!("Error: {}", String::from_utf8_lossy(str_bytes(msg)));
    std::process::exit(1);
}

// -------------------------------------------------------------------- math
macro_rules! math1 {
    ($($n:ident => $f:ident),*) => { $( #[no_mangle] pub extern "C" fn $n(x: f64) -> f64 { x.$f() } )* };
}
math1!(fl_math_sqrt => sqrt, fl_math_floor => floor, fl_math_ceil => ceil, fl_math_sin => sin,
       fl_math_cos => cos, fl_math_tan => tan, fl_math_exp => exp, fl_math_ln => ln, fl_math_abs => abs);
#[no_mangle]
pub extern "C" fn fl_math_pow(a: f64, b: f64) -> f64 {
    a.powf(b)
}

// ------------------------------------------------------------ process / time
static ARGS: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

#[no_mangle]
pub extern "C" fn fl_exit(code: i64) {
    flush_out();
    std::process::exit(code as i32);
}

#[no_mangle]
pub extern "C" fn fl_now_ns() -> i64 {
    use std::time::Instant;
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as i64
}

#[no_mangle]
pub extern "C" fn fl_sleep_ms(ms: i64) {
    flush_out();
    std::thread::sleep(std::time::Duration::from_millis(ms.max(0) as u64));
}

#[no_mangle]
pub unsafe extern "C" fn fl_args() -> *mut u8 {
    let a = ARGS.lock().unwrap_or_else(|e| e.into_inner());
    list_of_strings(a.iter().map(|s| new_str(s)).collect())
}

#[no_mangle]
pub extern "C" fn fl_read_line() -> *mut u8 {
    flush_out();
    let mut buf = Vec::new();
    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    let mut byte = [0u8; 1];
    while let Ok(1) = lock.read(&mut byte) {
        if byte[0] == b'\n' {
            break;
        }
        buf.push(byte[0]);
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    new_str(&buf)
}

// ------------------------------------------------------------ string library
fn lossy(p: *const u8) -> String {
    unsafe { String::from_utf8_lossy(str_bytes(p)).into_owned() }
}

#[no_mangle]
pub unsafe extern "C" fn fl_str_upper(p: *const u8) -> *mut u8 {
    new_str(lossy(p).to_uppercase().as_bytes())
}
#[no_mangle]
pub unsafe extern "C" fn fl_str_lower(p: *const u8) -> *mut u8 {
    new_str(lossy(p).to_lowercase().as_bytes())
}
#[no_mangle]
pub unsafe extern "C" fn fl_str_trim(p: *const u8) -> *mut u8 {
    new_str(lossy(p).trim().as_bytes())
}

fn find_sub(h: &[u8], n: &[u8]) -> Option<usize> {
    if n.is_empty() {
        return Some(0);
    }
    h.windows(n.len()).position(|w| w == n)
}

#[no_mangle]
pub unsafe extern "C" fn fl_str_contains(a: *const u8, b: *const u8) -> u8 {
    find_sub(str_bytes(a), str_bytes(b)).is_some() as u8
}
#[no_mangle]
pub unsafe extern "C" fn fl_str_starts_with(a: *const u8, b: *const u8) -> u8 {
    str_bytes(a).starts_with(str_bytes(b)) as u8
}
#[no_mangle]
pub unsafe extern "C" fn fl_str_ends_with(a: *const u8, b: *const u8) -> u8 {
    str_bytes(a).ends_with(str_bytes(b)) as u8
}
#[no_mangle]
pub unsafe extern "C" fn fl_str_find(a: *const u8, b: *const u8) -> i64 {
    find_sub(str_bytes(a), str_bytes(b)).map_or(-1, |i| i as i64)
}
#[no_mangle]
pub unsafe extern "C" fn fl_str_replace(s: *const u8, from: *const u8, to: *const u8) -> *mut u8 {
    let (s, f, t) = (str_bytes(s), str_bytes(from), str_bytes(to));
    if f.is_empty() {
        return new_str(s);
    }
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with(f) {
            out.extend_from_slice(t);
            i += f.len();
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    new_str(&out)
}
#[no_mangle]
pub unsafe extern "C" fn fl_str_split(s: *const u8, sep: *const u8) -> *mut u8 {
    let (s, sp) = (str_bytes(s), str_bytes(sep));
    let mut parts: Vec<*mut u8> = vec![];
    if sp.is_empty() {
        for ch in String::from_utf8_lossy(s).chars() {
            let mut b = [0u8; 4];
            parts.push(new_str(ch.encode_utf8(&mut b).as_bytes()));
        }
    } else {
        let mut start = 0;
        let mut i = 0;
        while i + sp.len() <= s.len() {
            if &s[i..i + sp.len()] == sp {
                parts.push(new_str(&s[start..i]));
                i += sp.len();
                start = i;
            } else {
                i += 1;
            }
        }
        parts.push(new_str(&s[start..]));
    }
    list_of_strings(parts)
}
#[no_mangle]
pub unsafe extern "C" fn fl_str_join(l: *const u8, sep: *const u8) -> *mut u8 {
    let h = &*(l as *const ListHdr);
    let sp = str_bytes(sep);
    let mut out = Vec::new();
    for i in 0..h.len as usize {
        if i > 0 {
            out.extend_from_slice(sp);
        }
        out.extend_from_slice(str_bytes(*(h.data as *const *const u8).add(i)));
    }
    new_str(&out)
}
#[no_mangle]
pub unsafe extern "C" fn fl_str_substr(s: *const u8, start: i64, len: i64) -> *mut u8 {
    let b = str_bytes(s);
    let st = (start.max(0) as usize).min(b.len());
    let en = st.saturating_add(len.max(0) as usize).min(b.len());
    new_str(&b[st..en])
}
#[no_mangle]
pub unsafe extern "C" fn fl_str_byte_at(s: *const u8, i: i64) -> u8 {
    let b = str_bytes(s);
    if i < 0 || i as usize >= b.len() {
        die(&format!("byte_at index {} is out of range for a string of length {}", i, b.len()));
    }
    b[i as usize]
}

// ------------------------------------------------------------------ Results
unsafe fn mk_result(tag: i64, val: i64, err: *mut u8) -> *mut u8 {
    let r = fl_alloc(24) as *mut i64;
    *r = tag;
    *r.add(1) = val;
    *r.add(2) = err as i64;
    r as *mut u8
}
fn res_err(msg: &str) -> *mut u8 {
    unsafe { mk_result(1, 0, new_str(msg.as_bytes())) }
}
fn res_ok_int(v: i64) -> *mut u8 {
    unsafe { mk_result(0, v, std::ptr::null_mut()) }
}
fn res_ok_str(b: &[u8]) -> *mut u8 {
    unsafe { mk_result(0, new_str(b) as i64, std::ptr::null_mut()) }
}
fn res_ok_none() -> *mut u8 {
    res_ok_int(0)
}

#[no_mangle]
pub unsafe extern "C" fn fl_result_error(r: *const u8) -> *mut u8 {
    if *(r as *const i64) == 0 {
        new_str(b"")
    } else {
        fl_str_clone(*(r as *const *const u8).add(2))
    }
}

#[no_mangle]
pub unsafe extern "C" fn fl_parse_int(s: *const u8) -> *mut u8 {
    let t = lossy(s);
    match t.trim().parse::<i64>() {
        Ok(v) => res_ok_int(v),
        Err(_) => res_err(&format!("'{}' is not a valid Int", t)),
    }
}
#[no_mangle]
pub unsafe extern "C" fn fl_parse_float(s: *const u8) -> *mut u8 {
    let t = lossy(s);
    match t.trim().parse::<f64>() {
        Ok(v) => res_ok_int(v.to_bits() as i64),
        Err(_) => res_err(&format!("'{}' is not a valid Float", t)),
    }
}

// ------------------------------------------------------------- files / paths
fn io_msg(e: &std::io::Error, what: &str, path: &str) -> String {
    use std::io::ErrorKind::*;
    let why = match e.kind() {
        NotFound => "the file does not exist".to_string(),
        PermissionDenied => "permission denied".to_string(),
        AlreadyExists => "it already exists".to_string(),
        _ => e.to_string(),
    };
    format!("cannot {} '{}': {}", what, path, why)
}

#[no_mangle]
pub unsafe extern "C" fn fl_read_file(p: *const u8) -> *mut u8 {
    let path = lossy(p);
    match std::fs::read(&path) {
        Ok(b) => res_ok_str(&b),
        Err(e) => res_err(&io_msg(&e, "read", &path)),
    }
}
#[no_mangle]
pub unsafe extern "C" fn fl_write_file(p: *const u8, t: *const u8) -> *mut u8 {
    let path = lossy(p);
    match std::fs::write(&path, str_bytes(t)) {
        Ok(()) => res_ok_none(),
        Err(e) => res_err(&io_msg(&e, "write", &path)),
    }
}
#[no_mangle]
pub unsafe extern "C" fn fl_append_file(p: *const u8, t: *const u8) -> *mut u8 {
    let path = lossy(p);
    let r = std::fs::OpenOptions::new().append(true).create(true).open(&path).and_then(|mut f| f.write_all(str_bytes(t)));
    match r {
        Ok(()) => res_ok_none(),
        Err(e) => res_err(&io_msg(&e, "append to", &path)),
    }
}
#[no_mangle]
pub unsafe extern "C" fn fl_file_exists(p: *const u8) -> u8 {
    std::path::Path::new(&lossy(p)).exists() as u8
}
#[no_mangle]
pub unsafe extern "C" fn fl_remove_file(p: *const u8) -> *mut u8 {
    let path = lossy(p);
    match std::fs::remove_file(&path) {
        Ok(()) => res_ok_none(),
        Err(e) => res_err(&io_msg(&e, "remove", &path)),
    }
}
#[no_mangle]
pub unsafe extern "C" fn fl_env_var(p: *const u8) -> *mut u8 {
    let name = lossy(p);
    match std::env::var(&name) {
        Ok(v) => res_ok_str(v.as_bytes()),
        Err(_) => res_err(&format!("the environment variable '{}' is not set", name)),
    }
}
#[no_mangle]
pub unsafe extern "C" fn fl_path_join(a: *const u8, b: *const u8) -> *mut u8 {
    let p = std::path::Path::new(&lossy(a)).join(lossy(b));
    new_str(p.to_string_lossy().as_bytes())
}
#[no_mangle]
pub unsafe extern "C" fn fl_path_filename(a: *const u8) -> *mut u8 {
    let s = lossy(a);
    let n = std::path::Path::new(&s).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    new_str(n.as_bytes())
}
#[no_mangle]
pub unsafe extern "C" fn fl_path_parent(a: *const u8) -> *mut u8 {
    let s = lossy(a);
    let n = std::path::Path::new(&s).parent().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    new_str(n.as_bytes())
}
#[no_mangle]
pub unsafe extern "C" fn fl_path_ext(a: *const u8) -> *mut u8 {
    let s = lossy(a);
    let n = std::path::Path::new(&s).extension().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    new_str(n.as_bytes())
}

// ------------------------------------------------------------------ plugins
#[no_mangle]
pub unsafe extern "C" fn fl_plugin_load(desc: *const u8, len: i64) {
    let text = std::slice::from_raw_parts(desc, len as usize);
    plugin::load(&String::from_utf8_lossy(text));
}

#[no_mangle]
pub unsafe extern "C" fn fl_plugin_call(
    plugin_idx: i64,
    func_idx: i64,
    args: *const flash_plugin_sdk::FlashValue,
    ret: *mut flash_plugin_sdk::FlashValue,
) -> i64 {
    plugin::call(plugin_idx as usize, func_idx as usize, args, ret)
}

// ------------------------------------------------------------------- entry
extern "C" {
    fn flash_main();
}

#[cfg(not(test))]
#[no_mangle]
pub unsafe extern "C" fn main(argc: i32, argv: *const *const std::os::raw::c_char) -> i32 {
    {
        let mut a = ARGS.lock().unwrap();
        for i in 1..argc as usize {
            a.push(std::ffi::CStr::from_ptr(*argv.add(i)).to_bytes().to_vec());
        }
    }
    flash_main();
    flush_out();
    if std::env::var_os("FLASH_RT_LEAKCHECK").is_some() {
        eprintln!("[flash] live allocations at exit: {}", LIVE_ALLOCS.load(std::sync::atomic::Ordering::Relaxed));
    }
    0
}
