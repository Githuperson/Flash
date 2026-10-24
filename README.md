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
> Performance: measured, not claimed — Flash is currently **slower** than Rust and C++ on every benchmark (see [Performance](#performance)).

## What works

* Lexer/parser with Python-style indentation and error recovery; name resolution; static type checking with inference; clear diagnostics with source excerpts and suggestions.
* Types: `Int` and fixed-width `i8…u64`, `Float`, `Bool`, `String`, `None`, `List[T]`, `Result[T]`, structs. No implicit numeric conversions; integer overflow wraps (defined behaviour).
* Functions with inferred parameter/return types (checked per call signature), recursion, keyword arguments, modules (`import`), f-strings, `Result` error handling with `?`.
* **Memory safety without a GC:** an ownership model (move semantics for strings/lists/structs, borrowed parameters, `mut`/`own` parameter modes) enforced at compile time, deterministic cleanup, bounds-checked lists.
  No use-after-free or double-free in safe code — verified by tests that count allocations. Details and limits: [LANGUAGE_SPEC.md §10](LANGUAGE_SPEC.md#10-ownership-and-memory-safety).
* Native code generation with **Cranelift** (default; fast compiles) or **LLVM via clang** (`--backend llvm`, better optimised); dev and release builds; constant folding, dead-code elimination, inlining, provable bounds-check elimination.
* A standard library of built-ins plus modules in `stdlib/` (`std.math`, `std.text`, `std.bench`): strings, math, lists, files, paths, environment, arguments, stdin, timing.
* **Rust and C++ plugins** loaded dynamically (`.so` / `.dylib` / `.dll`) with ABI-version, metadata and signature validation, optional SHA-256 pinning, error propagation (panics and exceptions never cross the boundary). Examples for both languages. [PLUGIN_ABI.md](PLUGIN_ABI.md)
* Tools: `flash run/build/check/test/fmt`, and `flashpkg` (local packages, `flash.lock` with integrity hashes).

## What does not exist yet

Tuples, maps, user generics/traits, methods, closures, threads, `unsafe`, static linking of native libraries, process-isolated plugins, incremental compilation, an LSP server,
a package registry, Windows linking. The full list is in [LANGUAGE_SPEC.md §16](LANGUAGE_SPEC.md#16-not-implemented-yet) and [ARCHITECTURE.md §9](ARCHITECTURE.md#9-known-limitations--roadmap).

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
cargo test --workspace            # ~110 tests; needs `cc`; plugin tests also use `c++`, LLVM tests use `clang` when present
flash test tests/flash            # Flash-language tests of the standard library
```

What is covered (`crates/flash/tests/`): lexer and parser (indentation errors, recovery, precedence), name resolution and scoping, type inference and 40+ error messages (`tests/errors/`), ownership rules and the allocation counter
(every program in `tests/programs/` must finish with 0 live allocations, in dev, release and LLVM builds), arithmetic/control flow/functions/structs/strings/lists/files, optimiser behaviour (constant folding, DCE, inlining, bounds-check elimination),
CLI exit codes and runtime-error exit code 101, formatter idempotence and behaviour preservation, `flash test`, cross-target object formats (Mach-O arm64, ELF aarch64, COFF x86-64), package manager (locking, tamper detection, native requirements),
and plugins: real calls into the Rust and C++ plugins plus nine ABI-validation failure modes. **Not tested:** anything on macOS, Windows or AArch64 hardware; concurrency (there is none).

## Performance

Measured with `python3 benchmarks/run.py` (Rust `-C opt-level=3`, C++ `-O3`, Flash `--release`; identical algorithms; median of 5 runs). The latest full results, including compile times,
peak memory and executable sizes, are in [benchmarks/RESULTS.md](benchmarks/RESULTS.md). Summary on the machine used here (Linux x86-64 Xeon):

SEE_RESULTS_PLACEHOLDER

**Flash is not faster than Rust or C++** on these benchmarks. Cranelift trades optimisation for compile speed; the LLVM backend narrows the gap. The numbers describe eight small programs on one machine and say nothing general.

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
