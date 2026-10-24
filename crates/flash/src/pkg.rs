//! `flashpkg`: a local package manager.
//!
//! Scope of v0.1 (deliberately small and honest):
//! * packages are directories with a `flash.toml`; dependencies are **local paths** only
//!   (there is no registry, no network access and no publishing);
//! * `flash.lock` records every resolved package with a SHA-256 integrity hash of its source tree,
//!   and `flashpkg build` refuses to build if a dependency changed since it was locked;
//! * native plugin packages declare supported OS / CPU / ABI / compiler requirements, which are
//!   checked before use. Build commands of dependencies are never run unless explicitly allowed.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MANIFEST: &str = "flash.toml";
pub const LOCKFILE: &str = "flash.lock";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Manifest {
    pub package: PackageSection,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, DepSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<NativeSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PackageSection {
    pub name: String,
    pub version: String,
    /// "app" (default), "library" or "native-plugin"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepSpec {
    pub path: String,
}

/// Requirements of a native plugin package.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NativeSection {
    /// e.g. ["linux", "macos", "windows"]
    pub supported_os: Vec<String>,
    /// e.g. ["x86_64", "aarch64"]
    pub supported_arch: Vec<String>,
    pub abi_version: u32,
    /// Informational/validated toolchain requirements, e.g. min_rustc = "1.70".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_rustc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cxx_standard: Option<String>,
    /// Command that builds the library (run only with --allow-native-builds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// Directory (relative to the package) that contains the built library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Lock {
    pub version: u32,
    #[serde(default, rename = "package")]
    pub packages: Vec<LockedPackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
    /// "root" or "path+<relative path from the root package>"
    pub source: String,
    pub integrity: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

pub struct Resolved {
    pub name: String,
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub source: String,
}

pub fn valid_name(n: &str) -> bool {
    let mut cs = n.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_lowercase() || c == '_') && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

pub fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let p = dir.join(MANIFEST);
    let text = std::fs::read_to_string(&p).map_err(|e| format!("cannot read {}: {}", p.display(), e))?;
    let m: Manifest = toml::from_str(&text).map_err(|e| format!("invalid {}: {}", p.display(), e))?;
    if !valid_name(&m.package.name) {
        return Err(format!("{}: package name '{}' must be lowercase letters, digits and underscores", p.display(), m.package.name));
    }
    Ok(m)
}

pub fn write_manifest(dir: &Path, m: &Manifest) -> Result<(), String> {
    let text = toml::to_string_pretty(m).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(MANIFEST), text).map_err(|e| e.to_string())
}

/// SHA-256 over the package's files (sorted by relative path), ignoring build output.
pub fn hash_dir(dir: &Path) -> Result<String, String> {
    fn walk(base: &Path, cur: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for e in std::fs::read_dir(cur)? {
            let p = e?.path();
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if p.is_dir() {
                if !matches!(name.as_str(), ".git" | "target" | "build" | ".flash-cache" | "node_modules") {
                    walk(base, &p, out)?;
                }
            } else if name != LOCKFILE && !name.ends_with(".o") {
                out.push(p.strip_prefix(base).unwrap().to_path_buf());
            }
        }
        Ok(())
    }
    let mut files = vec![];
    walk(dir, dir, &mut files).map_err(|e| format!("cannot read {}: {}", dir.display(), e))?;
    files.sort();
    let mut h = Sha256::new();
    for f in files {
        let rel = f.to_string_lossy().replace('\\', "/");
        let data = std::fs::read(dir.join(&f)).map_err(|e| e.to_string())?;
        h.update(rel.as_bytes());
        h.update([0]);
        h.update((data.len() as u64).to_le_bytes());
        h.update(&data);
    }
    Ok(format!("sha256:{}", h.finalize().iter().map(|b| format!("{:02x}", b)).collect::<String>()))
}

/// Resolve the dependency graph of the package in `root_dir` (breadth-first, path dependencies only).
pub fn resolve(root_dir: &Path) -> Result<Vec<Resolved>, String> {
    let root_dir = root_dir.canonicalize().map_err(|e| format!("{}: {}", root_dir.display(), e))?;
    let root = read_manifest(&root_dir)?;
    let mut out: Vec<Resolved> = vec![];
    let mut queue: VecDeque<(PathBuf, Manifest, String)> = VecDeque::new();
    queue.push_back((root_dir.clone(), root, "root".to_string()));
    while let Some((dir, m, source)) = queue.pop_front() {
        if let Some(existing) = out.iter().find(|r| r.name == m.package.name) {
            if existing.dir != dir {
                return Err(format!(
                    "two different packages are both named '{}': {} and {}",
                    m.package.name,
                    existing.dir.display(),
                    dir.display()
                ));
            }
            continue;
        }
        for (dep_name, spec) in &m.dependencies {
            let dep_dir = dir.join(&spec.path).canonicalize().map_err(|e| {
                format!("dependency '{}' of '{}': cannot open {}: {}", dep_name, m.package.name, dir.join(&spec.path).display(), e)
            })?;
            let dm = read_manifest(&dep_dir).map_err(|e| format!("dependency '{}': {}", dep_name, e))?;
            if &dm.package.name != dep_name {
                return Err(format!(
                    "dependency '{}' of '{}' points to a package named '{}'",
                    dep_name, m.package.name, dm.package.name
                ));
            }
            let src = if source == "root" { format!("path+{}", spec.path) } else { format!("path+{} (from {})", spec.path, m.package.name) };
            queue.push_back((dep_dir, dm, src));
        }
        out.push(Resolved { name: m.package.name.clone(), dir, manifest: m, source });
    }
    out.sort_by(|a, b| (a.source != "root", &a.name).cmp(&(b.source != "root", &b.name)));
    Ok(out)
}

pub fn make_lock(pkgs: &[Resolved]) -> Result<Lock, String> {
    let mut packages = vec![];
    for r in pkgs {
        let mut deps: Vec<String> = r.manifest.dependencies.keys().cloned().collect();
        deps.sort();
        packages.push(LockedPackage {
            name: r.name.clone(),
            version: r.manifest.package.version.clone(),
            source: r.source.clone(),
            // The root package is the code being developed; only dependencies are pinned.
            integrity: if r.source == "root" { "root".to_string() } else { hash_dir(&r.dir)? },
            dependencies: deps,
        });
    }
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Lock { version: 1, packages })
}

pub fn write_lock(dir: &Path, lock: &Lock) -> Result<(), String> {
    let mut text = String::from("# This file is generated by `flashpkg install`. Do not edit by hand.\n");
    text.push_str(&toml::to_string_pretty(lock).map_err(|e| e.to_string())?);
    std::fs::write(dir.join(LOCKFILE), text).map_err(|e| e.to_string())
}

pub fn read_lock(dir: &Path) -> Result<Lock, String> {
    let p = dir.join(LOCKFILE);
    let text = std::fs::read_to_string(&p).map_err(|_| format!("there is no {}. Run `flashpkg install` first.", LOCKFILE))?;
    toml::from_str(&text).map_err(|e| format!("invalid {}: {}", p.display(), e))
}

/// Compare the lock file with the current state of the packages.
pub fn verify_lock(root_dir: &Path, pkgs: &[Resolved]) -> Result<(), String> {
    let lock = read_lock(root_dir)?;
    let current = make_lock(pkgs)?;
    let mut problems = vec![];
    for cur in &current.packages {
        match lock.packages.iter().find(|l| l.name == cur.name) {
            None => problems.push(format!("'{}' is not in {} (run `flashpkg install`)", cur.name, LOCKFILE)),
            Some(l) if l.integrity != cur.integrity => problems.push(format!(
                "'{}' has changed since it was locked (locked {}, now {}). If this is expected run `flashpkg install` to update the lock; otherwise do not trust it.",
                cur.name, l.integrity, cur.integrity
            )),
            Some(l) if l.version != cur.version || l.dependencies != cur.dependencies => {
                problems.push(format!("'{}' metadata differs from {}", cur.name, LOCKFILE))
            }
            _ => {}
        }
    }
    for l in &lock.packages {
        if !current.packages.iter().any(|c| c.name == l.name) {
            problems.push(format!("{} lists '{}' which is no longer a dependency", LOCKFILE, l.name));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// Check platform / ABI / toolchain requirements of native plugin packages.
pub fn check_native_requirements(r: &Resolved) -> Result<(), String> {
    let Some(n) = &r.manifest.native else { return Ok(()) };
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    if !n.supported_os.iter().any(|o| o == os) {
        return Err(format!("native package '{}' supports {:?}, but this system is {}", r.name, n.supported_os, os));
    }
    if !n.supported_arch.iter().any(|a| a == arch) {
        return Err(format!("native package '{}' supports CPU architectures {:?}, but this system is {}", r.name, n.supported_arch, arch));
    }
    if n.abi_version != crate::plugins::SUPPORTED_ABI {
        return Err(format!(
            "native package '{}' needs plugin ABI version {}, but this flash supports version {}",
            r.name,
            n.abi_version,
            crate::plugins::SUPPORTED_ABI
        ));
    }
    if let Some(min) = &n.min_rustc {
        if let Ok(out) = std::process::Command::new("rustc").arg("--version").output() {
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            let have = text.split_whitespace().nth(1).unwrap_or("0.0.0");
            if version_lt(have, min) {
                return Err(format!("native package '{}' needs rustc >= {}, found {}", r.name, min, have));
            }
        }
    }
    Ok(())
}

fn version_lt(a: &str, b: &str) -> bool {
    let p = |s: &str| -> Vec<u64> { s.split('.').map(|x| x.parse().unwrap_or(0)).collect() };
    p(a) < p(b)
}

/// Environment variables that make a package's dependencies visible to the compiler and runtime.
pub fn build_env(root_dir: &Path, pkgs: &[Resolved]) -> (std::ffi::OsString, std::ffi::OsString) {
    let mut flash_path: Vec<PathBuf> = vec![];
    let mut plugin_path: Vec<PathBuf> = vec![];
    for r in pkgs {
        if r.dir == root_dir.canonicalize().unwrap_or_else(|_| root_dir.to_path_buf()) {
            continue;
        }
        flash_path.push(r.dir.join("src"));
        flash_path.push(r.dir.clone());
        plugin_path.push(r.dir.clone());
        if let Some(ld) = r.manifest.native.as_ref().and_then(|n| n.library_dir.as_ref()) {
            plugin_path.push(r.dir.join(ld));
        }
    }
    if let Some(existing) = std::env::var_os("FLASH_PLUGIN_PATH") {
        plugin_path.extend(std::env::split_paths(&existing));
    }
    (std::env::join_paths(flash_path).unwrap_or_default(), std::env::join_paths(plugin_path).unwrap_or_default())
}
