# Benchmarks

Flash is compared with Rust (`rustc -C opt-level=3`) and C++ (`g++ -O3`) on eight programs:
`int_loop`, `fib`, `mandelbrot`, `sieve`, `strings`, `alloc`, `nbody`, `matmul` (sources in `benchmarks/*.flash|rs|cpp`).

```bash
cargo build --workspace --release
python3 benchmarks/run.py --runs 7 --markdown benchmarks/RESULTS.md
```

The harness builds every variant, refuses to report a run whose output differs from the C++ output, takes the median of N runs,
and records machine, CPU and compiler versions. Nothing is hard-coded. Latest committed results: [benchmarks/RESULTS.md](benchmarks/RESULTS.md).

## Back ends

| `flash build --release …` | Code generator | Needs at compile time |
|---|---|---|
| *(default)* | Flash's own native back end | nothing |
| `--backend llvm` | clang -O3 on generated LLVM IR (legacy, `--features legacy-backends`) | `clang`, a C linker |
| `--backend cranelift` | Cranelift (legacy, same feature) | a C linker |

Compiled programs need no Flash runtime, interpreter, clang or C library at run time on the default path. See [benchmarks/RESULTS.md](benchmarks/RESULTS.md) for the measurements and what they do and do not show.

## Honest summary

* The native back end is within ~10–25% of Rust on `int_loop`, `mandelbrot`, `sieve`; 1.6–1.8× on `nbody`, `fib`; 2.2× on `matmul`; faster on `strings` and `alloc` (allocator differences, see RESULTS.md).
* There is **no** compute workload here where Flash is demonstrably faster than Rust; sub-1.0 ratios on `strings`/`alloc` compare Flash's allocator with glibc `malloc`.
* Not yet benchmarked: sorting, JSON, hash maps, file I/O, multithreading (these features do not exist yet; see ROADMAP.md).
* Flash checks list bounds and wraps on integer overflow; the C++ versions use unchecked access, so the comparison is not like-for-like.
