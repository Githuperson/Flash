//! Compilation driver: loads source files and imports, runs the stages, links executables.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::ast::{Item, PluginKind};
use crate::diag::{self, Diagnostic, Diagnostics};
use crate::hir::{PluginBinding, Program};
use crate::module::LoadedModule;
use crate::source::{SourceMap, Span};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Linker {
    /// Built-in when possible (Linux x86-64, Cranelift, no `--link`/`extern fn`), else the system linker.
    #[default]
    Auto,
    /// Link with `cc` (needs a C compiler/linker).
    System,
    /// No external tools: the executable is the prebuilt `flash-stub` with the program appended.
    Builtin,
}

#[derive(Clone, Debug, Default)]
pub struct BuildOptions {
    pub linker: Linker,
    pub release: bool,
    pub target: Option<String>,
    pub output: Option<PathBuf>,
    pub emit_ir: bool,
    /// Only produce the object file (needed when cross-compiling without a cross linker).
    pub emit_obj_only: bool,
    pub backend: crate::codegen::Backend,
    /// Extra Flash source appended to the root module (used by `flash test`).
    pub source_suffix: Option<String>,
    /// Reuse a previously linked executable when nothing relevant changed (see `cache_key`).
    pub no_cache: bool,
    /// Extra linker arguments for native libraries (`--link path`, `-l name`, `-L dir`).
    pub link_args: Vec<String>,
}

pub struct Frontend {
    pub sources: SourceMap,
    pub program: Program,
}

pub fn stdlib_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FLASH_STD") {
        return Some(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut d = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..4 {
            if let Some(dir) = &d {
                let c = dir.join("stdlib");
                if c.is_dir() {
                    return Some(c);
                }
                d = dir.parent().map(|p| p.to_path_buf());
            }
        }
    }
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../stdlib");
    dev.is_dir().then_some(dev)
}

struct Loader {
    sources: SourceMap,
    diags: Diagnostics,
    mods: Vec<LoadedModule>,
    by_path: HashMap<PathBuf, usize>,
    plugins: Vec<PluginBinding>,
    in_progress: Vec<PathBuf>,
    privileged: bool,
}

impl Loader {
    fn load(&mut self, path: &Path, alias: String, suffix: Option<&str>, import_span: Option<Span>) -> Option<usize> {
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Some(i) = self.by_path.get(&canon) {
            return Some(*i);
        }
        if self.in_progress.contains(&canon) {
            if let Some(sp) = import_span {
                self.diags.push(
                    Diagnostic::error(sp, "These modules import each other in a circle.").help("Move the shared code into a third module."),
                );
            }
            return None;
        }
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                let msg = format!("cannot read '{}': {}", path.display(), e);
                match import_span {
                    Some(sp) => self.diags.push(Diagnostic::error(sp, msg)),
                    None => self.diags.push(Diagnostic::error_nospan(msg)),
                }
                return None;
            }
        };
        self.load_text(path, text, alias, suffix)
    }

    fn load_text(&mut self, path: &Path, mut text: String, alias: String, suffix: Option<&str>) -> Option<usize> {
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Some(s) = suffix {
            text.push('\n');
            text.push_str(s);
        }
        let display = path.display().to_string();
        let file = self.sources.add(canon.clone(), display.trim_start_matches("./").to_string(), text.clone());
        let ast = crate::parser::parse_module(&text, file, &mut self.diags);
        let idx = self.mods.len();
        self.mods.push(LoadedModule { alias, ast, imports: vec![], plugins: vec![], privileged: self.privileged });
        self.by_path.insert(canon.clone(), idx);
        self.in_progress.push(canon.clone());
        let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let items: Vec<Item> = self.mods[idx].ast.items.clone();
        for it in &items {
            match it {
                Item::Import(imp) => {
                    let alias = imp.path.last().cloned().unwrap_or_default();
                    let mut file_path = if imp.path[0] == "std" {
                        match stdlib_dir() {
                            Some(d) => d,
                            None => {
                                self.diags.push(
                                    Diagnostic::error(imp.span, "Cannot find the Flash standard library folder.")
                                        .help("Set FLASH_STD to the `stdlib` directory."),
                                );
                                continue;
                            }
                        }
                    } else {
                        dir.clone()
                    };
                    let rel: &[String] = if imp.path[0] == "std" { &imp.path[1..] } else { &imp.path[..] };
                    if rel.is_empty() {
                        self.diags.push(Diagnostic::error(imp.span, "Write `import std.name` to import a standard-library module."));
                        continue;
                    }
                    for seg in rel {
                        file_path.push(seg);
                    }
                    file_path.set_extension("flash");
                    if !file_path.exists() && imp.path[0] != "std" {
                        // Packages installed by flashpkg are made visible through FLASH_PATH.
                        if let Some(paths) = std::env::var_os("FLASH_PATH") {
                            let mut rel_path = PathBuf::new();
                            for seg in rel {
                                rel_path.push(seg);
                            }
                            rel_path.set_extension("flash");
                            if let Some(found) = std::env::split_paths(&paths).map(|d| d.join(&rel_path)).find(|p| p.exists()) {
                                file_path = found;
                            }
                        }
                    }
                    if !file_path.exists() {
                        self.diags.push(
                            Diagnostic::error(imp.span, format!("Cannot find the module '{}'.", imp.path.join(".")))
                                .help(format!("I looked for the file {}.", file_path.display())),
                        );
                        continue;
                    }
                    if let Some(mi) = self.load(&file_path, alias.clone(), None, Some(imp.span)) {
                        self.mods[idx].imports.push((alias, mi));
                    }
                }
                Item::UsePlugin(pd) => {
                    let Some(manifest) = crate::plugins::find_manifest(&pd.name, &dir) else {
                        let searched: Vec<String> = crate::plugins::search_dirs(&dir).iter().map(|d| d.display().to_string()).collect();
                        self.diags.push(
                            Diagnostic::error(
                                pd.span,
                                format!("Cannot find the plugin manifest '{}'.", crate::plugins::manifest_file_name(&pd.name)),
                            )
                            .help(format!("Searched: {}. Set FLASH_PLUGIN_PATH to add folders.", searched.join(", "))),
                        );
                        continue;
                    };
                    let pi = match self.plugins.iter().position(|p| p.name == pd.name) {
                        Some(i) => i,
                        None => match crate::plugins::load_manifest(&manifest, pd.kind) {
                            Ok(b) => {
                                self.plugins.push(b);
                                self.plugins.len() - 1
                            }
                            Err(e) => {
                                self.diags.push(Diagnostic::error(pd.span, e));
                                continue;
                            }
                        },
                    };
                    self.mods[idx].plugins.push((pd.name.clone(), pi));
                    let _ = PluginKind::Rust;
                }
                _ => {}
            }
        }
        self.in_progress.pop();
        Some(idx)
    }
}

/// Run all front-end stages. On failure returns the fully rendered diagnostics.
pub fn frontend(path: &Path, suffix: Option<&str>) -> Result<Frontend, String> {
    frontend_impl(path, None, suffix, false)
}

/// Front end for the runtime library: like `frontend_from_source`, but the `__` intrinsics are allowed.
pub fn frontend_runtime(name: &str, text: &str) -> Result<Frontend, String> {
    frontend_impl(Path::new(name), Some(text.to_string()), None, true)
}

/// Front end for source text that is not stored in a file (used by tests and tools).
/// `name` is shown in diagnostics; imports resolve relative to the current directory.
pub fn frontend_from_source(name: &str, text: &str) -> Result<Frontend, String> {
    frontend_impl(Path::new(name), Some(text.to_string()), None, false)
}

fn frontend_impl(path: &Path, text: Option<String>, suffix: Option<&str>, privileged: bool) -> Result<Frontend, String> {
    let mut l = Loader {
        sources: SourceMap::default(),
        diags: Diagnostics::default(),
        mods: vec![],
        by_path: HashMap::new(),
        plugins: vec![],
        in_progress: vec![],
        privileged,
    };
    let alias = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match text {
        Some(t) => {
            l.load_text(path, t, alias, suffix);
        }
        None => {
            l.load(path, alias, suffix, None);
        }
    }
    let fail = |l: &Loader| diag::render_all(&l.sources, &l.diags);
    if l.diags.has_errors() || l.mods.is_empty() {
        return Err(fail(&l));
    }
    crate::resolve::resolve(&l.mods, &l.plugins, &mut l.diags);
    if l.diags.has_errors() {
        return Err(fail(&l));
    }
    let program = crate::check::check_program(&l.mods, &l.plugins, &mut l.diags);
    if l.diags.has_errors() {
        return Err(fail(&l));
    }
    let program = program.ok_or_else(|| fail(&l))?;
    crate::borrowck::check(&program, &mut l.diags);
    if l.diags.has_errors() {
        return Err(fail(&l));
    }
    // Warnings (if any) are rendered by the caller via the Err path only; keep quiet here.
    Ok(Frontend { sources: l.sources, program })
}

pub fn lower_and_optimise(program: &Program, release: bool) -> crate::ir::Module {
    let mut m = crate::lower::lower(program);
    crate::opt::optimise(&mut m, release);
    m
}

pub fn exe_suffix(target: Option<&str>) -> &'static str {
    let t = target.map(|t| t.to_string()).unwrap_or_else(|| std::env::consts::OS.to_string());
    if t.contains("windows") {
        ".exe"
    } else {
        ""
    }
}

#[cfg(feature = "legacy-backends")]
mod legacy_link {
    use super::*;
    pub fn exe_dir() -> Option<PathBuf> {
        std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf()))
    }

    pub fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    pub fn runtime_lib_name() -> &'static str {
        if cfg!(windows) {
            "flash_rt.lib"
        } else {
            "libflash_rt.a"
        }
    }

    /// Locate the runtime static library, building it from source in a development checkout if needed.
    pub fn find_runtime(release: bool) -> Result<PathBuf, String> {
        if let Some(p) = std::env::var_os("FLASH_RT_LIB") {
            return Ok(PathBuf::from(p));
        }
        let name = runtime_lib_name();
        let mut candidates = vec![];
        if let Some(d) = exe_dir() {
            candidates.push(d.join(name));
            // installed layout: <home>/bin/flash and <home>/lib/libflash_rt.a
            candidates.push(d.join("../lib").join(name));
            if let Some(parent) = d.parent() {
                // tests run from target/debug/deps
                candidates.push(parent.join(name));
            }
        }
        if let Some(c) = candidates.iter().find(|c| c.is_file()) {
            return Ok(c.clone());
        }
        // Development checkout: build the runtime crate on demand.
        let root = workspace_root();
        if root.join("crates/flash-rt/Cargo.toml").is_file() {
            let mut cmd = Command::new("cargo");
            cmd.current_dir(&root).args(["build", "-p", "flash-rt", "--quiet"]);
            if release {
                cmd.arg("--release");
            }
            let status = cmd.status().map_err(|e| format!("cannot run cargo to build the runtime: {}", e))?;
            if !status.success() {
                return Err("building the Flash runtime (crates/flash-rt) failed".into());
            }
            let target_dir = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| root.join("target"));
            let p = target_dir.join(if release { "release" } else { "debug" }).join(name);
            if p.is_file() {
                return Ok(p);
            }
        }
        Err(format!("cannot find the Flash runtime library '{}'. Run `cargo build --workspace` first, or set FLASH_RT_LIB.", name))
    }

    pub fn stub_name() -> &'static str {
        if cfg!(windows) {
            "flash-stub.exe"
        } else {
            "flash-stub"
        }
    }

    /// Locate the prebuilt stub executable (next to `flash`, in `../lib`, or built on demand in a dev checkout).
    pub fn find_stub(release: bool) -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("FLASH_STUB") {
            return Some(PathBuf::from(p));
        }
        let name = stub_name();
        if let Some(d) = exe_dir() {
            for c in [d.join(name), d.join("../lib").join(name), d.parent().map(|p| p.join(name)).unwrap_or_default()] {
                if c.is_file() {
                    return Some(c);
                }
            }
        }
        let root = workspace_root();
        if root.join("crates/flash-rt/Cargo.toml").is_file() {
            let mut cmd = Command::new("cargo");
            cmd.current_dir(&root).args(["build", "-p", "flash-rt", "--bin", "flash-stub", "--quiet"]);
            if release {
                cmd.arg("--release");
            }
            if cmd.status().map_or(false, |s| s.success()) {
                let target_dir = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| root.join("target"));
                let p = target_dir.join(if release { "release" } else { "debug" }).join(name);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
        None
    }

    /// Can this build use the built-in linker?
    pub fn builtin_possible(opts: &BuildOptions, program: &Program) -> Result<(), &'static str> {
        if !(cfg!(all(target_os = "linux", target_arch = "x86_64"))) {
            return Err("the built-in linker currently supports Linux x86-64 only");
        }
        if opts.target.is_some() || opts.backend != crate::codegen::Backend::Cranelift {
            return Err("the built-in linker needs the Cranelift backend and the host target");
        }
        if !opts.link_args.is_empty() || !program.externs.is_empty() {
            return Err("`extern fn` and --link need the system linker");
        }
        Ok(())
    }

    /// Write `[stub][object][len][magic]` to `out`.
    pub fn link_builtin(obj: &[u8], out: &Path, release: bool) -> Result<(), String> {
        let stub = find_stub(release).ok_or("cannot find flash-stub (run `cargo build --workspace`, or set FLASH_STUB)")?;
        let mut bytes = std::fs::read(&stub).map_err(|e| format!("cannot read {}: {}", stub.display(), e))?;
        bytes.extend_from_slice(obj);
        bytes.extend_from_slice(&(obj.len() as u64).to_le_bytes());
        bytes.extend_from_slice(b"FLASHPAY");
        let _ = std::fs::remove_file(out);
        std::fs::write(out, &bytes).map_err(|e| format!("cannot write {}: {}", out.display(), e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o755));
        }
        Ok(())
    }

    pub fn link(obj: &Path, out: &Path, opts: &BuildOptions) -> Result<(), String> {
        let rt = find_runtime(opts.release)?;
        let cc = std::env::var("FLASH_CC").or_else(|_| std::env::var("CC")).unwrap_or_else(|_| "cc".into());
        let mut cmd = Command::new(&cc);
        cmd.arg(obj).arg(&rt).arg("-o").arg(out);
        if cfg!(target_os = "linux") {
            cmd.args(["-lpthread", "-ldl", "-lm", "-Wl,--gc-sections"]);
            if opts.release {
                cmd.arg("-s");
            }
        } else if cfg!(target_os = "macos") {
            // No frameworks: Rust's std only needs libSystem, and every extra dylib costs process start-up time.
            // `-dead_strip` is cheap and shrinks dev builds too; release additionally drops local symbols and debug maps.
            cmd.args(["-lm", "-Wl,-dead_strip"]);
            if opts.release {
                cmd.args(["-Wl,-x", "-Wl,-S"]);
            }
        }
        for a in &opts.link_args {
            cmd.arg(a);
        }
        let output = cmd.output().map_err(|e| format!("cannot run the linker '{}': {} (install a C compiler or set FLASH_CC)", cc, e))?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            let hint = if err.contains("undefined reference") || err.contains("Undefined symbols") {
                "\nHint: a function declared with `extern fn` was not found. Pass the library that defines it, for example `--link path/to/libname.a` (or -lname -Ldir), and check the function name.\n"
            } else {
                ""
            };
            return Err(format!("linking failed:\n{}{}", err, hint));
        }
        Ok(())
    }

}
#[cfg(feature = "legacy-backends")]
use legacy_link::*;

pub fn default_output(src: &Path, opts: &BuildOptions) -> PathBuf {
    if let Some(o) = &opts.output {
        return o.clone();
    }
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "a".into());
    let dir = src.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    dir.join(format!("{}{}", stem, exe_suffix(opts.target.as_deref())))
}

pub struct BuildTimings {
    pub frontend: std::time::Duration,
    pub backend: std::time::Duration,
    pub link: std::time::Duration,
    /// The executable came from the build cache.
    pub cached: bool,
}

/// Directory of the build cache: `$FLASH_CACHE_DIR`, else `$XDG_CACHE_HOME/flash`, else `~/.cache/flash`.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("FLASH_CACHE_DIR") {
        return Some(PathBuf::from(d));
    }
    if let Some(d) = std::env::var_os("XDG_CACHE_HOME") {
        return Some(PathBuf::from(d).join("flash"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache").join("flash"))
}

/// Keep only the `keep` most recently written cache entries.
fn prune_cache(dir: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<(std::time::SystemTime, PathBuf)> =
        rd.flatten().filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).collect();
    if entries.len() <= keep {
        return;
    }
    entries.sort();
    for (_, p) in entries.iter().take(entries.len() - keep) {
        let _ = std::fs::remove_file(p);
    }
}

fn file_stamp(p: &Path) -> String {
    match std::fs::metadata(p) {
        Ok(m) => {
            let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
            format!("{}:{}", m.len(), t)
        }
        Err(_) => "missing".into(),
    }
}

/// Hash of everything that influences the produced executable: all source texts, the options, the
/// plugin signatures, and the identity of the compiler and runtime library.
fn cache_key(fe: &Frontend, opts: &BuildOptions) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(crate::VERSION.as_bytes());
    h.update(file_stamp(&std::env::current_exe().unwrap_or_default()).as_bytes());
    #[cfg(feature = "legacy-backends")]
    if let Ok(rt) = find_runtime(opts.release) {
        h.update(rt.to_string_lossy().as_bytes());
        h.update(file_stamp(&rt).as_bytes());
    }
    h.update(
        format!("{}|{:?}|{:?}|{:?}|{:?}", opts.release, opts.target, opts.backend, std::env::var("FLASH_CC").ok(), opts.link_args)
            .as_bytes(),
    );
    for a in &opts.link_args {
        // linked archives may change without their path changing
        h.update(file_stamp(Path::new(a)).as_bytes());
    }
    for f in &fe.sources.files {
        h.update(f.display.as_bytes());
        h.update((f.text.len() as u64).to_le_bytes());
        h.update(f.text.as_bytes());
    }
    for p in &fe.program.plugins {
        h.update(crate::plugins::descriptor(p).as_bytes());
    }
    h.finalize().iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(feature = "legacy-backends")]
fn legacy_llvm_available(opts: &BuildOptions) -> bool {
    opts.release && opts.target.is_none() && crate::llvm::find_clang().is_some()
}
#[cfg(not(feature = "legacy-backends"))]
fn legacy_llvm_available(_: &BuildOptions) -> bool {
    false
}

/// Turn `Backend::Auto` into a concrete backend. Flash's own native back end is used whenever it
/// can handle the program (Linux x86-64 host, no C libraries, no plugins); otherwise the optional
/// legacy back ends are used: LLVM for release builds when clang exists, else Cranelift.
fn resolve_backend(opts: &BuildOptions, program: &Program) -> crate::codegen::Backend {
    use crate::codegen::Backend;
    match opts.backend {
        Backend::Auto => {
            let native_ok = native_target(opts).is_ok()
                && opts.linker == Linker::Auto
                && opts.link_args.is_empty()
                && program.externs.is_empty()
                && program.plugins.is_empty();
            if native_ok {
                Backend::Native
            } else if legacy_llvm_available(opts) {
                Backend::Llvm
            } else {
                Backend::Cranelift
            }
        }
        b => b,
    }
}

/// Compile `src` into a native executable at the returned path.
pub fn build(src: &Path, opts: &BuildOptions) -> Result<(PathBuf, BuildTimings), String> {
    let t0 = std::time::Instant::now();
    let fe = frontend(src, opts.source_suffix.as_deref())?;
    let mut resolved = opts.clone();
    resolved.backend = resolve_backend(opts, &fe.program);
    let opts = &resolved;
    // Build cache: a hit skips lowering, code generation and linking.
    let cache_entry =
        if opts.no_cache || opts.emit_ir || (opts.emit_obj_only && opts.backend != crate::codegen::Backend::Native) { None } else { cache_dir().map(|d| d.join(cache_key(&fe, opts))) };
    let out = default_output(src, opts);
    if let Some(entry) = &cache_entry {
        if entry.is_file() && std::fs::copy(entry, &out).is_ok() {
            let t = t0.elapsed();
            return Ok((out, BuildTimings { frontend: t, backend: Default::default(), link: Default::default(), cached: true }));
        }
    }
    if opts.backend == crate::codegen::Backend::Native {
        return build_native(&fe, opts, &out, cache_entry.as_deref(), t0);
    }
    build_legacy(&fe, opts, &out, cache_entry.as_deref(), t0)
}

#[cfg(feature = "legacy-backends")]
fn build_legacy(
    fe: &Frontend,
    opts: &BuildOptions,
    out: &Path,
    cache_entry: Option<&Path>,
    t0: std::time::Instant,
) -> Result<(PathBuf, BuildTimings), String> {
    let out = out.to_path_buf();
    let cache_entry = cache_entry.map(|p| p.to_path_buf());
    let module = lower_and_optimise(&fe.program, opts.release);
    if opts.emit_ir {
        println!("{}", module);
    }
    let t1 = std::time::Instant::now();
    let cg_opts = crate::codegen::CodegenOptions { release: opts.release, target: opts.target.clone(), backend: opts.backend };
    // The code generator panics on unsupported targets; report that as an ordinary error.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let compiled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::codegen::compile(&module, &cg_opts)));
    std::panic::set_hook(prev_hook);
    let obj_bytes = match compiled {
        Ok(r) => r?,
        Err(_) => {
            return Err(format!(
                "the code generator failed for target '{}' (this target is probably not supported by the Cranelift backend yet)\n",
                opts.target.as_deref().unwrap_or("host")
            ))
        }
    };
    let t2 = std::time::Instant::now();
    let obj_path = if opts.emit_obj_only { out.clone() } else { out.with_extension("flash.o") };
    if opts.emit_obj_only {
        std::fs::write(&obj_path, &obj_bytes).map_err(|e| format!("cannot write {}: {}", obj_path.display(), e))?;
        return Ok((obj_path, BuildTimings { frontend: t1 - t0, backend: t2 - t1, link: Default::default(), cached: false }));
    }
    let use_builtin = match (opts.linker, builtin_possible(opts, &fe.program)) {
        (Linker::System, _) => false,
        (Linker::Builtin, Err(why)) => return Err(format!("--linker builtin: {}\n", why)),
        (Linker::Builtin, Ok(())) => true,
        (Linker::Auto, r) => r.is_ok() && find_stub(opts.release).is_some(),
    };
    if use_builtin {
        link_builtin(&obj_bytes, &out, opts.release)?;
    } else {
        std::fs::write(&obj_path, &obj_bytes).map_err(|e| format!("cannot write {}: {}", obj_path.display(), e))?;
        let r = link(&obj_path, &out, opts);
        let _ = std::fs::remove_file(&obj_path);
        r?;
    }
    let t3 = std::time::Instant::now();
    if let Some(entry) = &cache_entry {
        // Best effort: a failure to cache never fails the build.
        if let Some(parent) = entry.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = entry.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::copy(&out, &tmp).is_ok() && std::fs::rename(&tmp, entry).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        if let Some(dir) = entry.parent() {
            prune_cache(dir, 64);
        }
    }
    Ok((out, BuildTimings { frontend: t1 - t0, backend: t2 - t1, link: t3 - t2, cached: false }))
}

#[cfg(not(feature = "legacy-backends"))]
fn build_legacy(
    _fe: &Frontend,
    opts: &BuildOptions,
    _out: &Path,
    _cache_entry: Option<&Path>,
    _t0: std::time::Instant,
) -> Result<(PathBuf, BuildTimings), String> {
    if let Err(e) = native_target(opts) {
        return Err(format!("{}\n", e));
    }
    Err(format!(
        "this build of Flash contains only the native back end (Linux x86-64, no `extern fn`, no plugins){}.\nThe optional Cranelift/LLVM back ends are compiled in with `cargo build --features legacy-backends`.\n",
        match opts.target.as_deref() {
            Some(t) => format!(", so it cannot target '{}'", t),
            None => String::new(),
        }
    ))
}

/// The target the native back end generates code for: `--target`, else the machine we run on.
fn native_target(opts: &BuildOptions) -> Result<crate::native::Target, String> {
    match &opts.target {
        Some(t) => crate::native::Target::parse(t),
        None => crate::native::Target::host()
            .ok_or_else(|| "the native back end runs on Linux and macOS on x86-64 only; pass --target x86_64-unknown-linux-gnu or x86_64-apple-darwin to cross-compile".to_string()),
    }
}

/// Build with Flash's own back end: lower, link with the Flash-language runtime, emit an ELF file.
fn build_native(
    fe: &Frontend,
    opts: &BuildOptions,
    out: &Path,
    cache_entry: Option<&Path>,
    t0: std::time::Instant,
) -> Result<(PathBuf, BuildTimings), String> {
    let target = native_target(opts).map_err(|e| format!("{}\n", e))?;
    if !fe.program.externs.is_empty() || !fe.program.plugins.is_empty() || !opts.link_args.is_empty() {
        return Err("the native back end cannot link C libraries or plugins; use --backend cranelift or llvm for `extern fn`, --link and plugins\n".into());
    }
    let user = crate::lower::lower(&fe.program);
    if opts.emit_ir {
        println!("{}", user);
    }
    let rt = crate::native::link::lower_runtime(target).map_err(|e| format!("internal error in the runtime library:\n{}", e))?;
    let t1 = std::time::Instant::now();
    let module = crate::native::link::link(user, rt, opts.release).map_err(|e| format!("{}\n", e))?;
    let elf = crate::native::compile(module, target).map_err(|e| format!("{}\n", e))?;
    let t2 = std::time::Instant::now();
    std::fs::write(out, &elf).map_err(|e| format!("cannot write {}: {}", out.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("cannot mark {} executable: {}", out.display(), e))?;
    }
    if let Some(entry) = cache_entry {
        if let Some(parent) = entry.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = entry.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::copy(out, &tmp).is_ok() && std::fs::rename(&tmp, entry).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        if let Some(dir) = entry.parent() {
            prune_cache(dir, 64);
        }
    }
    Ok((out.to_path_buf(), BuildTimings { frontend: t1 - t0, backend: t2 - t1, link: Default::default(), cached: false }))
}

/// Type-check only.
pub fn check(src: &Path) -> Result<(), String> {
    frontend(src, None).map(|_| ())
}

// ------------------------------------------------------------------ flash test

/// Names of the `fn test_*()` functions in a source file.
pub fn find_tests(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else { return vec![] };
    let mut diags = Diagnostics::default();
    let m = crate::parser::parse_module(&text, 0, &mut diags);
    m.items
        .iter()
        .filter_map(|it| match it {
            Item::Fn(f) if f.name.starts_with("test_") && f.params.is_empty() => Some(f.name.clone()),
            _ => None,
        })
        .collect()
}

fn collect_flash_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if !name.starts_with('.') && name != "target" && name != "build" {
                collect_flash_files(&p, out);
            }
        } else if p.extension().is_some_and(|e| e == "flash") {
            out.push(p);
        }
    }
}

pub struct TestSummary {
    pub passed: usize,
    pub failed: usize,
    pub compile_errors: usize,
}

/// Compile each test file once and run every `test_*` function in its own process.
pub fn run_tests(target: &Path, release: bool) -> TestSummary {
    let mut files = vec![];
    if target.is_dir() {
        collect_flash_files(target, &mut files);
    } else {
        files.push(target.to_path_buf());
    }
    let mut summary = TestSummary { passed: 0, failed: 0, compile_errors: 0 };
    let tmp = std::env::temp_dir().join(format!("flash-test-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    for file in files {
        let tests = find_tests(&file);
        if tests.is_empty() {
            continue;
        }
        // Dispatch on the first command-line argument: `exe test_name` runs only that test.
        let mut suffix = String::from("__flash_selected = args()\nif len(__flash_selected) > 0:\n");
        for (i, t) in tests.iter().enumerate() {
            suffix.push_str(&format!("    {} __flash_selected[0] == \"{}\":\n        {}()\n", if i == 0 { "if" } else { "elif" }, t, t));
        }
        let exe = tmp.join(format!("{}{}", file.file_stem().unwrap().to_string_lossy(), exe_suffix(None)));
        let opts = BuildOptions { release, output: Some(exe.clone()), source_suffix: Some(suffix), ..Default::default() };
        match build(&file, &opts) {
            Err(e) => {
                eprint!("{}", e);
                println!("{}: FAILED TO COMPILE", file.display());
                summary.compile_errors += 1;
            }
            Ok(_) => {
                for t in tests {
                    let out = Command::new(&exe).arg(&t).output();
                    match out {
                        Ok(o) if o.status.success() => {
                            println!("test {}::{} ... ok", file.display(), t);
                            summary.passed += 1;
                        }
                        Ok(o) => {
                            println!("test {}::{} ... FAILED", file.display(), t);
                            let so = String::from_utf8_lossy(&o.stdout);
                            let se = String::from_utf8_lossy(&o.stderr);
                            if !so.trim().is_empty() {
                                println!("---- stdout ----\n{}", so.trim_end());
                            }
                            if !se.trim().is_empty() {
                                println!("---- stderr ----\n{}", se.trim_end());
                            }
                            summary.failed += 1;
                        }
                        Err(e) => {
                            println!("test {}::{} ... FAILED ({})", file.display(), t, e);
                            summary.failed += 1;
                        }
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    summary
}
