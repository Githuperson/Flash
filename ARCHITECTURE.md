# Flash compiler architecture

This document describes how the Flash v0.1 compiler is put together, why the pieces look the way they do,
and what is and is not finished. For the language itself see [LANGUAGE_SPEC.md](LANGUAGE_SPEC.md); for native
plugins see [PLUGIN_ABI.md](PLUGIN_ABI.md).

## 1. Pipeline

```
 .flash source ──► lexer ──► parser ──► AST ──► resolver ──► checker ──► HIR ──► borrow checker
   (+ imports,      tokens     (recovers     names, scopes,     types, inference,    typed,       moves, aliasing,
    plugin                      from errors)  duplicates         ownership modes     monomorphic  loop locks
    manifests)                                                                       instances
                                   ┌──────────────────────────────────────────────────────────────┘
                                   ▼
                             lowering ──► IR ──► optimiser ──► code generator ──► object file ──► linker ──► executable
                          (drops, flags,   CFG over    const-fold, copy-prop,    Cranelift (default)        `cc` + libflash_rt.a
                           temps, glue,    virtual     DCE, CFG cleanup,         or LLVM text → clang
                           bounds checks)  registers   inlining (release)
```

The front end (everything up to and including the borrow checker) knows nothing about machine code. The IR is the
only interface to the back ends, so another code generator is "just" another `fn(&ir::Module) -> object bytes`;
two exist today.

## 2. Source layout

| Path | Role |
|---|---|
| `crates/flash/src/source.rs`, `diag.rs` | source files, spans, diagnostics with excerpts, "did you mean" suggestions |
| `lexer.rs` | tokens, indentation (`INDENT`/`DEDENT`), f-strings, numeric literals |
| `parser.rs`, `ast.rs` | recursive descent parser with error recovery; f-strings are desugared to `"…" + str(x) + …` |
| `module.rs`, `driver.rs` | loading files, `import`, plugin manifests, running the stages, linking |
| `resolve.rs` | name resolution and scoping, duplicate definitions, `break` outside loops |
| `types.rs`, `hir.rs` | type representation; typed high-level IR |
| `check.rs`, `check_builtins.rs`, `builtins.rs` | type inference/checking, ownership modes, built-in function typing |
| `borrowck.rs` | flow-sensitive move analysis, aliasing and loop-lock checks |
| `ir.rs`, `rtsigs.rs` | low-level IR; signatures of runtime functions |
| `lower.rs`, `lower_builtins.rs` | HIR → IR, including every ownership-driven drop |
| `opt.rs` | IR optimisation passes |
| `codegen.rs` | Cranelift back end; `llvm.rs` — LLVM text back end |
| `plugins.rs` | plugin manifest parsing and the descriptor embedded in executables |
| `fmt.rs` | token-based formatter (`flash fmt`) |
| `pkg.rs`, `bin/flashpkg.rs` | package manager |
| `main.rs` | the `flash` command-line tool |
| `crates/flash-rt` | the runtime library linked into every program (strings, lists, I/O, plugin loader) |
| `crates/flash-plugin-sdk` | ABI structures and the `flash_plugin!` macro for Rust plugins |
| `include/flash_plugin.h`, `.hpp` | the same for C and C++ |
| `stdlib/` | standard library modules written in Flash |
| `plugins/` | the Rust and C++ example plugins |
| `tests/`, `crates/flash/tests/` | end-to-end programs, error programs, Rust integration tests |
| `benchmarks/` | Flash/Rust/C++ benchmark programs and the measuring harness |

## 3. Front end

### Lexing and parsing
Indentation is handled in the lexer, which emits `INDENT`/`DEDENT`/`NEWLINE` tokens from a stack of indentation widths
(rules in LANGUAGE_SPEC §1). Newlines inside brackets are suppressed. The parser is hand-written recursive descent with
precedence climbing. On a syntax error it records a diagnostic, skips to the end of the logical line (plus any indented block below
it) and continues, so several errors are reported per run.

### Name resolution (`resolve.rs`)
A separate pass over the AST of *every* function (including generic ones never instantiated) with block-scoped variable
tracking. It reports unknown names with suggestions, calls of non-functions, duplicate definitions, unknown types, and misplaced
`break`/`continue`. Because functions are checked lazily by the type checker, this is what guarantees that undefined names in unused generic functions are still found.

### Type checking and inference (`check.rs`)
* A bidirectional checker: `expr(e, expected, ctx)` returns a typed HIR expression. `expected` lets integer literals take the type required by context.
* **Functions are monomorphised at the type-checking stage.** Parameters without annotations take the argument types of each call; the body
  is checked once per distinct `(function, argument types)` *instance*, producing a separate typed `HFunc`. This gives the effect of
  generics for functions (templates, Zig-style) without a constraint solver, keeps error messages concrete ("Cannot multiply Float and Int" at the line that
  fails, with a note pointing to the call), and means all later stages only ever see fully typed code. The price is that unused generic bodies are not type-checked.
* Return types come from `return` statements (first return fixes the type, later ones must agree). Recursive calls to an instance whose return type is still
  unknown use the type of the first non-recursive return.
* Every use of a variable is classified as **copy**, **move** or **borrow** while the HIR is built (`UseMode`), according to the type (Copy types are
  copied) and the context (assignment/return/`own` argument consume; operands and borrowed arguments don't). Moving out of borrowed parameters, loop
  variables, fields and list elements is rejected here.

### Borrow checker (`borrowck.rs`)
Runs on the HIR of every function: a forward data-flow analysis over the set of possibly-moved variables with joins at control-flow merges,
fixed-point iteration for loops (so "moved in the previous iteration" is caught), unreachable-path tracking after `return`/`break`/`continue`,
plus the call-aliasing check and the "list is being iterated" lock. The soundness argument is in LANGUAGE_SPEC §10: borrows cannot be stored, so they cannot outlive their owner, and exclusive access is unique.

## 4. Middle end

### IR (`ir.rs`)
A control-flow graph of basic blocks over **mutable virtual registers** (not SSA — the Cranelift frontend builds SSA, LLVM gets `alloca`s that `mem2reg`
promotes). Machine types only: `I8 I16 I32 I64 F64`; pointers are `I64`; booleans are `I8`. Instructions: constants, copy, binary/unary/compare, casts, load/store with offset,
call (function / runtime import), address of static data or a stack slot, select. Terminators: jump, conditional branch, return, unreachable.

### Lowering (`lower.rs`) — where ownership becomes code
* **Heap layouts:** `String = [len][cap][bytes…]` in one allocation (`cap == -1` marks read-only literals, which are never freed);
  `List = [len][cap][data*]` with `stride`-sized elements; struct = one allocation with fields at natural alignment in declaration order;
  `Result = [tag][value][error String*]`.
* **Scopes and drops.** Lowering keeps a stack of scopes listing the owned locals declared so far. Leaving a scope — normally or through `break`, `continue`, `return`, or `?` — emits drops
  for exactly the owned locals that are live in the scopes being exited, in reverse declaration order.
* **Drop flags.** A variable that is moved somewhere in the function gets an `I8` flag, set when initialised and cleared at each move; its drops become `if flag { drop }`.
  Variables never moved get unconditional drops (no flag).
* **Temporaries.** An owned value produced by an expression and used only as an operand (`len(a + b)`, `say(f(x))`) is registered as a temporary and dropped at the end of the statement (or of the
  condition, for `if`/`while`), including on early exits.
* **Drop and clone glue** are synthesised per type on demand as IR functions (`List[Struct{String,…}]` loops over the elements, drops each field, frees the buffers); recursion through types is handled by reserving the function slot before generating the body.
* **Checks:** list indexing is bounds checked (`index <u len`, one unsigned compare); integer division and remainder test for zero and for `MIN / -1`; both call small panic blocks.
* **Bounds-check elimination:** `xs[i]` inside `for i in 0..len(xs)` loses its check if the loop body provably does not change `xs` (no `append`/`pop`/assignment/move/`mut` argument) or `i`.
* **Alias classes:** loads and stores of list lengths, list data pointers, list elements (by machine type) and struct fields carry a `MemTag`. The LLVM back end turns tags into TBAA metadata and the Cranelift back end into alias regions, so a store to a `Float` element or a field no longer forces a reload of the list header; this alone made matrix multiplication 2.2× faster under LLVM (see `benchmarks/RESULTS.md`).
* **Plugins:** a call builds a `FlashValue[]` in a stack slot and calls `fl_plugin_call`; a fallible function wraps the status into a `Result`.
* **`extern fn`:** a call to a C-ABI function widens integers to 64 bits, passes a `String` as `(ptr + 16, len)` and normalises a `Bool` result; the linker resolves the symbol (`--link`, `-l`, `-L`).

### Optimiser (`opt.rs`)
Per function, iterated to a fixed point: constant folding and propagation of single-definition constants; copy propagation; algebraic identities; folding of branches on constants;
jump threading over empty blocks; merging of straight-line blocks; unreachable-block removal; dead-code elimination of pure instructions. In **release** builds additionally: **inlining** of small
(≤ 60 instructions), non-recursive, non-exported functions (including drop glue), repeated for up to three rounds, followed by another cleanup; unused functions are then removed.
Dev builds run the cleanup passes only (they are cheap and the Cranelift verifier is on).

## 5. Back ends and the backend decision

| | Cranelift (default) | LLVM via `clang` (`--backend llvm`) |
|---|---|---|
| Integration | Rust crates, linked into `flash` | the compiler prints textual LLVM IR and runs `clang -O3 -c` |
| System requirements | none beyond a C linker (`cc`) | `clang` at compile time |
| Compile time of a small program | ~5 ms code generation | tens of ms (process start + LLVM pipeline) — see benchmarks |
| Generated code | simple optimiser (egraph rules, GVN, LICM, no auto-vectorisation, little inlining) | LLVM -O3 |
| Targets exercised here | object files for x86-64 Linux/macOS/Windows and AArch64 Linux/macOS | whatever the installed clang supports |

**Decision record.** The task asks to evaluate both. Cranelift was chosen as the default because it is the only option that
works with nothing but `cargo build`: it is a library, builds in about two and a half minutes together with the rest of the project, gives millisecond code generation
(compile + link of a program takes ~0.1 s, dominated by the system linker) and supports AArch64 and x86-64 object output for Linux, macOS and Windows from the same
build — which is what the macOS-ARM64-first goal needs. Using LLVM through the `llvm-sys`/`inkwell` crates would tie `cargo build` to a matching LLVM development
installation (LLVM 18 is installed in this environment, but a user's machine may differ) and multiply build time. Because the IR is backend independent, an LLVM option for release builds
was still worth having, so it is implemented without linking LLVM: `llvm.rs` prints textual IR (virtual registers as `alloca`, narrow integers `zeroext` for the C ABI, shifts masked to Flash's
semantics, saturating float→int via intrinsics) and invokes `clang`. The test-suite runs every program through it when `clang` is present. Measured effect on run time: see `benchmarks/RESULTS.md`
(the numbers are produced by `benchmarks/run.py`, nothing is hard-coded).

## 6. Runtime and linking

`crates/flash-rt` is a Rust `staticlib`/`rlib` that exports C-ABI functions (`fl_*`, listed with their signatures in `rtsigs.rs`) and the program's `main`, which records the
arguments, calls the compiled `flash_main`, flushes buffered output and exits. It provides string and list primitives, number formatting, buffered output, file and path functions,
the monotonic clock, process exit and runtime errors, and the **plugin loader** (`plugin.rs`; `libloading`, SHA-256 integrity check). An allocation counter reports live `fl_alloc`
allocations at exit when `FLASH_RT_LEAKCHECK` is set — the test-suite uses it to assert that programs free everything.

Executables are linked with the system C compiler driver (`cc`, override with `FLASH_CC`): `cc program.o libflash_rt.a -lpthread -ldl -lm` on Linux, `-lm -framework Security` on macOS
(Windows linking is not implemented). The runtime library is found next to the `flash` executable (`target/<profile>/libflash_rt.a`) or built on demand with
`cargo build -p flash-rt` in a development checkout. The output is a normal native executable: no interpreter, no bytecode, no JIT; plugins are the only run-time dependency and only if the program uses them.

## 7. Tooling

* **`flash fmt`** is token based: it re-spaces tokens line by line, normalises indentation, and preserves comments and literals verbatim, so it cannot change the meaning of a program (the tests verify formatting is idempotent and behaviour-preserving over all repository programs).
* **`flash test`** compiles each file once with a generated dispatcher appended (`if args()[0] == "test_x": test_x()`) and runs every `test_*` function in its own process.
* **`flashpkg`** (`pkg.rs`) implements local-path packages, `flash.lock` with SHA-256 source hashes, native-package requirement checks, and `FLASH_PATH`/`FLASH_PLUGIN_PATH` injection.
* **Build cache (whole-program incremental compilation):** after the front end runs (always — it is a few milliseconds and is what reports errors), `build`/`run` hash every source text, the build options, the plugin descriptors and the identity of the compiler and runtime; on a hit the previously linked executable is copied and lowering, code generation and linking (the ~100 ms part) are skipped. **Per-module** incremental compilation is not implemented: changing one file recompiles the whole program. The natural next step is caching typed HIR/IR per module keyed by content hash.

## 8. Platform status

| Platform | Status |
|---|---|
| Linux x86-64 | **built, tested, benchmarked** (all tests run here) |
| Linux AArch64, macOS (x86-64, ARM64), Windows x86-64 | the code generator produces correct-format object files (`crates/flash/tests/cli.rs` checks the headers); **linking and running were not tested** — no such machines were available. macOS linking uses `cc … -lm -framework Security`; narrow integer arguments are zero-extended as the Apple ARM64 ABI requires |
| Windows | the linker driver is not implemented; the plugin loader and library naming (`.dll`) are written but untested |

## 9. Known limitations / roadmap

Language: no tuples, maps, user generics, methods, closures, concurrency or `unsafe` (see LANGUAGE_SPEC §16). Compiler: no incremental compilation; generic function bodies are only checked when used;
`?` leaks owned temporaries evaluated earlier in the same expression; native stack overflow is not detected; Cranelift output is slower than LLVM, by 1.0–2.5× on the benchmark programs (measured in `benchmarks/RESULTS.md`).
Next steps in rough priority order: tuples and maps; user generics/traits; reference locals (`ref x = xs[i]`) with the same rules as borrowed parameters; per-module caching; static linking of native libraries;
process isolation for untrusted plugins; Windows/macOS CI; an LSP server built on the existing parser and checker; a thread-safe ownership model (`Send` transfer and channels).
