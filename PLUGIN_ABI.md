# Flash Native Plugin ABI — version 1

This document specifies how Flash programs call native code, and how to write a plugin in Rust or C++.
Everything here is implemented and tested: `plugins/rust/fast_math` (Rust), `plugins/cpp/geometry` (C++),
`examples/plugins_demo.flash`, and `crates/flash/tests/plugins.rs`.

> **Security warning.** A plugin is native code loaded into the Flash program's own process. It runs with
> the program's full privileges and can read or write any memory, files and network the program can.
> ABI validation (below) catches *mistakes* — wrong version, missing or mismatched functions, a file that
> changed since it was pinned — it cannot make an untrusted or malicious library safe. Only load plugins you trust.
> See [§9 Trust model and isolation](#9-trust-model-provenance-and-isolation).

## 1. Overview

```
 Flash program (compiled, native)                     plugin (.so / .dylib / .dll)
 ┌───────────────────────────────┐                   ┌───────────────────────────────┐
 │ fast_math.add(40, 2)          │                   │ flash_plugin_entry_v1()        │
 │   build FlashValue[] on stack │   fl_plugin_call  │   -> FlashPluginInfo           │
 │   ──────────────────────────► │ ───────────────►  │      funcs[i].func(args,n,ret) │
 │   read FlashValue result      │ ◄───────────────  │      free_buf(ptr,len)         │
 └───────────────────────────────┘   int32 status    └───────────────────────────────┘
```

* The boundary is **plain C**: a versioned entry point returning a static table of functions. No Rust or C++ types cross it.
* Every plugin function has the same C signature:
  `int32_t f(const FlashValue* args, uint32_t nargs, FlashValue* ret)`.
  Values are passed as tagged `FlashValue`s, so the loader can verify types, and one function pointer type serves every function.
* The Flash compiler learns each function's Flash-level signature from a **manifest** (`<name>.flashplugin.toml`) at compile time
  — it never loads the plugin — and the runtime re-validates the plugin against the manifest when the program starts.
* Because the entry point and tables are looked up by name at run time, **a compatible plugin can be replaced
  without rebuilding the Flash program** (same ABI version, same function names and signatures).

## 2. Versioning

* `FLASH_ABI_VERSION` is **1**. It is stored in the plugin manifest (`abi_version`), in the plugin's metadata block (`FlashPluginInfo::abi_version`)
  and in the symbol name `flash_plugin_entry_v1`.
* A plugin with a different ABI version is rejected: at compile time if the manifest says so
  (`requires plugin ABI version 2, but this compiler supports version 1`), at load time if the library says so
  (`reports ABI version 99, expected 1`).
* Compatible changes (new optional functions in a plugin) do not change the version. Changing the layout of `FlashValue`, `FlashFuncInfo`
  or `FlashPluginInfo`, or the meaning of a tag, requires a new version (`flash_plugin_entry_v2`), and a host may support several.
* `FlashPluginInfo::struct_size` lets a newer host recognise an older, smaller metadata block; the loader refuses blocks smaller than version 1's.

## 3. Types

| Flash type | `FlashValue` tag | Representation |
|---|---|---|
| `Int`, `i8`…`i64` | `FLASH_T_I64` (2) | sign-extended to 64 bits in `a` |
| `u8`…`u64`, **opaque handles** | `FLASH_T_U64` (3) | zero-extended to 64 bits in `a` |
| `Float` | `FLASH_T_F64` (4) | IEEE-754 double, bit pattern in `a` |
| `Bool` | `FLASH_T_BOOL` (1) | `a` is exactly `0` or `1` (a full 64-bit slot, so the representation is explicit) |
| `String` / byte buffer | `FLASH_T_STR` (5) | `a` = pointer to the first byte, `b` = length in bytes. UTF-8, **not** NUL-terminated |
| `None` | `FLASH_T_NONE` (0) | no payload (return type only) |

```c
typedef struct FlashValue {      // 24 bytes, naturally aligned, part of the ABI
    uint32_t tag;
    uint32_t reserved;           // must be 0
    uint64_t a;
    uint64_t b;
} FlashValue;
```

Not allowed across the boundary: Rust `String`/`Vec`/`Box`/references/panics, C++ `std::string`/STL containers/classes/references/exceptions,
callbacks, or any struct by value. Wrap those in plugin-internal code and expose scalars, strings and handles.

* **Opaque handles** (e.g. a pointer to a C++ object) are `u64` values in Flash. Flash cannot dereference or forge-protect them;
  the plugin must define explicit create/use/free functions (see `geometry.counter_new/add/value/free`) and validate handles it receives if it needs to defend against misuse.
* **Narrow integers**: a Flash `u8` is passed as a `U64` whose value is ≤ 255; a plugin returning an `i32` returns `I64`. Values returned by a plugin are truncated to the manifest's declared width.

## 4. Ownership and lifetime rules

| Value | Owner | Lifetime / who frees |
|---|---|---|
| Argument scalars | — | copied |
| **String argument** (`ptr`,`len`) | **Flash** | **borrowed for the duration of the call only**. The plugin must not keep the pointer, free it, or write through it. Copy what you need |
| Scalar result | — | copied |
| **String result** (`ret->a`,`ret->b`) | **the plugin allocates** | the host copies the bytes into a Flash `String` immediately after the call returns, then calls `FlashPluginInfo::free_buf(ptr, len)` **exactly once** to release them. The buffer must have been allocated by the same plugin (Rust: `Box<[u8]>`; C++: `new[]`; C: `malloc` + a matching `free_buf`). Memory is never freed across allocators |
| **Error message** (status ≠ 0) | **the plugin allocates** | same as a string result: `ret` holds a `FLASH_T_STR` message, released with `free_buf` |
| Opaque handle | the plugin | until the plugin's own `*_free` function is called |

A zero-length string result may use a null pointer; the host then does not call `free_buf`.

## 5. Status codes and errors

Return `0` on success. Any non-zero value means failure, and `*ret` **must** then hold a `FLASH_T_STR` message (allocated per §4) — the host also tolerates a missing message and substitutes a generic one.

* In Flash a plugin function declared `fallible = true` returns `Result[T]`: status 0 → `ok(value)`, non-zero → `fail(message)`.
* A non-fallible function that reports a failure terminates the program with `Error: <message>` (exit code 1).
* **Panics and exceptions must never unwind across the boundary.**
  * Rust: the `flash_plugin!` macro wraps every function in `catch_unwind` and converts a panic into status 2 with the message `the plugin function panicked`.
  * C++: the `FLASH_GUARD` macro in `flash_plugin.hpp` converts any `std::exception` (or `...`) into status 2 with the exception's message.
  * Plugin authors writing the table by hand are responsible for the same guarantee. Unwinding through the host is undefined behaviour.
* The reserved codes used by the helpers: `1` = the function returned an `Err`, `2` = panic/exception/bad arguments. Other codes are allowed; only zero/non-zero matters to the host.

## 6. The entry point and metadata

A plugin exports one symbol:

```c
FLASH_EXPORT const FlashPluginInfo* flash_plugin_entry_v1(void);
```

```c
typedef struct FlashFuncInfo {
    const char* name;            // NUL-terminated
    uint32_t nparams;
    uint32_t ret_tag;            // tag of a successful result
    const uint32_t* param_tags;  // nparams entries
    FlashFn func;                // int32_t (*)(const FlashValue*, uint32_t, FlashValue*)
} FlashFuncInfo;

typedef struct FlashPluginInfo {
    uint32_t abi_version;        // FLASH_ABI_VERSION
    uint32_t struct_size;        // sizeof(FlashPluginInfo)
    const char* name;            // must equal the manifest's plugin name
    const char* version;         // plugin's own version, shown in the load message
    uint32_t nfuncs;
    const FlashFuncInfo* funcs;
    void (*free_buf)(uint8_t* ptr, uint64_t len);
} FlashPluginInfo;
```

The returned pointer and everything it references must have static storage duration. The full header is `include/flash_plugin.h`.

## 7. The manifest (`<name>.flashplugin.toml`)

```toml
[plugin]
name = "fast_math"        # the string used in `use rust_plugin "fast_math"`; must match the library's own name
kind = "rust"             # "rust" or "cpp": must match the `use` keyword
abi_version = 1
version = "0.1.0"         # informational
library = "fast_math"     # base name: libfast_math.so / libfast_math.dylib / fast_math.dll
# integrity = "sha256:<64 hex digits>"   # optional pin of the library file (see §9)

[[function]]
name = "checked_div"
params = ["Int", "Int"]   # Int, i8..i64, u8..u64, Float, Bool, String
returns = "Int"           # omitted = None
fallible = true           # Flash sees Result[Int]
```

Flash source:

```
use rust_plugin "fast_math"        # or: use cpp_plugin "geometry"
result = fast_math.checked_div(84, 2)
```

Grammar: `use_plugin = "use" ("rust_plugin" | "cpp_plugin") STRING`, at the top level of a file.

**Where manifests are found** (first hit wins): the directory of the source file, `./plugins/` and up to two levels of sub-folders below it (so `plugins/rust/fast_math/` is found from the repository root),
the current directory, then every directory in `FLASH_PLUGIN_PATH`. `flashpkg` adds the directories of plugin packages to `FLASH_PLUGIN_PATH`.

**Where libraries are found at run time:** every directory in `FLASH_PLUGIN_PATH`, the directory of the executable (and its `plugins/` sub-folder), the manifest's directory and its `build/` folder, and the `target/{release,debug}` folders of enclosing Cargo workspaces. Library file name: Linux `lib<name>.so`, macOS `lib<name>.dylib`, Windows `<name>.dll`.

## 8. Loading and validation

When a program starts it loads each plugin (once, before the first statement) and checks, in order:

1. the ABI version recorded at compile time equals the runtime's (`1`);
2. the library file is found (error lists every directory searched);
3. if the manifest pins `integrity`, the file's SHA-256 matches (`integrity check failed for plugin 'x': … has sha256 … but the program pins …`);
4. the library loads (`dlopen`/`LoadLibrary` through the `libloading` crate) and exports `flash_plugin_entry_v1`;
5. `FlashPluginInfo::abi_version == 1` and `struct_size` is at least the size of version 1's block;
6. the plugin's own name equals the manifest's name;
7. **every function the program uses** exists, with exactly the parameter tags and return tag the manifest declares.

Any failure prints `Error: <reason>` and exits with code 1 **before** any user code runs. Tests for each failure mode are in `crates/flash/tests/plugins.rs`
(wrong ABI in manifest, wrong ABI in library, missing function, signature mismatch, wrong plugin name, missing entry point, missing library, integrity mismatch, wrong `use` kind).

After loading, a one-line provenance message goes to stderr:

```
[flash] warning: loaded native plugin 'fast_math' v0.1.0 from /path/libfast_math.so (sha256 4fb9f73b51a76d3b). Native plugins run with this program's full privileges; only use plugins you trust.
```

Set `FLASH_PLUGIN_QUIET=1` to silence it (for example in CI).

Calls themselves are `fl_plugin_call(plugin, function, args, ret)` in the runtime: it invokes the C function pointer, converts a returned string into a Flash `String` and releases the plugin's buffer, and checks that the returned tag matches the declared return type (`the plugin returned a value of the wrong type` otherwise).

### Updating a plugin without rebuilding the program

Replace the library file with a build that has the same ABI version, plugin name and the same function names/signatures (it may add functions and change behaviour).
The Flash executable does not embed the plugin; it embeds only the manifest-derived signature table that is checked against the new library at start-up.
If the manifest pins an `integrity` hash, the pin must be updated and the program recompiled; leave the pin out to allow updates.

## 9. Trust model, provenance and isolation

* **Trusted code only.** The loader and the validation above do not sandbox anything. A plugin can corrupt the host's memory, so memory safety of
  a program that uses native plugins is only as good as the plugins.
* **Provenance:** the loader reports name, plugin version, absolute path and SHA-256 of every loaded library (stderr); packages pinned through `flashpkg` get a
  SHA-256 of the whole package source recorded in `flash.lock`; a manifest can pin the library's SHA-256.
* **No implicit code execution at build time:** the Flash compiler never loads plugins (it reads only the manifest). `flashpkg` never runs a dependency's build command
  unless `--allow-native-builds` is passed.
* **Planned — process isolation for untrusted plugins (not implemented).** The ABI is designed so that every call is already a self-contained message
  (function index + tagged values in, tagged value or error string out, no shared pointers beyond the call). An isolation mode would run the library in a
  helper process, serialise the `FlashValue` array over a pipe or shared-memory channel, and apply OS sandboxing (seccomp/`pledge`/App Sandbox/job objects) to the helper.
  Handles would become opaque tokens owned by the helper. None of this exists yet; today plugins always load in-process.

## 10. Writing a plugin in Rust

`Cargo.toml`:

```toml
[lib]
crate-type = ["cdylib"]
[dependencies]
flash-plugin-sdk = { path = "../../../crates/flash-plugin-sdk" }
```

`src/lib.rs` (complete example: `plugins/rust/fast_math/src/lib.rs`):

```rust
use flash_plugin_sdk::flash_plugin;

flash_plugin! {
    name = "fast_math", version = "0.1.0",
    functions {
        fn add(a: i64, b: i64) -> i64 { a.wrapping_add(b) }
        fn shout(text: &str) -> String { format!("{}!", text.to_uppercase()) }
        fn checked_div(a: i64, b: i64) -> Result<i64, String> {
            if b == 0 { Err("division by zero".to_string()) } else { Ok(a / b) }
        }
    }
}
```

Parameter types: `i64`, `u64`, `f64`, `bool`, `&str`. Return types: those plus `String`, `()` and `Result<T, String>`.

Build and use:

```bash
cargo build -p fast_math --release          # -> target/release/libfast_math.so (.dylib on macOS, fast_math.dll on Windows)
flash run examples/plugins_demo.flash       # finds plugins/rust/fast_math/fast_math.flashplugin.toml and target/release|debug/…
```

## 11. Writing a plugin in C++

Include `include/flash_plugin.hpp` (C++17), write one wrapper per exported function, and fill the table (complete example: `plugins/cpp/geometry/geometry.cpp`):

```cpp
#include "flash_plugin.hpp"

static int32_t fn_describe(const FlashValue* a, uint32_t, FlashValue* ret) {
    FLASH_GUARD(ret, {                                   // exceptions become status 2
        std::string name(flash::get_str(a[0]));          // copy: the argument is only borrowed
        flash::set_str(ret, "Window '" + name + "'");    // new[]-allocated; released by flash_cpp_free_buf
        return 0;
    })
}
static const uint32_t P_S1[] = {FLASH_T_STR};
static const FlashFuncInfo FUNCS[] = {{"describe", 1, FLASH_T_STR, P_S1, fn_describe}};
static const FlashPluginInfo INFO = {FLASH_ABI_VERSION, sizeof(FlashPluginInfo), "geometry", "0.1.0", 1, FUNCS, flash_cpp_free_buf};
extern "C" FLASH_EXPORT const FlashPluginInfo* flash_plugin_entry_v1(void) { return &INFO; }
```

C++ classes are exposed through **thin C wrappers with opaque handles** — never by exporting the C++ ABI. `geometry.cpp` wraps a `Counter` class as
`counter_new(start) -> u64`, `counter_add(handle, n)`, `counter_value(handle)`, `counter_free(handle)`.

### Build instructions

Linux and macOS (`plugins/cpp/geometry/Makefile` does this):

```bash
# Linux
c++ -std=c++17 -O2 -fPIC -fvisibility=hidden -I include -shared plugins/cpp/geometry/geometry.cpp -o plugins/cpp/geometry/build/libgeometry.so
# macOS (Apple Silicon or Intel)
c++ -std=c++17 -O2 -fPIC -fvisibility=hidden -I include -dynamiclib plugins/cpp/geometry/geometry.cpp -o plugins/cpp/geometry/build/libgeometry.dylib
```

Windows (untested in this repository — written from the documented MSVC options):

```bat
cl /nologo /std:c++17 /O2 /EHsc /LD /I include plugins\cpp\geometry\geometry.cpp /Fe:plugins\cpp\geometry\build\geometry.dll
```

Use `-fvisibility=hidden` (or `/EHsc` with `FLASH_EXPORT` on the entry point) so only `flash_plugin_entry_v1` is exported.

## 12. Static linking (not implemented)

The task brief asks for static *and* dynamic native interop. v0.1 implements **dynamic plugins only**. The design for static linking is: a manifest with
`link = "static"` and a path to a `.a`/`.lib` that exports the same `flash_plugin_entry_v1` (renamed per plugin with a prefix to avoid symbol clashes);
the compiler would pass the archive to the linker and the runtime would call the entry point directly instead of `dlopen`. Until that exists, the
`flash` linker invocation cannot link user libraries.
