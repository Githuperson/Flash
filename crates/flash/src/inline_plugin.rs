//! Plugins written inside a Flash program:
//!
//! ```text
//! when using rust plugin "mathx":
//!     fn add(a: i64, b: i64) -> i64 { a + b }
//!
//! when using cpp plugin "text":
//!     std::string shout(std::string s) { return s + "!"; }
//! ```
//!
//! The block is compiled with `rustc` or the system C++ compiler into a shared library, next to a
//! generated manifest, and then loaded exactly like a plugin that was built by hand
//! (`use rust_plugin "mathx"`). The function signatures are read from the code. Builds are cached
//! by a hash of the code, so unchanged blocks are not compiled again.
//!
//! Rust blocks are the body of `flash_plugin! { functions { ... } }`: only `fn` items. C++ blocks
//! may contain `#include`s, helper types and `static` functions; every other function defined at
//! the top level is exported.

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::ast::PluginKind;

const SDK_SOURCE: &str = include_str!("../../flash-plugin-sdk/src/lib.rs");
const PLUGIN_H: &str = include_str!("../../../include/flash_plugin.h");
const PLUGIN_HPP: &str = include_str!("../../../include/flash_plugin.hpp");

/// One exported function, with Flash type names ("Int" is spelled "i64").
#[derive(Debug, PartialEq)]
struct Sig {
    name: String,
    params: Vec<String>,
    ret: Option<String>,
    fallible: bool,
    /// the C++ types, for the generated wrapper
    cpp: Vec<String>,
    cpp_ret: String,
}

/// Where plugins built from inline code are kept (next to, but outside, the build cache).
fn plugin_root() -> PathBuf {
    match crate::driver::cache_dir() {
        Some(d) => d.with_file_name(format!("{}-plugins", d.file_name().and_then(|n| n.to_str()).unwrap_or("flash"))),
        None => std::env::temp_dir().join("flash-plugins"),
    }
}

fn library_file(name: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("lib{}.dylib", name)
    } else if cfg!(windows) {
        format!("{}.dll", name)
    } else {
        format!("lib{}.so", name)
    }
}

/// Compile the code of an inline plugin (unless a build of exactly this code exists) and return
/// the path of its manifest.
pub fn build(kind: PluginKind, name: &str, code: &str) -> Result<PathBuf, String> {
    if !name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric()) || name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(format!("'{}' can't be a plugin name: use letters, digits and '_' (it becomes the name you call, like {}.add(1, 2)).", name, name));
    }
    let kind_name = if kind == PluginKind::Rust { "rust" } else { "cpp" };
    let sigs = if kind == PluginKind::Rust { rust_signatures(code)? } else { cpp_signatures(code)? };
    if sigs.is_empty() {
        return Err(match kind {
            PluginKind::Rust => "this Rust plugin block defines no functions; write `fn name(arg: i64) -> i64 { ... }`".into(),
            PluginKind::Cpp => "this C++ plugin block defines no exported functions; write a function like `int64_t add(int64_t a, int64_t b) { ... }` (static functions stay private)".into(),
        });
    }
    let mut h = Sha256::new();
    for part in [kind_name, name, code, SDK_SOURCE, PLUGIN_H, PLUGIN_HPP, env!("CARGO_PKG_VERSION")] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    let digest: String = h.finalize().iter().take(8).map(|b| format!("{:02x}", b)).collect();
    let dir = plugin_root().join(format!("{}-{}", name, digest));
    let manifest = dir.join(format!("{}.flashplugin.toml", name));
    let lib = dir.join(library_file(name));
    if manifest.is_file() && lib.is_file() {
        return Ok(manifest);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
    match kind {
        PluginKind::Rust => build_rust(&dir, name, code)?,
        PluginKind::Cpp => build_cpp(&dir, name, code, &sigs)?,
    }
    let mut toml = format!("[plugin]\nname = \"{}\"\nkind = \"{}\"\nabi_version = 1\nversion = \"0.1.0\"\nlibrary = \"{}\"\n", name, kind_name, name);
    for s in &sigs {
        let params: Vec<String> = s.params.iter().map(|p| format!("\"{}\"", p)).collect();
        toml.push_str(&format!("\n[[function]]\nname = \"{}\"\nparams = [{}]\n", s.name, params.join(", ")));
        if let Some(r) = &s.ret {
            toml.push_str(&format!("returns = \"{}\"\n", r));
        }
        if s.fallible {
            toml.push_str("fallible = true\n");
        }
    }
    std::fs::write(&manifest, toml).map_err(|e| format!("cannot write {}: {}", manifest.display(), e))?;
    Ok(manifest)
}

fn run(cmd: &mut Command, what: &str, hint: &str) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("cannot run {} ({}): {}", what, hint, e))?;
    if out.status.success() {
        return Ok(());
    }
    let mut msg = String::from_utf8_lossy(&out.stderr).to_string();
    if msg.trim().is_empty() {
        msg = String::from_utf8_lossy(&out.stdout).to_string();
    }
    Err(format!("{} failed (line numbers below are lines of the plugin block):\n{}", what, msg.trim_end()))
}

fn build_rust(dir: &Path, name: &str, code: &str) -> Result<(), String> {
    std::fs::write(dir.join("sdk.rs"), SDK_SOURCE).map_err(|e| e.to_string())?;
    // The macro opening shares the first line with the user's code so error line numbers match the block.
    let mut src = format!("use flash_plugin_sdk::flash_plugin; flash_plugin! {{ name = \"{}\", version = \"0.1.0\", functions {{ ", name);
    src.push_str(code);
    src.push_str("\n} }\n");
    std::fs::write(dir.join("plugin.rs"), src).map_err(|e| e.to_string())?;
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    run(
        Command::new(&rustc).current_dir(dir).args(["--edition", "2021", "--crate-type", "rlib", "--crate-name", "flash_plugin_sdk", "-O", "--cap-lints", "allow", "sdk.rs", "-o", "libflash_plugin_sdk.rlib"]),
        "rustc (building the plugin SDK)",
        "a Rust plugin needs `rustc` on the PATH",
    )?;
    run(
        Command::new(&rustc).current_dir(dir).args(["--edition", "2021", "--crate-type", "cdylib", "--crate-name", name, "-O", "plugin.rs", "--extern", "flash_plugin_sdk=libflash_plugin_sdk.rlib", "-o"]).arg(library_file(name)),
        "rustc",
        "a Rust plugin needs `rustc` on the PATH",
    )
}

fn build_cpp(dir: &Path, name: &str, code: &str, sigs: &[Sig]) -> Result<(), String> {
    std::fs::write(dir.join("flash_plugin.h"), PLUGIN_H).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("flash_plugin.hpp"), PLUGIN_HPP).map_err(|e| e.to_string())?;
    // the headers come in through `-include`, so the code keeps its own line numbers
    let mut src = String::from(code);
    src.push_str("\nnamespace {\n");
    for s in sigs {
        let mut args = vec![];
        for (i, t) in s.cpp.iter().enumerate() {
            args.push(match s.params[i].as_str() {
                "f64" | "Float" => format!("flash_get_f64(&a[{}])", i),
                "Bool" => format!("a[{}].a != 0", i),
                "String" => format!("std::string(flash::get_str(a[{}]))", i),
                _ => format!("static_cast<{}>(a[{}].a)", t, i),
            });
        }
        let call = format!("::{}({})", s.name, args.join(", "));
        let store = match s.ret.as_deref() {
            None => format!("{}; ret->tag = FLASH_T_NONE;", call),
            Some("Float") => format!("flash_set_f64(ret, {});", call),
            Some("Bool") => format!("flash_set_bool(ret, {});", call),
            Some("String") => format!("flash::set_str(ret, std::string({}));", call),
            Some(t) if t.starts_with('u') => format!("flash_set_u64(ret, static_cast<uint64_t>({}));", call),
            Some(_) => format!("flash_set_i64(ret, static_cast<int64_t>({}));", call),
        };
        src.push_str(&format!(
            "int32_t flash_wrap_{n}(const FlashValue* a, uint32_t, FlashValue* ret) {{\n    (void)a;\n    FLASH_GUARD(ret, {{ {store} return 0; }})\n}}\n",
            n = s.name,
            store = store
        ));
        let tags: Vec<&str> = s.params.iter().map(|p| tag_of(p)).collect();
        src.push_str(&format!(
            "const uint32_t flash_params_{n}[] = {{{t}}};\n",
            n = s.name,
            t = if tags.is_empty() { "0".to_string() } else { tags.join(", ") }
        ));
    }
    src.push_str("const FlashFuncInfo FUNCS[] = {\n");
    for s in sigs {
        src.push_str(&format!(
            "    {{\"{n}\", {k}, {r}, flash_params_{n}, flash_wrap_{n}}},\n",
            n = s.name,
            k = s.params.len(),
            r = s.ret.as_deref().map(tag_of).unwrap_or("FLASH_T_NONE")
        ));
    }
    src.push_str(&format!(
        "}};\nconst FlashPluginInfo INFO = {{FLASH_ABI_VERSION, sizeof(FlashPluginInfo), \"{name}\", \"0.1.0\", sizeof(FUNCS) / sizeof(FUNCS[0]), FUNCS, flash_cpp_free_buf}};\n}}  // namespace\nextern \"C\" FLASH_EXPORT const FlashPluginInfo* flash_plugin_entry_v1(void) {{ return &INFO; }}\n",
        name = name
    ));
    std::fs::write(dir.join("plugin.cpp"), src).map_err(|e| e.to_string())?;
    let cxx = std::env::var("CXX").unwrap_or_else(|_| "c++".into());
    let shared = if cfg!(target_os = "macos") { "-dynamiclib" } else { "-shared" };
    run(
        Command::new(&cxx)
            .current_dir(dir)
            .args(["-std=c++17", "-O2", "-fPIC", "-fvisibility=hidden", "-I.", "-include", "flash_plugin.hpp", "-include", "cstdint", "-include", "string"])
            .arg(shared)
            .arg("plugin.cpp")
            .arg("-o")
            .arg(library_file(name)),
        &cxx,
        "a C++ plugin needs a C++ compiler (set CXX to choose one)",
    )
}

fn tag_of(flash_type: &str) -> &'static str {
    match flash_type {
        "Float" => "FLASH_T_F64",
        "Bool" => "FLASH_T_BOOL",
        "String" => "FLASH_T_STR",
        t if t.starts_with('u') => "FLASH_T_U64",
        _ => "FLASH_T_I64",
    }
}

// ------------------------------------------------------------------ signatures

/// Remove comments and the contents of string/char literals, keeping everything else (and the
/// line structure) so that scanning for braces is safe.
fn blank_literals(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i < b.len() && !(b[i] == '*' && b.get(i + 1) == Some(&'/')) {
                if b[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i += 2;
        } else if c == '"' {
            out.push('"');
            i += 1;
            while i < b.len() && b[i] != '"' {
                if b[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            out.push('"');
            i += 1;
        } else if c == '\'' && b.get(i + 2) == Some(&'\'') {
            out.push_str("' '");
            i += 3;
        } else if c == '\'' && b.get(i + 1) == Some(&'\\') && b.get(i + 3) == Some(&'\'') {
            out.push_str("' '");
            i += 4;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// Split `a: i64, b: &str` at top-level commas.
fn split_top(s: &str) -> Vec<String> {
    let mut parts = vec![];
    let (mut depth, mut cur) = (0i32, String::new());
    for c in s.chars() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        parts.push(cur);
    }
    parts.into_iter().map(|p| p.trim().to_string()).collect()
}

fn rust_type(t: &str, is_ret: bool) -> Option<&'static str> {
    Some(match t.replace(' ', "").as_str() {
        "i8" => "i8",
        "i16" => "i16",
        "i32" => "i32",
        "i64" => "i64",
        "u8" => "u8",
        "u16" => "u16",
        "u32" => "u32",
        "u64" => "u64",
        "f64" => "Float",
        "bool" => "Bool",
        "&str" if !is_ret => "String",
        "String" if is_ret => "String",
        _ => return None,
    })
}

fn rust_signatures(code: &str) -> Result<Vec<Sig>, String> {
    let clean = blank_literals(code);
    let chars: Vec<char> = clean.chars().collect();
    let mut sigs = vec![];
    let (mut depth, mut i) = (0i32, 0usize);
    while i < chars.len() {
        let c = chars[i];
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
        let at_word_start = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
        if depth == 0 && c == 'f' && at_word_start && chars[i..].iter().take(3).collect::<String>() == "fn " {
            // fn name ( params ) [-> ret] {
            let rest: String = chars[i + 3..].iter().collect();
            let open = rest.find('(').ok_or("expected '(' after `fn name`")?;
            let name = rest[..open].trim().to_string();
            let mut level = 0;
            let mut close = None;
            for (k, ch) in rest.char_indices().skip(open) {
                match ch {
                    '(' => level += 1,
                    ')' => {
                        level -= 1;
                        if level == 0 {
                            close = Some(k);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let close = close.ok_or_else(|| format!("the parameter list of `fn {}` is not closed", name))?;
            let after = &rest[close + 1..];
            let brace = after.find('{').ok_or_else(|| format!("`fn {}` has no body", name))?;
            let ret_text = after[..brace].trim().strip_prefix("->").map(|s| s.trim().to_string());
            let mut params = vec![];
            let mut cpp = vec![];
            for p in split_top(&rest[open + 1..close]) {
                let (pn, pt) = p.split_once(':').ok_or_else(|| format!("`fn {}`: parameter '{}' needs a type", name, p))?;
                let t = rust_type(pt.trim(), false).ok_or_else(|| {
                    format!(
                        "`fn {}`: parameter '{}' has type `{}`, which can't be passed from Flash. Use i64 (and the other integer types), f64, bool or &str.",
                        name,
                        pn.trim(),
                        pt.trim()
                    )
                })?;
                params.push(t.to_string());
                cpp.push(String::new());
            }
            let (ret, fallible) = match ret_text.as_deref() {
                None | Some("()") => (None, false),
                Some(r) => {
                    let compact = r.replace(' ', "");
                    let (inner, fallible) = match compact.strip_prefix("Result<").and_then(|s| s.strip_suffix(",String>")) {
                        Some(i) => (i.to_string(), true),
                        None => (compact, false),
                    };
                    if inner == "()" {
                        (None, fallible)
                    } else {
                        let t = rust_type(&inner, true).ok_or_else(|| {
                            format!("`fn {}` returns `{}`, which can't be returned to Flash. Use i64 (and the other integer types), f64, bool, String or Result<T, String>.", name, r)
                        })?;
                        (Some(t.to_string()), fallible)
                    }
                }
            };
            sigs.push(Sig { name, params, ret, fallible, cpp, cpp_ret: String::new() });
        }
        i += 1;
    }
    let mut seen = std::collections::HashSet::new();
    for s in &sigs {
        if !seen.insert(s.name.clone()) {
            return Err(format!("the function `{}` is defined twice in this plugin", s.name));
        }
    }
    Ok(sigs)
}

fn cpp_type(t: &str) -> Option<&'static str> {
    let t = t.replace("const ", " ").replace(['&', '*'], " ");
    let t: Vec<&str> = t.split_whitespace().collect();
    let t = t.join(" ");
    Some(match t.as_str() {
        "int8_t" | "signed char" => "i8",
        "int16_t" | "short" => "i16",
        "int32_t" | "int" => "i32",
        "int64_t" | "long" | "long long" => "i64",
        "uint8_t" => "u8",
        "uint16_t" | "unsigned short" => "u16",
        "uint32_t" | "unsigned" | "unsigned int" => "u32",
        "uint64_t" | "size_t" | "unsigned long" | "unsigned long long" => "u64",
        "double" => "Float",
        "bool" => "Bool",
        "std::string" | "std::string_view" | "string" => "String",
        _ => return None,
    })
}

fn cpp_signatures(code: &str) -> Result<Vec<Sig>, String> {
    // preprocessor lines are not code
    let stripped: String = code.lines().map(|l| if l.trim_start().starts_with('#') { "" } else { l }).collect::<Vec<_>>().join("\n");
    let clean = blank_literals(&stripped);
    let (mut depth, mut head) = (0i32, String::new());
    let mut sigs = vec![];
    for c in clean.chars() {
        match c {
            '{' => {
                if depth == 0 {
                    if let Some(s) = cpp_head(&head)? {
                        sigs.push(s);
                    }
                    head.clear();
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    head.clear();
                }
            }
            ';' if depth == 0 => head.clear(),
            _ if depth == 0 => head.push(c),
            _ => {}
        }
    }
    let mut seen = std::collections::HashSet::new();
    for s in &sigs {
        if !seen.insert(s.name.clone()) {
            return Err(format!("the function `{}` is defined twice in this plugin (overloads can't be exported; rename one or make it `static`)", s.name));
        }
    }
    Ok(sigs)
}

/// The text before a top-level `{`: a function definition, or a class / namespace / etc.
fn cpp_head(head: &str) -> Result<Option<Sig>, String> {
    let head = head.trim();
    let Some(open) = head.find('(') else { return Ok(None) };
    let before = head[..open].trim();
    let words: Vec<&str> = before.split_whitespace().collect();
    if words.iter().any(|w| matches!(*w, "static" | "class" | "struct" | "namespace" | "enum" | "template" | "union" | "typedef" | "using" | "extern")) {
        return Ok(None);
    }
    let Some(name) = words.last().map(|s| s.trim_start_matches(['*', '&'])) else { return Ok(None) };
    if words.len() < 2 && !before.contains(' ') {
        return Ok(None); // a constructor-like or macro use without a return type
    }
    let ret_text = before[..before.rfind(name).unwrap_or(0)].trim().to_string();
    let close = head.rfind(')').ok_or_else(|| format!("the parameter list of `{}` is not closed", name))?;
    let mut params = vec![];
    let mut cpp = vec![];
    for p in split_top(&head[open + 1..close]) {
        if p == "void" {
            continue;
        }
        let mut toks: Vec<&str> = p.split_whitespace().collect();
        if toks.len() < 2 {
            return Err(format!("`{}`: parameter '{}' needs a name", name, p));
        }
        toks.pop();
        let ty = toks.join(" ");
        let fl = cpp_type(&ty).ok_or_else(|| {
            format!(
                "`{}`: parameter type `{}` can't be passed from Flash. Use int64_t (and the other integer types), double, bool or std::string. Mark helpers `static` to keep them out of the plugin's functions.",
                name, ty
            )
        })?;
        params.push(fl.to_string());
        cpp.push(ty.replace("const ", "").replace('&', "").trim().to_string());
    }
    let ret = if ret_text == "void" {
        None
    } else {
        let fl = cpp_type(&ret_text).ok_or_else(|| {
            format!("`{}` returns `{}`, which can't be returned to Flash. Use int64_t (and the other integer types), double, bool, std::string or void. Mark helpers `static` to keep them out of the plugin's functions.", name, ret_text)
        })?;
        Some(fl.to_string())
    };
    Ok(Some(Sig { name: name.to_string(), params, ret, fallible: false, cpp, cpp_ret: ret_text }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_signatures_are_read() {
        let s = rust_signatures("fn add(a: i64, b: i64) -> i64 { a + b }\nfn shout(t: &str) -> String { format!(\"{}!\", t) }\nfn ok(x: f64) -> Result<f64, String> { Ok(x) }\nfn hi() { }").unwrap();
        assert_eq!(s.len(), 4);
        assert_eq!(s[0].params, vec!["i64", "i64"]);
        assert_eq!(s[1].ret.as_deref(), Some("String"));
        assert!(s[2].fallible && s[2].ret.as_deref() == Some("Float"));
        assert_eq!(s[3].ret, None);
        assert!(rust_signatures("fn f(v: Vec<i64>) -> i64 { 0 }").is_err());
    }

    #[test]
    fn cpp_signatures_are_read() {
        let code = "#include <cmath>\nstatic int helper(int x) { return x; }\nclass K { public: int v() { return 1; } };\nint64_t add(int64_t a, int64_t b) { return a + b; }\nstd::string shout(const std::string& s) { return s + \"!\"; }\nvoid nothing() {}\n";
        let s = cpp_signatures(code).unwrap();
        let names: Vec<&str> = s.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["add", "shout", "nothing"]);
        assert_eq!(s[1].params, vec!["String"]);
        assert_eq!(s[2].ret, None);
        assert!(cpp_signatures("std::vector<int> f() { return {}; }").is_err());
    }
}
