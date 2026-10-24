use std::path::PathBuf;
use std::process::{Command, ExitCode};

use flashc::driver::{self, BuildOptions};

const HELP: &str = "\
Flash — a fast, Python-like compiled language

USAGE:
    flash run <file.flash> [--release] [-- args...]   Compile and run a program
    flash build <file.flash> [--release] [-o out]     Compile to a native executable
    flash check <file.flash>                          Type-check without generating code
    flash --version

OPTIONS:
    --release        Optimise (slower to compile, faster to run)
    -o <path>        Output file for `build`
    --target <triple>  Cross-compile (object file only, e.g. aarch64-apple-darwin)
    --emit-ir        Print the intermediate representation
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
    let mut prog_args: Vec<String> = vec![];
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--release" => opts.release = true,
            "--emit-ir" => opts.emit_ir = true,
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
        "build" => {
            let Some(f) = file else { return usage("build") };
            match driver::build(&f, &opts) {
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
