use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use flashc::driver::{self, BuildOptions};
use flashc::pkg::{self, DepSpec, Manifest, PackageSection};

const HELP: &str = "\
flashpkg — the Flash package manager (local packages only)

USAGE:
    flashpkg init [name]                  Create flash.toml and a starter project
    flashpkg add <name> --path <dir>      Add a local dependency
    flashpkg install                      Resolve dependencies and write flash.lock
    flashpkg list                         Show the locked dependency graph
    flashpkg verify                       Check dependencies against flash.lock
    flashpkg build [--release] [--allow-native-builds]
    flashpkg run [--release]              Build and run the package
    flashpkg test [--release]             Run the package's tests (tests/)

There is no online registry: dependencies are directories on disk.
";

fn fail(msg: impl AsRef<str>) -> ExitCode {
    eprintln!("flashpkg: {}", msg.as_ref());
    ExitCode::from(1)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(|s| s.as_str()) else {
        print!("{}", HELP);
        return ExitCode::SUCCESS;
    };
    let rest = &args[1..];
    let flag = |f: &str| rest.iter().any(|a| a == f);
    let root = PathBuf::from(".");
    match cmd {
        "--help" | "-h" | "help" => {
            print!("{}", HELP);
            ExitCode::SUCCESS
        }
        "init" => init(rest.first().map(|s| s.as_str())),
        "add" => add(&root, rest),
        "install" => match install(&root) {
            Ok(n) => {
                println!("Locked {} package{} in {}", n, if n == 1 { "" } else { "s" }, pkg::LOCKFILE);
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
        "list" => match pkg::read_lock(&root) {
            Ok(l) => {
                for p in &l.packages {
                    println!("{} {} ({}) {}", p.name, p.version, p.source, p.integrity);
                    for d in &p.dependencies {
                        println!("    -> {}", d);
                    }
                }
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
        "verify" => match pkg::resolve(&root).and_then(|p| pkg::verify_lock(&root, &p).map(|_| p.len())) {
            Ok(n) => {
                println!("OK: {} package{} match {}", n, if n == 1 { "" } else { "s" }, pkg::LOCKFILE);
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
        "build" | "run" | "test" => {
            let release = flag("--release");
            match prepare(&root, flag("--allow-native-builds")) {
                Err(e) => fail(e),
                Ok(m) => match cmd {
                    "test" => {
                        let s = driver::run_tests(&root.join("tests"), release);
                        println!("\ntest result: {} passed; {} failed; {} files failed to compile", s.passed, s.failed, s.compile_errors);
                        if s.failed + s.compile_errors > 0 || s.passed == 0 {
                            ExitCode::from(1)
                        } else {
                            ExitCode::SUCCESS
                        }
                    }
                    _ => {
                        let entry = root.join(m.package.entry.clone().unwrap_or_else(|| "src/main.flash".into()));
                        let out_dir = root.join("build");
                        let _ = std::fs::create_dir_all(&out_dir);
                        let out = out_dir.join(format!("{}{}", m.package.name, driver::exe_suffix(None)));
                        let opts = BuildOptions { release, output: Some(out.clone()), ..Default::default() };
                        match driver::build(&entry, &opts) {
                            Err(e) => {
                                eprint!("{}", e);
                                ExitCode::from(1)
                            }
                            Ok(_) => {
                                println!("Built {}", out.display());
                                if cmd == "run" {
                                    let extra: Vec<&String> = rest.iter().filter(|a| !a.starts_with("--")).collect();
                                    match Command::new(&out).args(extra).status() {
                                        Ok(s) => ExitCode::from(s.code().unwrap_or(1).clamp(0, 255) as u8),
                                        Err(e) => fail(format!("cannot run {}: {}", out.display(), e)),
                                    }
                                } else {
                                    ExitCode::SUCCESS
                                }
                            }
                        }
                    }
                },
            }
        }
        other => {
            eprintln!("flashpkg: unknown command '{}'\n", other);
            print!("{}", HELP);
            ExitCode::from(2)
        }
    }
}

fn init(name: Option<&str>) -> ExitCode {
    let dir = match name {
        Some(n) => PathBuf::from(n),
        None => PathBuf::from("."),
    };
    let pkg_name = match name {
        Some(n) => n.to_string(),
        None => std::env::current_dir()
            .ok()
            .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "app".into()),
    };
    let pkg_name = pkg_name.to_lowercase().replace(['-', ' '], "_");
    if !pkg::valid_name(&pkg_name) {
        return fail(format!("'{}' is not a valid package name (use lowercase letters, digits and underscores)", pkg_name));
    }
    if dir.join(pkg::MANIFEST).exists() {
        return fail(format!("{} already exists", dir.join(pkg::MANIFEST).display()));
    }
    let _ = std::fs::create_dir_all(dir.join("src"));
    let _ = std::fs::create_dir_all(dir.join("tests"));
    let m = Manifest {
        package: PackageSection {
            name: pkg_name.clone(),
            version: "0.1.0".into(),
            kind: Some("app".into()),
            entry: Some("src/main.flash".into()),
        },
        ..Default::default()
    };
    if let Err(e) = pkg::write_manifest(&dir, &m) {
        return fail(e);
    }
    let main = dir.join("src/main.flash");
    if !main.exists() {
        let _ = std::fs::write(&main, format!("say(\"Hello from {}!\")\n", pkg_name));
    }
    let t = dir.join("tests/basic_test.flash");
    if !t.exists() {
        let _ = std::fs::write(&t, "fn test_arithmetic():\n    assert(1 + 1 == 2, \"math still works\")\n");
    }
    println!("Created package '{}' in {}", pkg_name, dir.display());
    ExitCode::SUCCESS
}

fn add(root: &Path, rest: &[String]) -> ExitCode {
    let Some(name) = rest.first() else { return fail("usage: flashpkg add <name> --path <dir>") };
    let Some(pi) = rest.iter().position(|a| a == "--path") else {
        return fail("only local path dependencies are supported: flashpkg add <name> --path <dir> (there is no registry)");
    };
    let Some(path) = rest.get(pi + 1) else { return fail("--path needs a directory") };
    let dep_dir = root.join(path);
    let dm = match pkg::read_manifest(&dep_dir) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    if &dm.package.name != name {
        return fail(format!("{} contains the package '{}', not '{}'", dep_dir.display(), dm.package.name, name));
    }
    let mut m = match pkg::read_manifest(root) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    m.dependencies.insert(name.clone(), DepSpec { path: path.clone() });
    if let Err(e) = pkg::write_manifest(root, &m) {
        return fail(e);
    }
    match install(root) {
        Ok(_) => {
            println!("Added {} {} from {}", name, dm.package.version, path);
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

fn install(root: &Path) -> Result<usize, String> {
    let pkgs = pkg::resolve(root)?;
    for p in &pkgs {
        pkg::check_native_requirements(p)?;
    }
    let lock = pkg::make_lock(&pkgs)?;
    pkg::write_lock(root, &lock)?;
    Ok(lock.packages.len())
}

/// Verify the lock file, check native requirements, optionally run native builds, set the
/// environment so the compiler and the runtime can find dependencies.
fn prepare(root: &Path, allow_native_builds: bool) -> Result<Manifest, String> {
    let pkgs = pkg::resolve(root)?;
    pkg::verify_lock(root, &pkgs)?;
    for p in &pkgs {
        pkg::check_native_requirements(p)?;
        if let Some(cmd) = p.manifest.native.as_ref().and_then(|n| n.build.as_ref()) {
            if allow_native_builds {
                println!("Running native build for '{}': {}", p.name, cmd);
                let st = Command::new("sh").arg("-c").arg(cmd).current_dir(&p.dir).status().map_err(|e| e.to_string())?;
                if !st.success() {
                    return Err(format!("the native build of '{}' failed", p.name));
                }
            } else {
                eprintln!("note: '{}' declares a native build command; it was not run. Pass --allow-native-builds to run it (it executes code from the dependency).", p.name);
            }
        }
    }
    let (fp, pp) = pkg::build_env(root, &pkgs);
    std::env::set_var("FLASH_PATH", fp);
    std::env::set_var("FLASH_PLUGIN_PATH", pp);
    pkgs.into_iter().next().map(|r| r.manifest).ok_or_else(|| "no root package".to_string())
}
