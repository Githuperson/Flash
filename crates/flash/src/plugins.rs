//! Plugin manifests (`*.flashplugin.toml`) and the descriptor embedded into executables.
//! See PLUGIN_ABI.md.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::ast::PluginKind;
use crate::hir::{PluginBinding, PluginFunc};
use crate::types::{IntTy, Type};

pub const SUPPORTED_ABI: u32 = 1;

#[derive(Deserialize)]
struct Manifest {
    plugin: PluginSection,
    #[serde(default)]
    function: Vec<FunctionSection>,
}

#[derive(Deserialize)]
struct PluginSection {
    name: String,
    kind: String,
    abi_version: u32,
    library: Option<String>,
    #[allow(dead_code)]
    version: Option<String>,
    integrity: Option<String>,
}

#[derive(Deserialize)]
struct FunctionSection {
    name: String,
    #[serde(default)]
    params: Vec<String>,
    returns: Option<String>,
    #[serde(default)]
    fallible: bool,
}

pub fn manifest_file_name(name: &str) -> String {
    format!("{}.flashplugin.toml", name)
}

/// Directories searched for plugin manifests, in order. `plugins/` folders are searched up to
/// two levels deep (so `plugins/rust/fast_math/` is found from the repository root).
pub fn search_dirs(source_dir: &Path) -> Vec<PathBuf> {
    let mut bases = vec![source_dir.to_path_buf()];
    if let Ok(cwd) = std::env::current_dir() {
        bases.push(cwd);
    }
    let mut dirs: Vec<PathBuf> = vec![];
    for b in &bases {
        dirs.push(b.clone());
        let p = b.join("plugins");
        dirs.push(p.clone());
        if let Ok(rd) = std::fs::read_dir(&p) {
            for e in rd.flatten() {
                if e.path().is_dir() {
                    dirs.push(e.path());
                    if let Ok(rd2) = std::fs::read_dir(e.path()) {
                        for e2 in rd2.flatten() {
                            if e2.path().is_dir() {
                                dirs.push(e2.path());
                            }
                        }
                    }
                }
            }
        }
    }
    if let Some(p) = std::env::var_os("FLASH_PLUGIN_PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    dirs.dedup();
    dirs
}

pub fn find_manifest(name: &str, source_dir: &Path) -> Option<PathBuf> {
    let file = manifest_file_name(name);
    search_dirs(source_dir).into_iter().map(|d| d.join(&file)).find(|p| p.is_file())
}

fn parse_type(s: &str) -> Result<Type, String> {
    if let Some(i) = IntTy::from_name(s) {
        return Ok(Type::Int(i));
    }
    match s {
        "Float" | "f64" => Ok(Type::Float),
        "Bool" => Ok(Type::Bool),
        "String" => Ok(Type::Str),
        "None" => Ok(Type::None),
        other => Err(format!(
            "'{}' is not an ABI-safe type. Plugin functions may use Int, i8..i64, u8..u64, Float, Bool, String and None.",
            other
        )),
    }
}

pub fn load_manifest(path: &Path, expected_kind: PluginKind) -> Result<PluginBinding, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    let m: Manifest = toml::from_str(&text).map_err(|e| format!("invalid plugin manifest {}: {}", path.display(), e))?;
    let kind = match m.plugin.kind.as_str() {
        "rust" => PluginKind::Rust,
        "cpp" => PluginKind::Cpp,
        k => return Err(format!("{}: unknown plugin kind '{}' (expected \"rust\" or \"cpp\")", path.display(), k)),
    };
    if kind != expected_kind {
        let want = if expected_kind == PluginKind::Rust { "rust_plugin" } else { "cpp_plugin" };
        return Err(format!(
            "{} declares kind \"{}\" but was loaded with `use {}`. Use the matching `use` keyword.",
            path.display(),
            m.plugin.kind,
            want
        ));
    }
    if m.plugin.abi_version != SUPPORTED_ABI {
        return Err(format!(
            "{} requires plugin ABI version {}, but this compiler supports version {}.",
            path.display(),
            m.plugin.abi_version,
            SUPPORTED_ABI
        ));
    }
    let mut funcs = vec![];
    for f in &m.function {
        let mut params = vec![];
        for p in &f.params {
            let t = parse_type(p).map_err(|e| format!("{}: function {}: {}", path.display(), f.name, e))?;
            if t == Type::None {
                return Err(format!("{}: function {}: parameters cannot be None", path.display(), f.name));
            }
            params.push(t);
        }
        let ret = match &f.returns {
            Some(r) => parse_type(r).map_err(|e| format!("{}: function {}: {}", path.display(), f.name, e))?,
            None => Type::None,
        };
        funcs.push(PluginFunc { name: f.name.clone(), params, ret, fallible: f.fallible });
    }
    let dir = path.parent().unwrap_or(Path::new(".")).canonicalize().unwrap_or_else(|_| PathBuf::from("."));
    Ok(PluginBinding {
        name: m.plugin.name.clone(),
        kind,
        manifest_dir: dir,
        library: m.plugin.library.unwrap_or(m.plugin.name),
        abi_version: m.plugin.abi_version,
        integrity: m.plugin.integrity,
        funcs,
    })
}

fn type_token(t: &Type) -> String {
    match t {
        Type::Int(i) => format!("{}{}", if i.signed { "i" } else { "u" }, i.bits),
        Type::Float => "f64".into(),
        Type::Bool => "bool".into(),
        Type::Str => "str".into(),
        _ => "none".into(),
    }
}

/// Text embedded in the executable; the runtime parses it to locate and validate the plugin.
pub fn descriptor(p: &PluginBinding) -> String {
    let mut s = String::new();
    s.push_str("FLASHPLUGIN 1\n");
    s.push_str(&format!("name\t{}\n", p.name));
    s.push_str(&format!("abi\t{}\n", p.abi_version));
    s.push_str(&format!("lib\t{}\n", p.library));
    s.push_str(&format!("dir\t{}\n", p.manifest_dir.display()));
    s.push_str(&format!("integrity\t{}\n", p.integrity.as_deref().unwrap_or("-")));
    for f in &p.funcs {
        let params: Vec<String> = f.params.iter().map(type_token).collect();
        s.push_str(&format!(
            "fn\t{}\t{}\t{}\t{}\n",
            f.name,
            if params.is_empty() { "-".to_string() } else { params.join(",") },
            type_token(&f.ret),
            if f.fallible { 1 } else { 0 }
        ));
    }
    s
}
