# Benchmark results

These tables are the unedited output of `python3 benchmarks/run.py --runs 9 --legacy-flash <flash built with --features legacy-backends>`
(the harness builds every program, checks that the outputs are identical to the C++ output, and measures; nothing is hard-coded).
Re-run it on your own machine: absolute times and even the ordering will differ, and this machine showed run-to-run noise of 10–30% on some programs (`nbody`, `matmul`).

**How to read them**

* `flash-dev` and `flash-release` are Flash's own native back end (`flash build`, `flash build --release`). `flash-release-cranelift` and `flash-release-llvm` are the *optional legacy* back ends
  (they need `--features legacy-backends`; LLVM also needs `clang`), included for comparison only. Rust uses `rustc -C opt-level=3`; C++ uses `g++ -O3` (and `-O0` for reference).
  No `-march=native` anywhere, no LTO, one core.
* The algorithms are the same in the three languages (see `benchmarks/*.flash|rs|cpp`), but the languages are *not* equivalent: Flash checks every list index and wraps on integer overflow;
  the C++ code uses unchecked `std::vector` access; Rust checks bounds. Flash structs are always heap-allocated (a `List[Body]` is a list of pointers), Rust and C++ store them inline.
  Flash programs use Flash's own allocator; the Rust and C++ programs use the system allocator (glibc `malloc`), which matters for `alloc` and `strings`.
* "Correct" is enforced: a run whose output differs from the C++ output is marked `✗WRONG` (none are).
* Compile times are wall-clock for the whole command (for Flash that includes producing the final executable: there is no separate link step). Peak memory is `ru_maxrss` measured by a tiny wrapper process.
  Native Flash executables contain no C library or Rust runtime, which is why they are tens of KiB and need 0.3 MiB for a trivial program.

**What the numbers say (for this machine and these 8 programs)**

* The native back end is within about 1.1–1.25× of Rust on `int_loop`, `mandelbrot` and `sieve`, 1.6× on `nbody`, 1.8× on `fib` and 2.2× on `matmul`. It is **faster** than Rust on `strings` (0.74×) and `alloc` (0.31×): the latter two are dominated by allocation, where
  Flash's size-class free-list allocator (a few inlined instructions) beats glibc `malloc`; that is a difference of allocators and allocation counts (Flash frees deterministically, no extra work), not evidence that Flash code in general is faster.
* The remaining gaps are known: no vectorisation or instruction scheduling; no tail-recursion-to-loop conversion (`fib`); list-of-lists indexing keeps bounds checks on the invariant row index (`matmul`); structs are heap blocks, so field accesses
  go through a pointer (`nbody`). Differences within ~10% on programs that run for 0.1 s are noise.
* Compared with the legacy back ends, native is similar to Cranelift (better on `alloc`/`strings` thanks to the runtime, usually within noise elsewhere) and 0.9–2× slower than LLVM, which wins clearly on `fib` and `matmul`.
* Flash is **not** shown to be faster than Rust or C++ on compute workloads.

---


* Date: 2026-10-25 15:32:52  
* Machine: Linux-6.18.44-fc-v153-x86_64-with-glibc2.39  
* CPU: Intel(R) Xeon(R) Processor @ 2.10GHz  
* Rust: rustc 1.97.0 (2d8144b78 2026-07-07)  
* C++: g++ (Ubuntu 13.3.0-6ubuntu2~24.04.1) 13.3.0  
* Runs per benchmark: 9 (median reported)

Run time = median wall-clock seconds of the compiled executable. Correct = output identical to the C++ -O3 build.

## Run time (seconds, lower is better)

| benchmark | flash-dev | flash-release | flash-release-cranelift | flash-release-llvm | rust -O | c++ -O3 | c++ -O0 |
|---|---|---|---|---|---|---|---|
| int_loop | 0.560 | 0.570 | 0.517 | 0.500 | 0.465 | 0.540 | 1.040 |
| fib | 0.230 | 0.240 | 0.309 | 0.124 | 0.136 | 0.075 | 0.345 |
| mandelbrot | 0.167 | 0.148 | 0.154 | 0.140 | 0.133 | 0.137 | 0.297 |
| sieve | 0.154 | 0.143 | 0.138 | 0.116 | 0.119 | 0.114 | 0.267 |
| strings | 0.048 | 0.048 | 0.064 | 0.063 | 0.066 | 0.051 | 0.105 |
| alloc | 0.185 | 0.085 | 0.235 | 0.248 | 0.272 | 0.135 | 0.719 |
| nbody | 0.754 | 0.331 | 0.414 | 0.329 | 0.209 | 0.325 | 3.216 |
| matmul | 0.058 | 0.045 | 0.043 | 0.022 | 0.020 | 0.013 | 0.150 |

## Flash release vs Rust and C++ (run time ratio; 1.00 = same, >1 means Flash is slower; native = `flash-release`, cranelift/llvm = the optional legacy back ends)

| benchmark | native / rust | native / c++ -O3 | cranelift / rust | llvm / rust | llvm / c++ -O3 |
|---|---|---|---|---|---|
| int_loop | 1.22x | 1.06x | 1.11x | 1.07x | 0.93x |
| fib | 1.76x | 3.20x | 2.27x | 0.91x | 1.65x |
| mandelbrot | 1.11x | 1.08x | 1.15x | 1.05x | 1.02x |
| sieve | 1.20x | 1.26x | 1.15x | 0.97x | 1.02x |
| strings | 0.74x | 0.94x | 0.98x | 0.97x | 1.23x |
| alloc | 0.31x | 0.63x | 0.86x | 0.91x | 1.84x |
| nbody | 1.59x | 1.02x | 1.98x | 1.57x | 1.01x |
| matmul | 2.24x | 3.39x | 2.10x | 1.08x | 1.64x |

## Compile time (seconds)

| benchmark | flash-dev | flash-release | flash-release-cranelift | flash-release-llvm | rust -O | c++ -O3 | c++ -O0 |
|---|---|---|---|---|---|---|---|
| int_loop | 0.01 | 0.02 | 0.01 | 0.14 | 0.13 | 0.07 | 0.06 |
| fib | 0.01 | 0.02 | 0.01 | 0.13 | 0.15 | 0.13 | 0.06 |
| mandelbrot | 0.02 | 0.02 | 0.01 | 0.16 | 0.13 | 0.07 | 0.06 |
| sieve | 0.02 | 0.03 | 0.01 | 0.13 | 0.15 | 0.22 | 0.21 |
| strings | 0.02 | 0.12 | 0.01 | 0.16 | 0.32 | 0.37 | 0.37 |
| alloc | 0.02 | 0.06 | 0.01 | 0.14 | 0.15 | 0.35 | 0.31 |
| nbody | 0.02 | 0.09 | 0.01 | 0.19 | 0.21 | 0.43 | 0.35 |
| matmul | 0.02 | 0.08 | 0.01 | 0.16 | 0.18 | 0.29 | 0.24 |

## Peak memory (MiB, max RSS)

| benchmark | flash-dev | flash-release | flash-release-cranelift | flash-release-llvm | rust -O | c++ -O3 | c++ -O0 |
|---|---|---|---|---|---|---|---|
| int_loop | 0.3 | 0.3 | 2.7 | 2.2 | 2.2 | 1.5 | 1.5 |
| fib | 0.3 | 0.3 | 2.8 | 2.2 | 2.2 | 1.5 | 1.5 |
| mandelbrot | 0.3 | 0.3 | 2.8 | 2.2 | 2.2 | 1.5 | 1.5 |
| sieve | 19.0 | 19.0 | 21.8 | 20.9 | 21.0 | 22.4 | 22.4 |
| strings | 27.7 | 27.7 | 31.1 | 30.1 | 36.2 | 41.4 | 41.4 |
| alloc | 0.3 | 0.3 | 2.8 | 2.2 | 2.2 | 3.7 | 3.7 |
| nbody | 0.3 | 0.3 | 2.8 | 2.2 | 2.2 | 3.7 | 3.7 |
| matmul | 3.5 | 3.5 | 5.8 | 5.0 | 5.0 | 5.8 | 5.8 |

## Executable size (KiB)

| benchmark | flash-dev | flash-release | flash-release-cranelift | flash-release-llvm | rust -O | c++ -O3 | c++ -O0 |
|---|---|---|---|---|---|---|---|
| int_loop | 13 | 13 | 701 | 329 | 335 | 14 | 16 |
| fib | 13 | 13 | 701 | 329 | 335 | 14 | 16 |
| mandelbrot | 13 | 13 | 701 | 329 | 335 | 14 | 16 |
| sieve | 14 | 17 | 701 | 337 | 335 | 14 | 20 |
| strings | 18 | 33 | 702 | 345 | 340 | 18 | 29 |
| alloc | 14 | 21 | 702 | 341 | 338 | 14 | 26 |
| nbody | 18 | 29 | 704 | 345 | 339 | 14 | 24 |
| matmul | 14 | 29 | 703 | 345 | 338 | 14 | 39 |
