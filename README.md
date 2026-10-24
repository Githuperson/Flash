# Flash

**Flash** is a small, statically typed, ahead-of-time compiled programming language with Python-like syntax.
`flash build` turns a `.flash` file into a standalone native executable (no interpreter, no VM, no garbage collector),
and Flash programs can call native **Rust** and **C++** libraries through a documented, versioned C ABI.

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

> **Status: v0.1, a working prototype.** The compiler, runtime, formatter, test runner, local package manager and the Rust/C++
> plugin interface all work and are covered by automated tests — on **Linux x86-64**, which is the only platform this was built and run on.
> Code generation for macOS (including Apple Silicon), Windows and AArch64 Linux is implemented in the compiler but linking and running there is **untested**.
> Performance: measured, not claimed (see [Performance](#performance)). With the default Cranelift backend Flash is 1.0–2.4× slower than Rust on the eight benchmarks; with the optional LLVM backend it is roughly on par with Rust on most of them and still slower than C++ on three.

## What works

* Lexer/parser with Python-style indentation and error recovery; name resolution; static type checking with inference; clear diagnostics with source excerpts and suggestions.
* Types: `Int` and fixed-width `i8…u64`, `Float`, `Bool`, `String`, `None`, `List[T]`, `Result[T]`, structs. No implicit numeric conversions; integer overflow wraps (defined behaviour).
* Functions with inferred parameter/return types (checked per call signature), recursion, keyword arguments, modules (`import`), f-strings, `Result` error handling with `?`.
* **Memory safety without a GC:** an ownership model (move semantics for strings/lists/structs, borrowed parameters, `mut`/`own` parameter modes) enforced at compile time, deterministic cleanup, bounds-checked lists.
  No use-after-free or double-free in safe code — verified by tests that count allocations. Details and limits: [LANGUAGE_SPEC.md §10](LANGUAGE_SPEC.md#10-ownership-and-memory-safety).
* Native code generation with **Cranelift** (default; fast compiles) or **LLVM via clang** (`--backend llvm`, better optimised); dev and release builds; constant folding, dead-code elimination, inlining, provable bounds-check elimination, type-based alias analysis; a build cache that skips unchanged programs.
* A standard library of built-ins plus modules in `stdlib/` (`std.math`, `std.text`, `std.bench`): strings, math, lists, files, paths, environment, arguments, stdin, timing.
* **Static linking** of C, C++ (via `extern "C"`) and `no_std` Rust libraries with `extern fn` declarations and `--link`.
* **Rust and C++ plugins** loaded dynamically (`.so` / `.dylib` / `.dll`) with ABI-version, metadata and signature validation, optional SHA-256 pinning, error propagation (panics and exceptions never cross the boundary). Examples for both languages. [PLUGIN_ABI.md](PLUGIN_ABI.md)
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
(`FLASH_HOME=dir` to change). If no release exists yet it builds from source instead, which needs Rust ([rustup.rs](https://rustup.rs)). Compiling Flash programs always needs a C compiler (`cc`) for linking.
`flash` finds its runtime library and standard library relative to its own location, so no environment variables are needed. `./uninstall.sh` removes everything.

* **From a checkout:** `./install.sh` builds from source (`FLASH_FROM_SOURCE=1` forces that for the curl form too).
* **Publishing a release (maintainers):** `git tag v0.1.0 && git push origin v0.1.0` runs `.github/workflows/release.yml`, which builds the three archives with `scripts/package.sh` and attaches them to a GitHub release.
  **That workflow has never run**, and the one-line installer only works once this code is on `main` and a release exists; the Linux archive and the download/checksum/unpack path were tested locally against a local web server, macOS archives were not.
* The plugin examples are not part of the archive; build them from a checkout (below).

## Build

Requirements: a Rust toolchain (tested with 1.97; any recent stable should work) and a C compiler/linker reachable as `cc`. Optional: `clang` for `--backend llvm`, `c++`/`make` for the C++ plugin, Python 3 for the benchmarks.

```bash
git clone <this repository> flash && cd flash
cargo build --workspace            # compiler (flash, flashpkg), runtime library, Rust example plugin   (add --release for an optimised compiler)
export PATH="$PWD/target/debug:$PATH"
flash --version
```

`cargo build --workspace` produces `target/debug/flash`, `target/debug/flashpkg`, `target/debug/libflash_rt.a` (the runtime every program is linked with) and `target/debug/libfast_math.so`.
Keep `libflash_rt.a` next to the `flash` executable (or set `FLASH_RT_LIB`).

## Use

```bash
flash run examples/hello.flash                  # compile to a temporary executable and run it
flash run examples/loops.flash --release        # optimised build
flash run prog.flash -- arg1 arg2               # arguments after `--` go to the program (see args())
flash build prog.flash -o prog                  # produce ./prog; add --release for an optimised build
flash build prog.flash --release --backend llvm # use clang/LLVM for code generation
flash build prog.flash --target aarch64-apple-darwin -o prog.o   # cross-compile an object file (no link)
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

```bash
cargo build -p fast_math                        # Rust plugin  -> target/debug/libfast_math.so
make -C plugins/cpp/geometry                    # C++ plugin   -> plugins/cpp/geometry/build/libgeometry.so (.dylib on macOS)
flash run examples/plugins_demo.flash           # calls both
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

## Static linking

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
cargo test --workspace            # 75 test functions (the error test checks 49 programs, the fuzz test ~100 generated ones); needs `cc`, `c++`, `ar`, `rustc`; LLVM tests use `clang` and the memory test uses `valgrind` when present
flash test tests/flash            # Flash-language tests of the standard library
```

What is covered (`crates/flash/tests/`): lexer and parser (indentation errors, recovery, precedence), name resolution and scoping, type inference and 40+ error messages (`tests/errors/`), ownership rules and the allocation counter
(every program in `tests/programs/` must finish with 0 live allocations, in dev, release and LLVM builds), arithmetic/control flow/functions/structs/strings/lists/files, optimiser behaviour (constant folding, DCE, inlining, bounds-check elimination),
CLI exit codes and runtime-error exit code 101, formatter idempotence and behaviour preservation, `flash test`, cross-target object formats (Mach-O arm64, ELF aarch64, COFF x86-64), package manager (locking, tamper detection, native requirements),
and plugins: real calls into the Rust and C++ plugins plus nine ABI-validation failure modes. **Not tested:** anything on macOS, Windows or AArch64 hardware; concurrency (there is none).

## Performance

Measured with `python3 benchmarks/run.py` (Rust `-C opt-level=3`, C++ `-O3`, Flash `--release`; identical algorithms; median of 7 runs). The latest full results, including compile times,
peak memory and executable sizes, are in [benchmarks/RESULTS.md](benchmarks/RESULTS.md). Summary on the machine used here (Linux x86-64 Xeon):

Run time in seconds (median of 7; lower is better):

| benchmark | flash-dev | flash-release | flash-release-llvm | rust -O | c++ -O3 | c++ -O0 |
|---|---|---|---|---|---|---|
| int_loop | 1.052 | 0.377 | 0.324 | 0.324 | 0.322 | 0.774 |
| fib | 0.156 | 0.156 | 0.096 | 0.102 | 0.056 | 0.264 |
| mandelbrot | 0.111 | 0.112 | 0.095 | 0.093 | 0.092 | 0.207 |
| sieve | 0.136 | 0.131 | 0.113 | 0.122 | 0.113 | 0.219 |
| strings | 0.045 | 0.045 | 0.042 | 0.044 | 0.040 | 0.070 |
| alloc | 0.196 | 0.197 | 0.174 | 0.197 | 0.081 | 0.407 |
| nbody | 0.541 | 0.278 | 0.213 | 0.163 | 0.218 | 1.698 |
| matmul | 0.046 | 0.037 | 0.016 | 0.015 | 0.010 | 0.116 |

Flash run time divided by Rust / C++ run time (above 1.00 means Flash is slower):

| benchmark | cranelift / rust | cranelift / c++ -O3 | llvm / rust | llvm / c++ -O3 |
|---|---|---|---|---|
| int_loop | 1.16x | 1.17x | 1.00x | 1.01x |
| fib | 1.53x | 2.79x | 0.94x | 1.71x |
| mandelbrot | 1.20x | 1.22x | 1.02x | 1.03x |
| sieve | 1.07x | 1.16x | 0.92x | 1.00x |
| strings | 1.02x | 1.12x | 0.95x | 1.04x |
| alloc | 1.00x | 2.42x | 0.89x | 2.14x |
| nbody | 1.71x | 1.28x | 1.31x | 0.98x |
| matmul | 2.42x | 3.90x | 1.06x | 1.70x |

**What this does and does not show.** With the default Cranelift backend Flash is slower than Rust on 7 of 8 programs (1.0×–2.4×) and slower than C++ on all of them. With `--backend llvm` the run times are within about ±10% of Rust on six programs (differences that small on ~0.1 s runs are noise, not a win), 1.31× on `nbody` and 1.06× on `matmul`, and 1.0–2.1× of C++ (`fib`, `matmul` and `alloc` are the slow cases). Flash is **not** shown to be faster than Rust or C++: Flash checks list indexes and the benchmarks are small, single-machine measurements. Cranelift trades optimisation for compile speed (Flash builds take 0.05–0.13 s).

## Documentation

* [LANGUAGE_SPEC.md](LANGUAGE_SPEC.md) — syntax, types, semantics, ownership rules, built-ins, grammar, and what is not implemented
* [ARCHITECTURE.md](ARCHITECTURE.md) — compiler stages, IR, backend decision, runtime, status and roadmap
* [PLUGIN_ABI.md](PLUGIN_ABI.md) — native plugin ABI v1, manifests, ownership at the boundary, trust model, Rust and C++ authoring guides
* [benchmarks/RESULTS.md](benchmarks/RESULTS.md) — latest measurements

## Repository layout

```
crates/flash/        compiler library + `flash` and `flashpkg` binaries (src/), integration tests (tests/)
crates/flash-rt/     runtime static library linked into every program, incl. the plugin loader
crates/flash-plugin-sdk/  Rust SDK for plugins
include/             C/C++ plugin headers
plugins/rust/fast_math, plugins/cpp/geometry   example plugins (+ manifests, flash.toml)
stdlib/              standard library modules written in Flash
examples/, tests/    sample programs, expected outputs, error programs, Flash tests
benchmarks/          Flash / Rust / C++ benchmark programs and run.py
```

License: MIT (see `LICENSE`).
