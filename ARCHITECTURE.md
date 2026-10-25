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
                             lowering ──► IR ──► optimiser ──► native back end ──► executable file
                          (drops, flags,   CFG over    const-fold, copy-prop,    instruction selection,    ELF (Linux) or
                           temps, glue,    virtual     DCE, jump threading,      register allocation,      Mach-O (macOS),
                           bounds checks)  registers   inlining, GVN, LICM       machine code + runtime    written directly
```

The default path contains no other program: no assembler, linker, C compiler, C library or LLVM. The optional legacy back ends (Cranelift,
LLVM via `clang`) can be compiled in with `--features legacy-backends`; they are described in §5.2.

The front end (everything up to and including the borrow checker) knows nothing about machine code. The IR is the
only interface to the back ends, so another code generator is "just" another `fn(&ir::Module) -> bytes`;
three exist today (native, and the optional Cranelift and LLVM ones).

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
| `opt.rs`, `gvn.rs`, `licm.rs`, `cfg.rs` | IR optimisation passes (cleanup, inlining, value numbering, loop-invariant motion), CFG and dominators |
| `native/` | **the default back end**: `asm.rs` x86-64 encoder, `regalloc.rs` live-range register allocator, `codegen.rs` instruction selection, `addrfold.rs`, `magic.rs` (division by constants), `link.rs` (runtime merge), `elf.rs` / `macho.rs` file writers |
| `crates/flash/runtime/*.flash` | the runtime library, written in Flash (see §6) |
| `codegen.rs`, `llvm.rs` | optional legacy back ends (feature `legacy-backends`): Cranelift, LLVM text via `clang` |
| `plugins.rs` | plugin manifest parsing and the descriptor embedded in executables |
| `fmt.rs` | token-based formatter (`flash fmt`) |
| `pkg.rs`, `bin/flashpkg.rs` | package manager |
| `main.rs` | the `flash` command-line tool |
| `crates/flash-rt` | the optional legacy runtime (Rust static library) used only by the legacy back ends, plugins and `extern fn` |
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
A control-flow graph of basic blocks over **mutable virtual registers** (not SSA; the optimiser numbers *values* instead, and the register allocator works on live ranges). Machine types only: `I8 I16 I32 I64 F64`; pointers are `I64`; booleans are `I8`. Instructions: constants, copy, binary/unary/compare, casts, load/store with offset,
call (function / runtime import / raw system call), address of static data or a stack slot, select. Terminators: jump, conditional branch, return, unreachable.

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
* **Alias classes:** loads and stores of list lengths, list data pointers, list elements (by machine type) and struct fields carry a `MemTag`. The native optimiser (value numbering, loop-invariant motion) treats different tags as non-aliasing and untagged accesses as aliasing everything, so a store to a `Float` element or a field no longer forces a reload of the list header; the LLVM back end turns tags into TBAA metadata.
* **Plugins:** a call builds a `FlashValue[]` in a stack slot and calls `fl_plugin_call`; a fallible function wraps the status into a `Result`.
* **`extern fn`:** a call to a C-ABI function widens integers to 64 bits, passes a `String` as `(ptr + 16, len)` and normalises a `Bool` result; the linker resolves the symbol (`--link`, `-l`, `-L`).

### Optimiser (`opt.rs`, `gvn.rs`, `licm.rs`)
Per function, iterated to a fixed point: constant folding and propagation of single-definition constants; local and global copy propagation; copy folding (`t = a + b; x = t` computes straight into `x`); algebraic identities;
folding of branches on constants; jump threading over empty blocks and over branches on copies of an already-tested condition (this restores plain control flow for `a and b`); merging of straight-line blocks;
unreachable-block removal; dead-code elimination of pure instructions, using block liveness so that assignments that are overwritten before being read disappear too.
Loops are **rotated** (a jump to a tiny test-and-branch block is replaced by a copy of the test) so a loop runs with a single conditional branch per iteration.

In **release** builds additionally:
* **inlining** of small (≤ 60 instructions), non-recursive, non-exported functions, repeated for up to three rounds. Calls on paths that end in `unreachable` (panics) are never inlined, and neither are functions that contain system calls (slow paths);
* **global value numbering over the dominator tree** (`gvn.rs`): registers map to value ids, equal computations become copies, loads are keyed by (base value, offset, alias tag) and the tag's *epoch*, which stores of the same tag, calls and untagged stores advance;
  a store forwards its value to later loads of the same address; a branch whose condition is known from a dominating branch becomes a jump (repeated bounds checks disappear). At joins and loop headers everything that can change on a path from the dominator is forgotten first;
* **loop-invariant code motion** (`licm.rs`): natural loops get a preheader; invariant arithmetic and loads of memory the loop cannot write (by alias tag), in blocks every loop exit passes through, move there; the invariant *prefix* of a loop body (leading blocks that only compute invariant values and test invariant conditions against panic paths) is peeled in front of the loop.

Dev builds run the cleanup passes only. The native back end's last IR pass, **address folding**, rewrites `p = base + i*8; load [p+off]` into one indexed load/store.

## 5. Back ends

### 5.1 The native back end (default)

`flash build` turns the optimised IR directly into an executable. Nothing else runs: no assembler, linker, C compiler, C library or LLVM, and the output depends on no library
(`ldd` says "not a dynamic executable"; a hello-world is about 13 KB).

* **Register allocation** (`native/regalloc.rs`): blocks are laid out in reverse post-order with panic paths last; liveness is computed per block and turned into *live ranges with holes*
  (so a value used only on a cold panic path does not occupy a register across the hot loop); vregs that are live across a call or system call may only use callee-saved registers; a linear scan assigns registers
  by start position, spilling the cheapest conflicting value (use count weighted by loop depth) when none is free. Single-definition constants are never allocated: they become immediates or are rematerialised.
* **Instruction selection** (`native/codegen.rs`): two-address forms with the destination coalesced where possible, comparison-and-branch fusion, `lea` for additions, constant divisors by multiplication with a magic number
  (`magic.rs`, checked against hardware division by a unit test) or shifts, saturating float→integer conversions, `floor`/`ceil` without SSE4.1, narrow integers kept zero-extended in registers.
* **Calling convention** (Flash's own, System-V-like): integer arguments in `rdi rsi rdx rcx r8 r9`, floats in `xmm0–7`, the rest on the stack; results in `rax`/`xmm0`; `rbx rbp r12–r15` and `xmm8–13` are callee-saved. Nothing outside the program calls into it, so no foreign ABI is involved.
* **Files**: a static ELF (`elf.rs`: three `PT_LOAD` segments, non-executable stack, a symbol table for debuggers) for Linux, a static Mach-O (`macho.rs`: `__PAGEZERO`, `__TEXT`, `__DATA`, `LC_UNIXTHREAD`) for macOS x86-64; for AArch64 the same writers produce ELF (`EM_AARCH64`, 16 KiB pages) and a *dynamic* Mach-O for arm64 (`LC_MAIN`, `LC_LOAD_DYLINKER`, a libSystem dependency, empty chained-fixup tables, PIE, 4 GiB `__PAGEZERO`; a CI probe showed macOS 26 kills static Mach-O files, so the program is loaded by dyld but still makes raw BSD system calls and never calls libSystem; the entry stub derives its stack block from `main`'s `argv`) with an ad-hoc code signature (`codesign.rs`: SHA-256 page hashes in a CodeDirectory, which Apple Silicon requires for every executable).
* **AArch64 back end** (`native/a64.rs` encoder, `a64gen.rs` instruction selection, shared register allocator with a per-architecture `RegConfig`): arguments in `x0–x7`/`d0–d7` (outside the allocation pool), `x9–x15`/`d16–d29` caller-saved, `x19–x28`/`d8–d15` callee-saved; data addresses by `adrp`+`add`; the runtime's platform layer (`__arch()`) picks Linux-aarch64 or BSD system-call numbers (`svc #0`, or `svc #0x80` with the number in `x16` and the carry flag as error indicator on macOS).
* **System calls**: Linux uses the `syscall` instruction directly; for macOS the code generator adds the BSD class bits and converts the carry-flag error convention to `-errno`.
* **Not supported by this back end**: `extern fn`/`--link`, native plugins (both need a C ABI and dynamic loading — use the legacy back ends), Windows and 32-bit. Division by constants uses the same magic numbers on AArch64 (`smulh`/`umulh`), checked against the x86-64 output in the emulator tests.

Measured behaviour: see `benchmarks/RESULTS.md`. Compile time is a few milliseconds of code generation plus the front end (the whole `flash build` of a small program takes 10–30 ms).

### 5.2 Optional legacy back ends (`--features legacy-backends`)

| | Cranelift | LLVM via `clang` (`--backend llvm`) |
|---|---|---|
| Integration | Rust crates linked into `flash`, output linked with `cc` and `flash-rt` | textual LLVM IR → `clang -O3 -c`, linked with `cc` |
| Needed for | `extern fn`, `--link`, native plugins, targets other than the native back end's | the same, with better optimisation |

They exist because the C ABI features cannot be provided by a standalone runtime, and as a cross-check: the test-suite runs every program through all back ends when they are compiled in.
They are not built by default (`cargo build` compiles neither Cranelift nor the Rust runtime), and `Backend::Auto` only selects them when the program needs them.

**Decision record.** The project first used Cranelift (fast to build, objects for many targets) and optionally LLVM. Both produce code that depends on a C linker and on a Rust runtime library, which conflicts with the goal of
standalone executables and a runtime that is not written in another language. The native back end replaces them for everything that does not need a C ABI. It is deliberately smaller than LLVM — no vectorisation, no
instruction scheduling, simple register allocation — and the benchmarks show where that costs time.

## 6. Runtime and executables

The runtime library is **written in Flash** (`crates/flash/runtime/*.flash`, embedded in the compiler with `include_str!`): memory allocator, strings, lists, integer and float formatting and parsing, math functions, files, paths,
environment, time, standard input and output, panics. It is compiled by the same pipeline as the user's program (with a few privileged `__` intrinsics for raw memory access, system calls, the address of its global state block and the
target OS, which only runtime code may use), merged with the program's IR, pruned to what is reachable, and optimised together with it. That is why `fl_alloc`'s free-list fast path inlines into user code.

* `plat.flash` — the platform layer: every operating-system request (read, write, open, mmap, exit, clock, sleep…) with the per-OS system-call numbers and flags; `__os()` is a compile-time constant, so only the code for the target remains.
* `sys.flash` — output buffering, panics, the **allocator** (size classes of 16 bytes up to 1 KiB, then powers of two up to 1 MiB, free lists per class, 4 MiB chunks from `mmap`; larger blocks are mapped directly; `fl_free` is given the size, so blocks have no header) and the process entry (`fl_start`).
* `text.flash` — strings, lists, results, integer parsing. `float.flash` — shortest-round-trip float printing (a fast path for short decimals, Dragon4 on a small big-number library otherwise) and correctly rounded parsing. `math.flash` — `exp ln sin cos tan pow`
  ported from the classic fdlibm algorithms, with an exact Payne–Hanek argument reduction for huge angles. `io.flash` — files, paths, environment, time, standard input.
* An allocation counter reports live blocks at exit when `FLASH_RT_LEAKCHECK` is set — the test-suite uses it to assert that programs free everything.

`crates/flash/tests/native_runtime.rs` checks float printing/parsing and the math functions against Rust's standard library on thousands of random inputs (math: at most 1 ulp from libm on the tested inputs).

Known differences from the previous Rust runtime: `upper`/`lower` change ASCII letters only; floating-point `parse` accepts what Rust's does; paths are handled as bytes; on macOS `now_ns` uses the wall clock with microsecond resolution.

The legacy runtime (`crates/flash-rt`, a Rust static library, with the plugin loader and the `flash-stub` loader) is only used with `--features legacy-backends`.

## 7. Tooling

* **`flash fmt`** is token based: it re-spaces tokens line by line, normalises indentation, and preserves comments and literals verbatim, so it cannot change the meaning of a program (the tests verify formatting is idempotent and behaviour-preserving over all repository programs).
* **`flash test`** compiles each file once with a generated dispatcher appended (`if args()[0] == "test_x": test_x()`) and runs every `test_*` function in its own process.
* **`flashpkg`** (`pkg.rs`) implements local-path packages, `flash.lock` with SHA-256 source hashes, native-package requirement checks, and `FLASH_PATH`/`FLASH_PLUGIN_PATH` injection.
* **Build cache (whole-program incremental compilation):** after the front end runs (always — it is a few milliseconds and is what reports errors), `build`/`run` hash every source text, the build options, the plugin descriptors and the identity of the compiler and runtime; on a hit the previously linked executable is copied and lowering, code generation and linking (the ~100 ms part) are skipped. **Per-module** incremental compilation is not implemented: changing one file recompiles the whole program. The natural next step is caching typed HIR/IR per module keyed by content hash.

## 8. Platform status

| Platform | Status |
|---|---|
| Linux x86-64 | **built, tested, benchmarked** with the native back end (all tests run here), and with the legacy back ends |
| macOS x86-64 (Intel, and Apple Silicon through Rosetta 2) | the native back end writes a static Mach-O (`--target x86_64-apple-darwin`); its structure is checked with LLVM's Mach-O reader, **it has never been executed on a Mac** |
| macOS ARM64, Linux AArch64 | the native back end writes a static ELF (Linux) and a dynamic signed Mach-O (macOS); Linux **run only in a CPU emulator** (Unicorn); macOS arm64 **runs in CI on a GitHub Apple Silicon runner, full test suite green** (not tried elsewhere). Emulator check: where examples, runtime float/math tests and benchmarks match the x86-64 output; never run on real hardware |
| Windows | not supported (the legacy plugin loader and library naming are written but untested) |

## 9. Known limitations / roadmap

Language: no tuples, maps, user generics, methods, closures, concurrency or `unsafe` (see LANGUAGE_SPEC §16). Compiler: no incremental compilation; generic function bodies are only checked when used;
`?` leaks owned temporaries evaluated earlier in the same expression; native stack overflow is not detected; the native back end is 1.0–2× slower than Rust on the compute benchmarks (see `benchmarks/RESULTS.md`), with no vectorisation.
Next steps in rough priority order: tuples and maps; user generics/traits; reference locals (`ref x = xs[i]`) with the same rules as borrowed parameters; per-module caching; static linking of native libraries;
process isolation for untrusted plugins; Windows/macOS CI; an LSP server built on the existing parser and checker; a thread-safe ownership model (`Send` transfer and channels).
