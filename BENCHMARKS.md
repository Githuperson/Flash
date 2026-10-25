# Benchmarks

Flash is compared with Rust (`rustc -C opt-level=3`) and C++ (`g++ -O3`) on eight programs:
`int_loop`, `fib`, `mandelbrot`, `sieve`, `strings`, `alloc`, `nbody`, `matmul` (sources in `benchmarks/*.flash|rs|cpp`).

```bash
cargo build --workspace --release
python3 benchmarks/run.py --runs 7 --markdown benchmarks/RESULTS.md
```

The harness builds every variant, refuses to report a run whose output differs from the C++ output, takes the median of N runs,
and records machine, CPU and compiler versions. Nothing is hard-coded. Latest committed results: [benchmarks/RESULTS.md](benchmarks/RESULTS.md).

## Backends

| `flash build --release …` | Code generator | Needs at compile time | Measured vs Rust (8 programs) |
|---|---|---|---|
| *(default, `--backend auto`)* | LLVM if `clang` is installed, otherwise Cranelift | optional `clang` | see LLVM row, or Cranelift row |
| `--backend llvm` | clang -O3 on generated LLVM IR | `clang` | 0.8×–1.4× the run time of Rust |
| `--backend cranelift` | Cranelift (pure Rust) | nothing | 1.0×–2.6× the run time of Rust |

Compiled programs need no Flash runtime, interpreter or clang at run time either way.

## Honest summary

* Flash with LLVM is roughly on par with Rust on most of these programs, and slower on `nbody` (~1.3–1.4×) and `matmul` (~1.1×);
  it is slower than C++ on `fib`, `alloc` and `matmul`. Differences within ~10% on sub-second programs are noise.
* There is **no** workload here where Flash is demonstrably faster than Rust; do not read the sub-1.0 ratios as superiority.
* Not yet benchmarked: sorting, JSON, hash maps, file I/O, multithreading (these features do not exist yet; see ROADMAP.md).
* Flash checks list bounds and wraps on integer overflow; the C++ versions use unchecked access, so the comparison is not like-for-like.
