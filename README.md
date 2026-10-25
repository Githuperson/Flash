# Flash ⚡

Python-looking code, native-speed programs. Flash is a small, statically typed language that compiles straight to machine code: no interpreter, no VM, no garbage collector.

The part we're proudest of: **the compiler does everything itself.** It generates the machine code and writes the finished executable. No LLVM, no Cranelift, no C compiler, no linker, no libc, and no Rust in the programs you build. A hello-world is about 13 KB.

```python
struct Player:
    name: String
    score: Int

fn level_up(mut p: Player):
    p.score = p.score + 100

fn fib(n):                      # types are inferred
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)

player = Player(name="Alex", score=100)
level_up(player)
say(f"{player.name} has {player.score} points; fib(20) = {fib(20)}")

for i in 0..3:                  # 0, 1, 2  (use 0..=3 to include 3)
    say(i)
```

## Install

```bash
curl -sSf https://raw.githubusercontent.com/Githuperson/Flash/main/install.sh | sh
export PATH="$HOME/.flash/bin:$PATH"
flash --version
```

The script grabs the prebuilt archive for your machine from the [latest release](https://github.com/Githuperson/Flash/releases/latest) and checks its SHA-256. **You don't need Rust, a C compiler or anything else.** Only if there's no archive for your machine (an Intel Mac, say) does it fall back to building from source, which needs [Rust](https://rustup.rs).

**Windows:** download `flash-windows-x86_64.zip` from the releases page, unzip it and add its `bin` folder to `PATH`.

## Try it

```bash
echo 'say("Hello, world!")' > hello.flash
flash run hello.flash                         # compile and run (add -- args to pass arguments)
flash build hello.flash --release -o hello    # standalone, optimised executable
flash check hello.flash                       # type-check only
flash fmt hello.flash                         # format in place
flash test tests/flash                        # run every `fn test_*()` under a path
flash build hello.flash --target x86_64-pc-windows-msvc    # cross-compile, no extra toolchain
```

Targets: Linux, macOS and Windows on x86-64, plus Linux and macOS on arm64 (Windows on ARM isn't supported). Any `flash` can cross-compile for the others (macOS Intel is the shaky one, see below). More examples are in [`examples/`](examples/). `./uninstall.sh` removes everything the installer put in `~/.flash`.

## What you get

- **Safe without a GC.** Ownership and move semantics are checked at compile time, cleanup is deterministic, and lists are bounds-checked. Tests count allocations: no leaks, no use-after-free in safe code. ([the rules](LANGUAGE_SPEC.md#10-ownership-and-memory-safety))
- **Friendly errors.** Source excerpts, a pointer to the culprit and a suggested fix:
  ```
  Error in main.flash, line 2, column 16:
  Cannot add Int and String.
    2 | result = age + "hello"
      |                ^^^^^^^ expected a compatible numeric value
  Help: Convert the number to text with `str(x)`, or use an f-string like f"value: {x}".
  ```
- **Fast to compile.** Dev builds take ~10 ms, release builds 20–100 ms, and unchanged programs are served from a build cache.
- **Types:** `Int`, `i8…u64`, `Float`, `Bool`, `String`, `List[T]`, `Result[T]` (with `?`), structs, f-strings, modules, keyword arguments. Integer overflow wraps, and there are no implicit conversions.
- **Batteries:** strings, math, files, paths, args, env, stdin, timing, plus `std.math` / `std.text` / `std.bench`. The runtime is written in Flash itself and talks straight to the OS.
- **Tools:** `flash run | build | check | test | fmt`, and `flashpkg` for local packages with a hash-checked lock file.

Not there yet: tuples, maps, user generics, methods, closures, threads, an LSP server, a package registry. See [LANGUAGE_SPEC.md §16](LANGUAGE_SPEC.md#16-not-implemented-yet).

## How well does it run?

Honest numbers against Rust (`-O`) on one Linux x86-64 box, run time divided by Rust's (1.00 = same speed, higher = Flash slower). Timings wobble 10–30% between runs, so don't read much into small differences:

| benchmark | Flash / Rust |
|---|---|
| fib | **0.93×** (the optimiser expands small recursive functions once) |
| strings | **0.59×** (allocator-bound; Flash's allocator is a few inlined instructions) |
| alloc | **0.34×** (same) |
| sieve | 1.11× |
| mandelbrot | 1.15× |
| int_loop | 1.23× |
| nbody | 1.6–2× |
| matmul | 2.4× |

So: on par with Rust on some programs, behind on others, and **not** faster on pure number-crunching. Flash bounds-checks lists, structs live on the heap and there is no vectorisation yet. It also uses much less memory (0.3 MiB for a trivial program vs 2.2 MiB for Rust). Full tables, methodology and caveats: [BENCHMARKS.md](BENCHMARKS.md) and [benchmarks/RESULTS.md](benchmarks/RESULTS.md).

## Where has it actually run?

| Platform | Status |
|---|---|
| Linux x86-64 | Everything is tested here (the full suite, benchmarks, valgrind-checked memory) |
| macOS Apple Silicon | Runs on a real M1 and on a CI runner (macOS 26); the whole suite passes there |
| Windows x86-64 | Runs natively on a GitHub Windows runner: examples, file/stdin/args tests and benchmarks |
| Linux arm64 | Builds and runs hello on a GitHub arm64 runner; the rest is checked in an emulator |
| macOS Intel | Written but **never run**; modern macOS rejects static executables, so this target is likely broken, and there's no release archive for it |

The emulator tests (`scripts/run_a64.py`, `scripts/run_win.py`, which need Python and `unicorn`) check that AArch64 and Windows programs print exactly what the x86-64 builds print.

## Optional extras

The default build has no C dependencies at all. If you build the compiler with `cargo build --features legacy-backends` you also get the older Cranelift/LLVM back ends, which unlock **`extern fn` + `--link`** (static C/C++/`no_std` Rust libraries) and **native plugins** in Rust and C++. Plugins are native code with full access to your process; read the trust model in [PLUGIN_ABI.md](PLUGIN_ABI.md) first. Guides: [static linking](LANGUAGE_SPEC.md#statically-linked-native-libraries-extern-fn) · [`examples/static_link`](examples/static_link) · [`plugins/`](plugins).

## Hacking on Flash

```bash
git clone https://github.com/Githuperson/Flash && cd Flash
cargo build --release          # the only requirement is Rust (depends on just serde, toml and sha2)
cargo test                     # ~70 tests; add --features legacy-backends for the interop ones
```

The compiler is `crates/flash/src/` (the native back end lives in `native/`), the runtime written in Flash is in `crates/flash/runtime/`, and the standard library is in `stdlib/`. Releases are built by `.github/workflows/release.yml` (run it by hand with a tag name, or push a `v*` tag).

## Docs

[LANGUAGE_SPEC.md](LANGUAGE_SPEC.md) (the language) · [ARCHITECTURE.md](ARCHITECTURE.md) (how the compiler works) · [ROADMAP.md](ROADMAP.md) (what's next) · [BENCHMARKS.md](BENCHMARKS.md) · [PLUGIN_ABI.md](PLUGIN_ABI.md)

MIT licensed. See `LICENSE`.
