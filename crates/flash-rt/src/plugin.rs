//! Dynamic plugin loader: finds the shared library, validates the ABI version, plugin metadata
//! and the exported function table against what the compiler recorded, then dispatches calls.
//!
//! Plugins are *trusted native code*: validation catches mistakes (wrong ABI, missing or
//! mismatched functions, tampered file when an integrity pin is present) but cannot make
//! malicious code safe.

use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use flash_plugin_sdk::{tag, FlashFn, FlashPluginInfo, FlashValue, FreeBufFn, ABI_VERSION, ENTRY_SYMBOL};
use sha2::{Digest, Sha256};

struct Func {
    name: String,
    ptr: FlashFn,
    ret_tag: u32,
    nparams: u32,
}

struct Loaded {
    _lib: libloading::Library,
    funcs: Vec<Func>,
    free_buf: FreeBufFn,
    name: String,
}

static PLUGINS: Mutex<Vec<Loaded>> = Mutex::new(Vec::new());

fn fatal(msg: &str) -> ! {
    crate::flush_out();
    eprintln!("Error: {}", msg);
    std::process::exit(1);
}

/// Parameter/return token → expected tag.
fn tag_of(token: &str) -> Option<u32> {
    Some(match token {
        "bool" => tag::BOOL,
        "f64" => tag::F64,
        "str" => tag::STR,
        "none" => tag::NONE,
        t if t.starts_with('i') && t[1..].parse::<u32>().is_ok() => tag::I64,
        t if t.starts_with('u') && t[1..].parse::<u32>().is_ok() => tag::U64,
        _ => return None,
    })
}

pub fn library_file_name(base: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{}.dll", base)
    } else if cfg!(target_os = "macos") {
        format!("lib{}.dylib", base)
    } else {
        format!("lib{}.so", base)
    }
}

fn search_dirs(manifest_dir: &str) -> Vec<PathBuf> {
    let mut v = vec![];
    if let Some(p) = std::env::var_os("FLASH_PLUGIN_PATH") {
        v.extend(std::env::split_paths(&p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            v.push(d.to_path_buf());
            v.push(d.join("plugins"));
        }
    }
    v.push(PathBuf::from(manifest_dir));
    v
}

fn sha256_hex(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    let d = Sha256::digest(&bytes);
    d.iter().map(|b| format!("{:02x}", b)).collect()
}

pub fn load(desc: &str) {
    let mut name = String::new();
    let mut abi = 0u32;
    let mut lib = String::new();
    let mut dir = String::new();
    let mut integrity = String::from("-");
    let mut funcs: Vec<(String, Vec<u32>, u32, bool)> = vec![];
    for line in desc.lines().skip(1) {
        let parts: Vec<&str> = line.split('\t').collect();
        match parts.as_slice() {
            ["name", n] => name = n.to_string(),
            ["abi", a] => abi = a.parse().unwrap_or(0),
            ["lib", l] => lib = l.to_string(),
            ["dir", d] => dir = d.to_string(),
            ["integrity", i] => integrity = i.to_string(),
            ["fn", fname, params, ret, _fallible] => {
                let pt: Vec<u32> = if *params == "-" {
                    vec![]
                } else {
                    params.split(',').map(|t| tag_of(t).unwrap_or(u32::MAX)).collect()
                };
                funcs.push((fname.to_string(), pt, tag_of(ret).unwrap_or(u32::MAX), *_fallible == "1"));
            }
            _ => {}
        }
    }
    if abi != ABI_VERSION {
        fatal(&format!("plugin '{}' was compiled against plugin ABI {}, but this runtime implements ABI {}", name, abi, ABI_VERSION));
    }
    let file = library_file_name(&lib);
    let dirs = search_dirs(&dir);
    let Some(path) = dirs.iter().map(|d| d.join(&file)).find(|p| p.is_file()) else {
        let tried: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        fatal(&format!(
            "cannot find the plugin library '{}' for plugin '{}'.\nSearched: {}\nSet FLASH_PLUGIN_PATH to the folder that contains it.",
            file,
            name,
            tried.join(", ")
        ));
    };
    let digest = sha256_hex(&path);
    if integrity != "-" {
        let want = integrity.trim_start_matches("sha256:");
        if !want.eq_ignore_ascii_case(&digest) {
            fatal(&format!(
                "integrity check failed for plugin '{}': {} has sha256 {} but the program pins {}",
                name,
                path.display(),
                digest,
                want
            ));
        }
    }
    // SAFETY: loading a native library runs its initialisers. Plugins are trusted code.
    let library = unsafe { libloading::Library::new(&path) }
        .unwrap_or_else(|e| fatal(&format!("cannot load plugin library {}: {}", path.display(), e)));
    let entry: libloading::Symbol<unsafe extern "C" fn() -> *const FlashPluginInfo> = unsafe { library.get(ENTRY_SYMBOL.as_bytes()) }
        .unwrap_or_else(|_| fatal(&format!("{} does not export the entry point '{}': it is not a Flash plugin for ABI {}", path.display(), ENTRY_SYMBOL, ABI_VERSION)));
    let info = unsafe { entry() };
    if info.is_null() {
        fatal(&format!("plugin '{}' returned no metadata", name));
    }
    let info = unsafe { &*info };
    if info.abi_version != ABI_VERSION {
        fatal(&format!("plugin '{}' reports ABI version {}, expected {}", name, info.abi_version, ABI_VERSION));
    }
    if (info.struct_size as usize) < std::mem::size_of::<FlashPluginInfo>() {
        fatal(&format!("plugin '{}' has a metadata block that is too small ({} bytes)", name, info.struct_size));
    }
    let pname = unsafe { CStr::from_ptr(info.name) }.to_string_lossy().into_owned();
    let pver = unsafe { CStr::from_ptr(info.version) }.to_string_lossy().into_owned();
    if pname != name {
        fatal(&format!("the library {} identifies itself as plugin '{}', but the program expects '{}'", path.display(), pname, name));
    }
    let table = unsafe { std::slice::from_raw_parts(info.funcs, info.nfuncs as usize) };
    let mut resolved = vec![];
    for (fname, ptags, rtag, _) in &funcs {
        let Some(fi) = table.iter().find(|f| unsafe { CStr::from_ptr(f.name) }.to_str() == Ok(fname.as_str())) else {
            fatal(&format!("plugin '{}' does not export the function '{}' that the program uses", name, fname));
        };
        let actual: Vec<u32> = (0..fi.nparams as usize).map(|i| unsafe { *fi.param_tags.add(i) }).collect();
        if &actual != ptags || fi.ret_tag != *rtag {
            fatal(&format!(
                "plugin '{}': the signature of '{}' does not match its manifest (plugin has params {:?} -> {}, manifest says {:?} -> {})",
                name, fname, actual, fi.ret_tag, ptags, rtag
            ));
        }
        resolved.push(Func { name: fname.clone(), ptr: fi.func, ret_tag: *rtag, nparams: fi.nparams });
    }
    if std::env::var_os("FLASH_PLUGIN_QUIET").is_none() {
        eprintln!(
            "[flash] warning: loaded native plugin '{}' v{} from {} (sha256 {}). Native plugins run with this program's full privileges; only use plugins you trust.",
            name,
            pver,
            path.display(),
            &digest[..16.min(digest.len())]
        );
    }
    PLUGINS.lock().unwrap().push(Loaded { _lib: library, funcs: resolved, free_buf: info.free_buf, name });
}

/// Convert a plugin-allocated string into a Flash String and release the plugin's buffer.
unsafe fn take_string(v: &FlashValue, free_buf: FreeBufFn) -> *mut u8 {
    let bytes = if v.a == 0 { &[][..] } else { std::slice::from_raw_parts(v.a as *const u8, v.b as usize) };
    let s = crate::new_str(bytes);
    if v.a != 0 {
        free_buf(v.a as *mut u8, v.b);
    }
    s
}

pub unsafe fn call(pidx: usize, fidx: usize, args: *const FlashValue, ret: *mut FlashValue) -> i64 {
    let (fnptr, ret_tag, free_buf, nparams) = {
        let g = PLUGINS.lock().unwrap();
        let p = &g[pidx];
        let f = &p.funcs[fidx];
        (f.ptr, f.ret_tag, p.free_buf, f.nparams)
    };
    *ret = FlashValue::NONE;
    let status = fnptr(args, nparams, ret);
    if status != 0 {
        // error: ret carries a plugin-allocated message
        let msg = if (*ret).tag == tag::STR { take_string(&*ret, free_buf) } else { crate::new_str(b"the plugin reported an error") };
        *ret = FlashValue { tag: tag::STR, reserved: 0, a: msg as u64, b: 0 };
        return status as i64;
    }
    let wrong = |ret: *mut FlashValue| {
        *ret = FlashValue { tag: tag::STR, reserved: 0, a: crate::new_str(b"the plugin returned a value of the wrong type") as u64, b: 0 };
        -1
    };
    match ret_tag {
        tag::STR => {
            if (*ret).tag != tag::STR {
                return wrong(ret);
            }
            let s = take_string(&*ret, free_buf);
            *ret = FlashValue { tag: tag::STR, reserved: 0, a: s as u64, b: 0 };
        }
        tag::NONE => {}
        t => {
            if (*ret).tag != t {
                return wrong(ret);
            }
        }
    }
    0
}
