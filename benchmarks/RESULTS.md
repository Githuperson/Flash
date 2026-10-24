# Benchmark results

These tables are the unedited output of `python3 benchmarks/run.py --runs 7` (the harness builds every program, checks that the outputs
are identical, and measures; nothing is hard-coded). Re-run it on your own machine: absolute times and even the ordering will differ.

**How to read them**

* Flash programs are compiled with `flash build` (dev) and `flash build --release` (Cranelift) or `--release --backend llvm` (clang -O3 on the generated LLVM IR).
  Rust uses `rustc -C opt-level=3`; C++ uses `g++ -O3` (and `-O0` for reference). No `-march=native` anywhere, no LTO, one core.
* The algorithms are the same in the three languages (see `benchmarks/*.flash|rs|cpp`), but the languages are *not* equivalent: Flash checks every list index and wraps on integer overflow;
  the C++ code uses unchecked `std::vector` access; Rust checks bounds. Flash structs are always heap-allocated (a `List[Body]` is a list of pointers), Rust and C++ store them inline —
  this, not the code generator, is the main reason for the `alloc` gap. Flash `nbody` indexes the list for every field access because Flash has no reference locals.
* "Correct" is enforced: a run whose output differs from the C++ output is marked `✗WRONG` (none are).
* Compile times are wall-clock for the whole command including linking with the system linker (~0.05–0.1 s of every Flash build is `cc`). Peak memory is `ru_maxrss` measured by a tiny wrapper process.
  The executable size of Flash programs is dominated by the Rust runtime (`libflash_rt.a`, ~330 KiB stripped); dev builds are not stripped.
* Single machine, single run of the harness, 7 repetitions: treat differences of a few percent as noise.

**What the numbers say (for this machine and these 8 programs)**

* With the Cranelift backend Flash is between 1.0× and 2.4× the run time of Rust and between 1.1× and 3.9× that of C++ `-O3`. With the LLVM backend it is between 0.89× and 1.31× of Rust and 0.98×–2.14× of C++. Ratios within roughly ±10% on programs that run for about 0.1 s (`fib` 0.94×, `sieve` 0.92×, `strings` 0.95×, `alloc` 0.89× against Rust) are measurement noise, not evidence that Flash is faster; no claim of superiority is made. The clear Flash deficits are `nbody` (1.31× Rust under LLVM, 1.71× under Cranelift) and, against C++, `fib`, `matmul` and `alloc`.
* The biggest Cranelift costs are call-heavy code (`fib`: no inlining of recursive calls, unlike gcc), list-of-list indexing (`matmul`), and the lack of loop-invariant hoisting of list headers.
  Type-based alias information (added after the first measurement round, see ARCHITECTURE.md) cut `matmul` from 0.034 s to 0.016 s with LLVM and `nbody` from 0.29 s to 0.21 s.
  A first round before the runtime fix for integer formatting and allocation counting (relaxed atomic) and before alias tags measured `alloc` at 0.36 s and `matmul` at 0.034 s (LLVM).
* Compile times: a Flash build takes 0.05–0.13 s (most of it the linker); that is faster than rustc and g++ -O3 on the larger programs, and the dev/release difference is small.
  This is the measured benefit of using Cranelift as the default backend.
* Remaining gap to C++ on `alloc` (2.4×) comes from allocating one heap block per struct and per string, and from the generic `malloc` path through the runtime.

---


* Date: 2026-10-24 10:36:36  
* Machine: Linux-6.18.44-fc-v153-x86_64-with-glibc2.39  
* CPU: Intel(R) Xeon(R) Processor  
* Rust: rustc 1.97.0 (2d8144b78 2026-07-07)  
* C++: g++ (Ubuntu 13.3.0-6ubuntu2~24.04.1) 13.3.0  
* Runs per benchmark: 7 (median reported)

Run time = median wall-clock seconds of the compiled executable. Correct = output identical to the C++ -O3 build.

## Run time (seconds, lower is better)

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

## Flash release vs Rust and C++ (run time ratio; 1.00 = same, >1 means Flash is slower; cranelift = `flash-release`, llvm = `flash-release-llvm`)

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

## Compile time (seconds)

| benchmark | flash-dev | flash-release | flash-release-llvm | rust -O | c++ -O3 | c++ -O0 |
|---|---|---|---|---|---|---|
| int_loop | 0.09 | 0.05 | 0.10 | 0.09 | 0.05 | 0.04 |
| fib | 0.10 | 0.05 | 0.10 | 0.09 | 0.07 | 0.04 |
| mandelbrot | 0.09 | 0.06 | 0.10 | 0.10 | 0.05 | 0.04 |
| sieve | 0.10 | 0.05 | 0.10 | 0.11 | 0.16 | 0.15 |
| strings | 0.09 | 0.06 | 0.10 | 0.22 | 0.30 | 0.22 |
| alloc | 0.09 | 0.06 | 0.11 | 0.13 | 0.26 | 0.21 |
| nbody | 0.10 | 0.06 | 0.13 | 0.14 | 0.24 | 0.22 |
| matmul | 0.10 | 0.06 | 0.12 | 0.14 | 0.24 | 0.19 |

## Peak memory (MiB, max RSS)

| benchmark | flash-dev | flash-release | flash-release-llvm | rust -O | c++ -O3 | c++ -O0 |
|---|---|---|---|---|---|---|
| int_loop | 2.2 | 2.2 | 2.2 | 2.2 | 1.6 | 1.6 |
| fib | 2.2 | 2.2 | 2.2 | 2.2 | 1.6 | 1.6 |
| mandelbrot | 2.2 | 2.2 | 2.2 | 2.2 | 1.6 | 1.6 |
| sieve | 20.9 | 20.9 | 20.9 | 21.0 | 22.4 | 22.4 |
| strings | 30.1 | 30.1 | 30.1 | 36.2 | 41.5 | 41.5 |
| alloc | 2.2 | 2.2 | 2.2 | 2.2 | 3.8 | 3.8 |
| nbody | 2.2 | 2.2 | 2.2 | 2.2 | 3.7 | 3.7 |
| matmul | 5.1 | 5.0 | 5.1 | 5.0 | 5.8 | 5.9 |

## Executable size (KiB)

| benchmark | flash-dev | flash-release | flash-release-llvm | rust -O | c++ -O3 | c++ -O0 |
|---|---|---|---|---|---|---|
| int_loop | 4207 | 329 | 329 | 335 | 14 | 16 |
| fib | 4207 | 329 | 329 | 335 | 14 | 16 |
| mandelbrot | 4207 | 329 | 329 | 335 | 14 | 16 |
| sieve | 4229 | 345 | 337 | 335 | 14 | 20 |
| strings | 4229 | 345 | 345 | 340 | 18 | 29 |
| alloc | 4220 | 337 | 341 | 338 | 14 | 26 |
| nbody | 4233 | 345 | 345 | 339 | 14 | 24 |
| matmul | 4229 | 345 | 345 | 338 | 14 | 39 |
