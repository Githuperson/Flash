use std::path::PathBuf;
use std::process::{Command, ExitCode};

use flashc::driver::{self, BuildOptions};

const HELP: &str = "\
Flash — a fast, Python-like compiled language

USAGE:
    flash run <file.flash> [--release] [-- args...]   Compile and run a program
    flash build <file.flash> [--release] [-o out]     Compile to a native executable
    flash test [file-or-dir] [--release]               Run `fn test_*()` functions
    flash check <file.flash>                          Type-check without generating code
    flash fmt <file.flash> [--check|--stdout]         Format source code
    flash --version

OPTIONS:
    --release        Optimise (slower to compile, faster to run)
    -o <path>        Output file for `build`
    --target <triple>  Cross-compile (object file only, e.g. aarch64-apple-darwin)
    --emit-ir        Print the intermediate representation
    --link <lib>     Link a native library (.a/.o/.so) for `extern fn` declarations; also -l<name> -L<dir>
    --linker <l>     `builtin` (no C compiler needed; Linux x86-64), `system` (cc), or `auto` (default: builtin when possible)
    --no-cache       Do not reuse or store executables in the build cache (~/.cache/flash, or $FLASH_CACHE_DIR)
    --backend <b>    Code generator: `auto` (default: LLVM for --release when clang is installed, else Cranelift),
                     `cranelift` (fast compiles, no external tools) or `llvm` (needs clang)
    --emit-llvm      Print the LLVM IR instead of building
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" || args[0] == "help" {
        print!("{}", HELP);
        return ExitCode::SUCCESS;
    }
    if args[0] == "--version" || args[0] == "-V" {
        println!("flash {}", flashc::VERSION);
        return ExitCode::SUCCESS;
    }
    let cmd = args[0].as_str();
    let rest = &args[1..];
    let mut opts = BuildOptions::default();
    let mut file: Option<PathBuf> = None;
    let mut opts_check = false;
    let mut opts_emit_llvm = false;
    let mut opts_stdout = false;
    let mut prog_args: Vec<String> = vec![];
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--release" => opts.release = true,
            "--emit-ir" => opts.emit_ir = true,
            "--no-cache" => opts.no_cache = true,
            "--link" => {
                i += 1;
                match rest.get(i) {
                    Some(p) => opts.link_args.push(p.clone()),
                    None => {
                        eprintln!("--link needs the path of a library (.a / .o / .so)");
                        return ExitCode::from(2);
                    }
                }
            }
            "-l" | "-L" => {
                let flag = rest[i].clone();
                i += 1;
                match rest.get(i) {
                    Some(v) => opts.link_args.push(format!("{}{}", flag, v)),
                    None => {
                        eprintln!("{} needs a value", flag);
                        return ExitCode::from(2);
                    }
                }
            }
            "--linker" => {
                i += 1;
                opts.linker = match rest.get(i).map(|s| s.as_str()) {
                    Some("auto") => driver::Linker::Auto,
                    Some("system") => driver::Linker::System,
                    Some("builtin") => driver::Linker::Builtin,
                    other => {
                        eprintln!("--linker must be `auto`, `system` or `builtin` (got {:?})", other);
                        return ExitCode::from(2);
                    }
                };
            }
            "--emit-llvm" => {
                opts_emit_llvm = true;
            }
            "--backend" => {
                i += 1;
                opts.backend = match rest.get(i).map(|s| s.as_str()) {
                    Some("auto") => flashc::codegen::Backend::Auto,
                    Some("cranelift") => flashc::codegen::Backend::Cranelift,
                    Some("llvm") => flashc::codegen::Backend::Llvm,
                    other => {
                        eprintln!("--backend must be `auto`, `cranelift` or `llvm` (got {:?})", other);
                        return ExitCode::from(2);
                    }
                };
            }
            "--check" => opts_check = true,
            "--stdout" => opts_stdout = true,
            "-o" => {
                i += 1;
                opts.output = rest.get(i).map(PathBuf::from);
            }
            "--target" => {
                i += 1;
                opts.target = rest.get(i).cloned();
                opts.emit_obj_only = true;
            }
            "--" => {
                prog_args = rest[i + 1..].to_vec();
                break;
            }
            a if a.starts_with("-l") || a.starts_with("-L") || a.starts_with("-Wl,") => opts.link_args.push(a.to_string()),
            a if a.starts_with('-') => {
                eprintln!("Unknown option '{}'. Run `flash --help`.", a);
                return ExitCode::from(2);
            }
            a => {
                if file.is_none() {
                    file = Some(PathBuf::from(a));
                } else {
                    prog_args.push(a.to_string());
                }
            }
        }
        i += 1;
    }
    match cmd {
        "check" => {
            let Some(f) = file else { return usage("check") };
            match driver::check(&f) {
                Ok(()) => {
                    println!("OK: {} has no errors.", f.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprint!("{}", e);
                    ExitCode::from(1)
                }
            }
        }
        "build" if opts_emit_llvm => {
            let Some(f) = file else { return usage("build") };
            match driver::frontend(&f, None) {
                Ok(fe) => {
                    let m = driver::lower_and_optimise(&fe.program, opts.release);
                    print!("{}", flashc::llvm::emit_ir(&m));
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprint!("{}", e);
                    ExitCode::from(1)
                }
            }
        }
        "build" => {
            let Some(f) = file else { return usage("build") };
            match driver::build(&f, &opts) {
                Ok((out, t)) if t.cached => {
                    println!(
                        "Built {} ({} mode; reused from the build cache in {:.0?})",
                        out.display(),
                        if opts.release { "release" } else { "dev" },
                        t.frontend
                    );
                    ExitCode::SUCCESS
                }
                Ok((out, t)) => {
                    println!(
                        "Built {} ({} mode; front end {:.0?}, code generation {:.0?}, link {:.0?})",
                        out.display(),
                        if opts.release { "release" } else { "dev" },
                        t.frontend,
                        t.backend,
                        t.link
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprint!("{}", e);
                    ExitCode::from(1)
                }
            }
        }
        "test" => {
            let target =
                file.unwrap_or_else(|| if std::path::Path::new("tests").is_dir() { PathBuf::from("tests") } else { PathBuf::from(".") });
            let s = driver::run_tests(&target, opts.release);
            println!("\ntest result: {} passed; {} failed; {} files failed to compile", s.passed, s.failed, s.compile_errors);
            if s.passed + s.failed + s.compile_errors == 0 {
                eprintln!("No tests found under {}. A test is a function named `test_...` with no parameters.", target.display());
                return ExitCode::from(1);
            }
            if s.failed + s.compile_errors > 0 {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        "fmt" => {
            let Some(f) = file else { return usage("fmt") };
            let src = match std::fs::read_to_string(&f) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("cannot read {}: {}", f.display(), e);
                    return ExitCode::from(1);
                }
            };
            match flashc::fmt::format_source(&src) {
                Ok(out) => {
                    if opts_check {
                        if out != src {
                            eprintln!("{} is not formatted. Run `flash fmt {}`.", f.display(), f.display());
                            return ExitCode::from(1);
                        }
                        println!("{} is formatted.", f.display());
                    } else if opts_stdout {
                        print!("{}", out);
                    } else if out != src {
                        if let Err(e) = std::fs::write(&f, out) {
                            eprintln!("cannot write {}: {}", f.display(), e);
                            return ExitCode::from(1);
                        }
                        println!("Formatted {}", f.display());
                    } else {
                        println!("{} was already formatted.", f.display());
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{}: {}", f.display(), e);
                    ExitCode::from(1)
                }
            }
        }
        "run" => {
            let Some(f) = file else { return usage("run") };
            if opts.target.is_some() {
                eprintln!("`flash run` cannot be combined with --target.");
                return ExitCode::from(2);
            }
            let tmp = std::env::temp_dir().join(format!("flash-run-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&tmp);
            let stem = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "prog".into());
            opts.output = Some(tmp.join(format!("{}{}", stem, driver::exe_suffix(None))));
            let built = driver::build(&f, &opts);
            let exe = match built {
                Ok((p, _)) => p,
                Err(e) => {
                    eprint!("{}", e);
                    let _ = std::fs::remove_dir_all(&tmp);
                    return ExitCode::from(1);
                }
            };
            let status = Command::new(&exe).args(&prog_args).status();
            let _ = std::fs::remove_dir_all(&tmp);
            match status {
                Ok(s) => ExitCode::from(s.code().unwrap_or(1).clamp(0, 255) as u8),
                Err(e) => {
                    eprintln!("cannot run the compiled program: {}", e);
                    ExitCode::from(1)
                }
            }
        }
        other => {
            eprintln!("Unknown command '{}'. Run `flash --help`.", other);
            ExitCode::from(2)
        }
    }
}

fn usage(cmd: &str) -> ExitCode {
    eprintln!("Missing file. Usage: flash {} <file.flash>", cmd);
    ExitCode::from(2)
}
