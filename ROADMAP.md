# Roadmap

Status key: **done** = implemented and covered by automated tests on Linux x86-64; **partial**; **todo**.
Flash is an ahead-of-time compiled language: `flash build` emits a native executable; there is no interpreter or VM.

| Milestone | Status | Notes |
|---|---|---|
| 1. Foundation: lexer, parser, docs, tests | **done** | ARCHITECTURE.md, LANGUAGE_SPEC.md |
| 2. Minimal language: ints, bools, floats, strings, lists, structs, functions, loops, conditionals, `Result` | **done** | examples/ |
| 3. Native compiler: typed IR, object code, linking, diagnostics | **done** | Cranelift and LLVM(clang) backends; built-in linker on Linux x86-64 (no C compiler needed) |
| 4. Performance: optimisation passes, release mode, Rust/C++ benchmarks | **partial** | const folding, DCE, inlining, bounds-check elimination, TBAA. `--release` now picks LLVM automatically when clang exists. Gaps: no loop-invariant hoisting / vectorisation in our own IR, structs always heap-allocated (`alloc`, `nbody`) |
| 5. Standard library | **partial** | strings, math, lists, files, paths, env, stdin, time. Missing: maps, sets, JSON, networking, random, crypto |
| 6. Production foundations: generics, concurrency, packages | **partial** | local packages + lockfile done; generics, traits, closures, threads, `unsafe` not started |
| 7. Self-hosting | **todo** | The compiler is written in Rust. Needs maps, generics, enums/sum types and richer strings first. No self-hosting claim is made |
| 8. Ecosystem: targets, plugins, LSP | **partial** | Rust/C++ plugin ABI done; AArch64/macOS/Windows codegen exists but linking is untested; no LSP |

## Next concrete steps (in order)

1. Inline struct layout (value types) to close the `alloc`/`nbody` gap.
2. Maps, sets, tuples and enums (prerequisites for JSON and for self-hosting).
3. Loop-invariant code motion and list-header hoisting in `opt.rs`; re-measure before keeping each pass.
4. Threads and synchronisation primitives; add multithreaded benchmarks.
5. Verify macOS/AArch64 end to end; start a Flash-in-Flash port of the lexer as the first self-hosting step.
