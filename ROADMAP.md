# Roadmap

Status key: **done** = implemented and covered by automated tests on Linux x86-64; **partial**; **todo**.
Flash is an ahead-of-time compiled language: `flash build` emits a native executable; there is no interpreter or VM.

| Milestone | Status | Notes |
|---|---|---|
| 1. Foundation: lexer, parser, docs, tests | **done** | ARCHITECTURE.md, LANGUAGE_SPEC.md |
| 2. Minimal language: ints, bools, floats, strings, lists, structs, functions, loops, conditionals, `Result` | **done** | examples/ |
| 3. Native compiler: typed IR, machine code, executable files, diagnostics | **done** | Own x86-64 code generator and ELF/Mach-O writers (`crates/flash/src/native`): no LLVM, Cranelift, assembler, linker or C library. Optional legacy Cranelift/LLVM back ends behind `--features legacy-backends` |
| 4. Performance: optimisation passes, release mode, Rust/C++ benchmarks | **partial** | Passes: folding, copy-prop, jump threading, loop rotation, inlining (incl. one-level expansion of small self-recursive functions), GVN with alias classes, LICM + loop peeling, constant-division strength reduction, addressing-mode folding. On par with or within 1.1–2.4× of Rust on 6/8 benchmarks (`fib` 0.93×), faster on 2 (allocator). Gaps: no vectorisation, no instruction scheduling, no tail-recursion elimination, structs always heap-allocated |
| 5. Standard library | **partial** | Runtime and library written in Flash: strings, math, lists, files, paths, env, stdin, time. Missing: maps, sets, JSON, networking, random, crypto |
| 6. Production foundations: generics, concurrency, packages | **partial** | local packages + lockfile done; generics, traits, closures, threads, `unsafe` not started |
| 7. Self-hosting | **todo** | The compiler is written in Rust; the *runtime* is now written in Flash (a first step: the Flash compiler already compiles ~2,500 lines of systems code). Needs maps, generics, enums/sum types and richer strings before the compiler itself can move |
| 8. Ecosystem: targets, plugins, LSP | **partial** | macOS x86-64 target written (untested on hardware); plugins and `extern fn` only via the legacy back ends; AArch64 native back end (Linux ELF, macOS arm64 Mach-O) Linux ELF checked in an emulator, macOS arm64 Mach-O runs on a CI Apple Silicon runner; no LSP |

## Next concrete steps (in order)

1. Run the macOS x86-64 output on a real Mac (Intel or Rosetta) and fix what the kernel rejects; and the Linux AArch64 output on real hardware (so far only in an emulator); a Windows ARM64 target; a dynamic x86-64 Mach-O (the static one is probably rejected by current macOS as well); then a peephole pass for AArch64.
2. Inline struct layout (value types) to close the `nbody` gap; peel invariant bounds checks out of nested loops (`matmul`: loads of `b[k]` stay in the loop because LICM runs before loop rotation; hoisting them made register pressure worse in the huge inlined `multiply`, so it needs a smarter register allocator first).
3. Maps, sets, tuples and enums (prerequisites for JSON and for self-hosting).
4. Tail-recursion elimination and better register allocation across calls (`fib`); SIMD for numeric loops.
5. Threads and synchronisation primitives; add multithreaded benchmarks.
