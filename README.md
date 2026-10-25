# Flash

**Flash** is a small, statically typed, ahead-of-time compiled programming language with Python-like syntax.
`flash build` turns a `.flash` file into a standalone native executable (no interpreter, no VM, no garbage collector).
The compiler generates the machine code itself and writes the executable file directly: **no LLVM, no Cranelift, no C compiler, no linker, no C library and no Rust code in the programs you build**
(a hello-world is about 13 KB). Programs that need to call C, C++ or Rust libraries can opt in to an optional legacy back end.

```python
struct Player:
    name: String
    score: Int

fn level_up(mut p: Player):
    p.score = p.score + 100

fn fib(n):                      # parameter and return types are inferred
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)

player = Player(name="Alex", score=100)
level_up(player)
say(f"{player.name} has {player.score} points; fib(20) = {fib(20)}")

for i in 0..3:                  # 0, 1, 2   (the upper bound is excluded; use 0..=3 to include it)
    say(i)
```

> **Status: v0.2, a working prototype.** The compiler, runtime, formatter, test runner and local package manager work and are covered by automated tests — on **Linux x86-64**, which is the only platform this was built and run on.
> The **macOS x86-64** target (`--target x86_64-apple-darwin`; also runs on Apple Silicon through Rosetta 2) is implemented, and the files it writes are checked with LLVM's Mach-O reader, but **it has never been run on a Mac**.
> The native back end also writes **AArch64** executables (`--target aarch64-unknown-linux-gnu` → static ELF, `--target aarch64-apple-darwin` → ad-hoc-signed, dynamic position-independent Mach-O for Apple Silicon, because current macOS kills static executables). The Linux one has been run only in a CPU emulator (Unicorn, `scripts/run_a64.py`), where the examples, float/math runtime tests and four benchmarks print exactly what the x86-64 builds print. The macOS one **runs on a GitHub-hosted Apple Silicon runner (macOS 26) in CI, and the whole test suite passes there**. Neither has been tried on real Linux ARM hardware or on a personal Mac. Performance: measured, not claimed (see [Performance](#performance)): Flash's own code generator is within 1.1–2.3× of Rust on six of the eight benchmarks,
> faster on the two allocation-heavy ones, and slower than LLVM-compiled code on `fib` and `matmul`.

## What works

* Lexer/parser with Python-style indentation and error recovery; name resolution; static type checking with inference; clear diagnostics with source excerpts and suggestions.
* Types: `Int` and fixed-width `i8…u64`, `Float`, `Bool`, `String`, `None`, `List[T]`, `Result[T]`, structs. No implicit numeric conversions; integer overflow wraps (defined behaviour).
* Functions with inferred parameter/return types (checked per call signature), recursion, keyword arguments, modules (`import`), f-strings, `Result` error handling with `?`.
* **Memory safety without a GC:** an ownership model (move semantics for strings/lists/structs, borrowed parameters, `mut`/`own` parameter modes) enforced at compile time, deterministic cleanup, bounds-checked lists.
  No use-after-free or double-free in safe code — verified by tests that count allocations. Details and limits: [LANGUAGE_SPEC.md §10](LANGUAGE_SPEC.md#10-ownership-and-memory-safety).
* **Own native code generator** (`crates/flash/src/native`): typed IR → optimiser → x86-64 machine code → a static ELF (Linux) or Mach-O (macOS) file, written directly. Optimisations: constant folding, copy propagation, jump threading,
  loop rotation, inlining, global value numbering with alias-class aware load elimination, loop-invariant code motion, strength reduction of division by constants, folded addressing modes, provable bounds-check elimination, a live-range register allocator.
  Dev builds take about 10 ms, release builds 20–100 ms; a build cache skips unchanged programs.
* **A runtime written in Flash** (`crates/flash/runtime/*.flash`): allocator, strings, lists, float printing/parsing, math functions, files and paths, all on raw system calls. Nothing is linked from outside.
* A standard library of built-ins plus modules in `stdlib/` (`std.math`, `std.text`, `std.bench`): strings, math, lists, files, paths, environment, arguments, stdin, timing.
* *Optional (build with `--features legacy-backends`):* **static linking** of C, C++ (via `extern "C"`) and `no_std` Rust libraries with `extern fn` declarations and `--link`, via the Cranelift or LLVM back ends and the system linker.
* *Optional (same feature):* **Rust and C++ plugins** loaded dynamically (`.so` / `.dylib` / `.dll`) with ABI-version, metadata and signature validation, optional SHA-256 pinning, error propagation (panics and exceptions never cross the boundary). Examples for both languages. [PLUGIN_ABI.md](PLUGIN_ABI.md)
* Tools: `flash run/build/check/test/fmt`, and `flashpkg` (local packages, `flash.lock` with integrity hashes).

## What does not exist yet

Tuples, maps, user generics/traits, methods, closures, threads, `unsafe`, strings/structs across `extern fn`, process-isolated plugins, per-module incremental compilation (only whole-program caching exists), an LSP server,
a package registry, Windows linking. The full list is in [LANGUAGE_SPEC.md §16](LANGUAGE_SPEC.md#16-not-implemented-yet) and [ARCHITECTURE.md §9](ARCHITECTURE.md#9-known-limitations--roadmap).

## Install

```bash
curl -sSf https://raw.githubusercontent.com/Githuperson/Flash/main/install.sh | sh
export PATH="$HOME/.flash/bin:$PATH"      # add to ~/.zshrc or ~/.bashrc
flash --version
```

The script downloads the prebuilt archive for your platform (Linux x86-64, macOS Apple Silicon, macOS Intel) from the latest GitHub release, verifies its SHA-256, and unpacks it into `~/.flash`
(`FLASH_HOME=dir` to change). If no release exists yet it builds from source instead, which needs Rust ([rustup.rs](https://rustup.rs)) — only to build the compiler itself. Compiling and running Flash programs needs no C compiler, linker or C library.
`flash` finds its standard library relative to its own location, so no environment variables are needed. `./uninstall.sh` removes everything.

* **From a checkout:** `./install.sh` builds from source (`FLASH_FROM_SOURCE=1` forces that for the curl form too).
* **Publishing a release (maintainers):** `git tag v0.1.0 && git push origin v0.1.0` runs `.github/workflows/release.yml`, which builds the three archives with `scripts/package.sh` and attaches them to a GitHub release.
  **That workflow has never run**, and the one-line installer only works once this code is on `main` and a release exists; the Linux archive and the download/checksum/unpack path were tested locally against a local web server, macOS archives were not.
* The plugin examples are not part of the archive; build them from a checkout (below).

## Build

Requirements: a Rust toolchain (tested with 1.97; any recent stable should work). Optional: Python 3 and `rustc`/`g++` for the benchmarks, `valgrind` for the memory test,
and — only for the optional legacy back ends — a C compiler (`cc`), `clang` for `--backend llvm`, `c++`/`make` for the C++ plugin.

```bash
git clone <this repository> flash && cd flash
cargo build --release              # the compiler: target/release/flash and flashpkg (depends only on serde, toml and sha2)
export PATH="$PWD/target/release:$PATH"
flash --version
```

`cargo build --features legacy-backends` additionally compiles in Cranelift, the LLVM text back end, `extern fn`/`--link` and plugin support; build the legacy runtime with `cargo build --workspace --features flash/legacy-backends`.

## Use

```bash
flash run examples/hello.flash                  # compile to a temporary executable and run it
flash run examples/loops.flash --release        # optimised build
flash run prog.flash -- arg1 arg2               # arguments after `--` go to the program (see args())
flash build prog.flash -o prog                  # produce ./prog; add --release for an optimised build
flash build prog.flash --target x86_64-apple-darwin -o prog      # cross-compile for macOS x86-64 (a finished Mach-O executable)
flash build prog.flash --backend llvm --release  # optional legacy back end (needs --features legacy-backends and clang)
flash check prog.flash                          # type-check only
flash fmt prog.flash                            # format in place (--check, --stdout)
flash test tests/flash                          # run every `fn test_*()` in the files under a path
```

Exit codes: `0` success, `1` compile error / failed tests / the program's own failure, `2` usage error; a compiled program exits with `101` after a runtime error (bounds, division by zero, `assert`, `unwrap`) and with whatever code it passes to `exit()`.

A diagnostic looks like this:

```
Error in main.flash, line 2, column 16:
Cannot add Int and String.

  2 | result = age + "hello"
    |                ^^^^^^^ expected a compatible numeric value

Note: this is Int (main.flash, line 2)
  2 | result = age + "hello"
    |          ^^^

Help: Convert the number to text with `str(x)`, or use an f-string like f"value: {x}".
```

### Examples

| File | Shows |
|---|---|
| `examples/hello.flash`, `arithmetic.flash`, `conditions.flash`, `loops.flash`, `functions.flash` | the basics |
| `examples/structs.flash`, `lists.flash` | structs, `mut` parameters, lists |
| `examples/plugins_demo.flash` | calling the Rust and C++ plugins |
| `tests/programs/*.flash` | larger programs used as tests (integers, strings, results/files, ownership, modules) |
| `benchmarks/*.flash` | the benchmark programs |

## Native plugins in five minutes

*Optional: needs a compiler built with `--features legacy-backends`, because plugins use a C ABI and dynamic loading, which Flash's own standalone back end deliberately does not.*

```bash
cargo build -p fast_math                        # Rust plugin  -> target/debug/libfast_math.so
make -C plugins/cpp/geometry                    # C++ plugin   -> plugins/cpp/geometry/build/libgeometry.so (.dylib on macOS)
cargo build --features legacy-backends -p flash -p flash-rt
./target/debug/flash run examples/plugins_demo.flash   # calls both
```

```python
use rust_plugin "fast_math"
use cpp_plugin "geometry"
say(fast_math.add(40, 2))                        # 42
say(geometry.distance(0.0, 0.0, 3.0, 4.0))       # 5.0
say(error_message(geometry.checked_sqrt(-4.0)))  # errors come back as Results
```

How to write, build (Linux, macOS, Windows) and load plugins, the ownership rules at the boundary, and the trust model: [PLUGIN_ABI.md](PLUGIN_ABI.md).
**Plugins are native code with full access to your process; the loader prints a warning and the library's SHA-256 each time one is loaded. ABI validation does not make an untrusted plugin safe.**

## Standalone executables

`flash build` writes the finished executable itself; there is no object file, linker run or system library involved on the default path. What a Flash executable contains: your program's machine code, the parts of the
Flash runtime it uses (compiled from Flash source into the same file), a small constant pool. What it needs from the system: the Linux kernel (or macOS kernel) — it makes raw system calls and has no dynamic section
(`ldd` reports "not a dynamic executable"). Executables are target specific: one file per OS and CPU; the macOS one has never been executed (see Status).

Limits of this path, all reported as clear errors: no `extern fn` / `--link` / native plugins (they need a C ABI; use `--features legacy-backends`), Linux and macOS on x86-64 only, no Windows.

## Static linking

*Optional: needs `--features legacy-backends` (and a C compiler as the linker).*

```bash
make -C examples/static_link                                   # builds libmathlib.a (C), libstats.a (C++), librust_static.a (no_std Rust)
flash run examples/static_link/main.flash --link examples/static_link/build/libmathlib.a \
    --link examples/static_link/build/libstats.a -lstdc++
```

```python
extern fn c_gcd(a: Int, b: Int) -> Int          # a C-ABI symbol provided by a linked library
say(c_gcd(84, 36))
```

Flash trusts the declaration (it cannot verify the C signature); types are limited to integers, `Float`, `Bool` and borrowed `String` arguments. Details: [LANGUAGE_SPEC.md](LANGUAGE_SPEC.md#statically-linked-native-libraries-extern-fn).

## Build cache

`flash build` and `flash run` store the linked executable in `~/.cache/flash` (or `$FLASH_CACHE_DIR`) keyed by a hash of every source file, the options, the plugin signatures and the compiler/runtime
identity; an unchanged program is reused without lowering, code generation or linking (`--no-cache` disables it, the newest 64 entries are kept). This is whole-program caching, not per-module incremental compilation.

## Packages

```bash
flashpkg init myapp && cd myapp
flashpkg add mylib --path ../mylib      # local dependency; writes flash.toml and flash.lock (SHA-256 per package)
flashpkg run                            # verifies the lock, builds src/main.flash, runs it
flashpkg test
```

Local paths only — there is no registry and nothing is downloaded. A changed dependency is refused until you run `flashpkg install` again. Native plugin packages declare supported OS, CPU architectures, ABI version and
toolchain requirements in `flash.toml`; their build commands are never run unless you pass `--allow-native-builds`.

## Tests

```bash
cargo test                        # ~69 test functions, native back end only (the error test checks 49 programs, the fuzz test ~100 generated ones); the memory test uses `valgrind` when present
cargo test --features legacy-backends   # ~82: also Cranelift, LLVM (needs clang), static linking and plugins (needs cc, c++, ar, rustc)
flash test tests/flash            # Flash-language tests of the standard library
```

What is covered (`crates/flash/tests/`): lexer and parser (indentation errors, recovery, precedence), name resolution and scoping, type inference and 40+ error messages (`tests/errors/`), ownership rules and the allocation counter
(every program in `tests/programs/` must finish with 0 live allocations, in dev and release builds, and with the legacy back ends when compiled in), arithmetic/control flow/functions/structs/strings/lists/files, optimiser behaviour (constant folding, DCE, inlining, bounds-check elimination),
CLI exit codes and runtime-error exit code 101, formatter idempotence and behaviour preservation, `flash test`, the Mach-O output structure (checked with LLVM's reader), cross-target object formats of the legacy back ends (Mach-O arm64, ELF aarch64, COFF x86-64), the Flash runtime's float printing/parsing and math functions against Rust's standard library on thousands of random inputs, package manager (locking, tamper detection, native requirements),
and plugins: real calls into the Rust and C++ plugins plus nine ABI-validation failure modes. AArch64 (Linux and macOS) output in an emulator against the x86-64 results (`tests/native_aarch64.rs`, needs python3 + `unicorn`). **Not tested:** anything on real macOS, Windows or AArch64 hardware; concurrency (there is none).

## Performance

Measured with `python3 benchmarks/run.py --legacy-flash <legacy-enabled flash>` (Rust `-C opt-level=3`, C++ `-O3`, Flash `--release`; identical algorithms; median of 9 runs). The latest full results, including compile times,
peak memory and executable sizes, are in [benchmarks/RESULTS.md](benchmarks/RESULTS.md). Summary on the machine used here (Linux x86-64 Xeon, noisy: 10–30% run-to-run on some programs):

Run time in seconds (median of 9; lower is better):

| benchmark | flash-dev | flash-release (native) | flash cranelift\* | flash llvm\* | rust -O | c++ -O3 | c++ -O0 |
|---|---|---|---|---|---|---|---|
| int_loop | 0.560 | 0.570 | 0.517 | 0.500 | 0.465 | 0.540 | 1.040 |
| fib | 0.230 | 0.240 | 0.309 | 0.124 | 0.136 | 0.075 | 0.345 |
| mandelbrot | 0.167 | 0.148 | 0.154 | 0.140 | 0.133 | 0.137 | 0.297 |
| sieve | 0.154 | 0.143 | 0.138 | 0.116 | 0.119 | 0.114 | 0.267 |
| strings | 0.048 | 0.048 | 0.064 | 0.063 | 0.066 | 0.051 | 0.105 |
| alloc | 0.185 | 0.085 | 0.235 | 0.248 | 0.272 | 0.135 | 0.719 |
| nbody | 0.754 | 0.331 | 0.414 | 0.329 | 0.209 | 0.325 | 3.216 |
| matmul | 0.058 | 0.045 | 0.043 | 0.022 | 0.020 | 0.013 | 0.150 |

\* the optional legacy back ends, for comparison.

Flash (native) run time divided by Rust / C++ run time (above 1.00 means Flash is slower):

| benchmark | native / rust | native / c++ -O3 | llvm\* / rust |
|---|---|---|---|
| int_loop | 1.22x | 1.06x | 1.07x |
| fib | 1.76x | 3.20x | 0.91x |
| mandelbrot | 1.11x | 1.08x | 1.05x |
| sieve | 1.20x | 1.26x | 0.97x |
| strings | 0.74x | 0.94x | 0.97x |
| alloc | 0.31x | 0.63x | 0.91x |
| nbody | 1.59x | 1.02x | 1.57x |
| matmul | 2.24x | 3.39x | 1.08x |

**What this does and does not show.** Flash's own code generator is within roughly 10–25% of Rust on three programs, 1.6–1.8× on `nbody` and `fib`, and 2.2× on `matmul`; it is faster than Rust on `strings` and `alloc`, which are dominated by
allocation (Flash's allocator is a few inlined instructions; the Rust/C++ programs use glibc `malloc`, so this compares allocators as much as compilers). LLVM still generates better code for `fib` and `matmul`.
Flash is **not** shown to be faster than Rust or C++ on compute workloads: Flash checks list indexes, structs are heap blocks, there is no vectorisation, and the benchmarks are small single-machine measurements.
A native-only Flash executable needs 0.3 MiB of memory for a trivial program (Rust: 2.2 MiB) and is tens of KiB in size.

## Documentation

* [LANGUAGE_SPEC.md](LANGUAGE_SPEC.md) — syntax, types, semantics, ownership rules, built-ins, grammar, and what is not implemented
* [ARCHITECTURE.md](ARCHITECTURE.md) — compiler stages, IR, optimiser, the native back end, the Flash-written runtime, status
* [ROADMAP.md](ROADMAP.md), [BENCHMARKS.md](BENCHMARKS.md) — what is done and what is next; how the comparison with Rust and C++ is made
* [PLUGIN_ABI.md](PLUGIN_ABI.md) — native plugin ABI v1, manifests, ownership at the boundary, trust model, Rust and C++ authoring guides
* [benchmarks/RESULTS.md](benchmarks/RESULTS.md) — latest measurements

## Repository layout

```
crates/flash/        compiler library + `flash` and `flashpkg` binaries (src/, the native back end is src/native/), integration tests (tests/)
crates/flash/runtime/  the Flash runtime library, written in Flash (embedded into the compiler)
crates/flash-rt/     legacy Rust runtime: only for the optional legacy back ends, plugins and `extern fn`
crates/flash-plugin-sdk/  Rust SDK for plugins
include/             C/C++ plugin headers
plugins/rust/fast_math, plugins/cpp/geometry   example plugins (+ manifests, flash.toml)
stdlib/              standard library modules written in Flash
examples/, tests/    sample programs, expected outputs, error programs, Flash tests
benchmarks/          Flash / Rust / C++ benchmark programs and run.py
```

License: MIT (see `LICENSE`).
