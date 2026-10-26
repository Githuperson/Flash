# Flash Language Reference (v0.1)

Flash is a statically typed, ahead-of-time compiled language with Python-like syntax.
This document describes **what the v0.1 compiler actually implements**. Features that are designed
but not implemented are listed explicitly in [§16 Not implemented](#16-not-implemented-yet).

Every example in this document that is not marked *(error)* is accepted by the compiler; the
programs in `examples/` and `tests/programs/` are compiled and run by the test-suite.

Contents: [1 Lexical structure](#1-lexical-structure) · [2 Types](#2-types) · [3 Expressions](#3-expressions) ·
[4 Statements](#4-statements) · [5 Functions and inference](#5-functions-and-type-inference) · [6 Structs](#6-structs) ·
[7 Lists](#7-lists) · [8 Strings](#8-strings) · [9 Errors](#9-error-handling-result-and-) · [10 Ownership](#10-ownership-and-memory-safety) ·
[11 Modules](#11-modules-and-the-standard-library) · [12 Native plugins](#12-native-plugins) · [13 Built-in functions](#13-built-in-functions) ·
[14 Runtime errors](#14-runtime-errors-and-exit-codes) · [15 Grammar](#15-grammar) · [16 Not implemented](#16-not-implemented-yet)

---

## 1. Lexical structure

* Source files are UTF-8 and use the extension `.flash`.
* **Comments** start with `#` and run to the end of the line.
* **Blocks are defined by indentation**; there are no braces and no semicolons.

### Indentation rules

1. Only **spaces** may be used for indentation. A tab in indentation is an error
   (`Tabs are not allowed for indentation`); configure your editor to insert spaces. 4 spaces per level is the convention
   (and what `flash fmt` produces), but any consistent width is accepted.
2. A block starts after a line ending in `:` and must be indented deeper than the line that opened it.
3. A line that is indented less must return exactly to the indentation of an enclosing block; otherwise the compiler reports
   `This line is indented differently from any enclosing block`.
4. **Blank lines and comment-only lines** are ignored for indentation purposes.
5. Newlines inside `( )` and `[ ]` are ignored, so long calls and lists can span lines.
6. A short body may follow the colon on the same line: `if x: say("hi")`.
7. An empty block is written `pass`.

### Error recovery

The parser records an error, skips to the end of the offending logical line (and any indented block under it) and keeps
parsing, so one run reports several independent syntax errors. Name-resolution and type errors are reported only when the
program parses cleanly, and ownership errors only when it type-checks.

### Literals

| Literal | Type | Notes |
|---|---|---|
| `42`, `1_000`, `0xFF`, `0b1010`, `0o17` | `Int` (or the integer type expected by context) | decimal, hex, binary, octal; `_` separators |
| `3.14`, `1e9`, `2.5e-3` | `Float` | 64-bit IEEE 754 |
| `"text"`, `'text'` | `String` | escapes: `\n \t \r \0 \\ \" \' \{ \}` |
| `f"x = {x + 1}"` | `String` | f-string; `{expr}` is converted with `str()`; `{{` and `}}` are literal braces |
| `True`, `False` | `Bool` | |
| `None` | `None` | the empty value |

Keywords: `fn struct if elif else while for in return break continue pass and or not True False None use import mut own extern`.

---

## 2. Types

| Type | Description |
|---|---|
| `Int` (= `i64`) | 64-bit signed integer |
| `i8 i16 i32 i64`, `u8 u16 u32 u64` | fixed-width signed / unsigned integers |
| `Float` (= `f64`) | 64-bit float |
| `Bool` | `True` / `False` |
| `String` | UTF-8 text, owned |
| `None` | the unit type; the type of functions that return nothing |
| `List[T]` | growable list of `T` |
| `Result[T]` | a `T` or an error message (§9) |
| *struct name* | user-defined struct (§6) |

Functions are values of type `Fn(A) -> R` (see *Function values and lambdas* in §5). Other types: `(A, B)` tuples, `Map[K, V]`, `Set[T]`, enums (§6) and generic structs/enums such as `Pair[A, B]`.

### Numeric conversions are always explicit

There are **no implicit numeric conversions**. `1 + 2.5` is a compile-time error, and so is adding an `i32` to an `i64`.

```
x = 7
y = float(x) / 2.0        # 3.5
z = int(y)                # 3     (Float -> integer truncates toward zero; saturates at the limits)
b = u8(300)               # 44    (integer -> narrower integer wraps modulo 2^8)
c = i32(b) + 1            # explicit widening
```

* Conversion functions: `int(x)` (= `i64`), `i8(x) … u64(x)`, `float(x)`; each accepts any integer type, `Float`
  or (for integer targets) `Bool`.
* **Integer literals adapt** to the integer type expected by context: `a: u8 = 200`, `a + 55`, `f(3)` where the parameter is
  `u8`. A *constant expression made only of literals* (`7`, `-1`, `~0`, `(1 + 2) * 3`) has no type of its own and adapts the same way;
  with no context at all (for example `say(1 + 2)`, or comparing two constant expressions) it is an `Int`. A literal that does not fit is an error: `The number 300 does not fit in u8.`
* `Float` -> integer conversion: `NaN` becomes 0, out-of-range values saturate to the minimum/maximum.

### Integer overflow

Integer arithmetic **wraps** (two's complement) for `+ - *` and unary `-`; this is defined behaviour, never undefined.
Division and remainder by zero stop the program with a runtime error (§14). `MIN / -1` is defined as wrapping (`MIN`)
and `MIN % -1` is `0`. Shift amounts are taken modulo the bit width of the left operand; `>>` is arithmetic for signed and
logical for unsigned types.

---

## 3. Expressions

Precedence, from lowest to highest binding:

| Level | Operators |
|---|---|
| 1 | `or` |
| 2 | `and` |
| 3 | `not` |
| 4 | `==  !=  <  <=  >  >=` (not chainable: `a < b < c` is an error) |
| 5 | `\|` |
| 6 | `^` |
| 7 | `&` |
| 8 | `<<  >>` |
| 9 | `+  -` |
| 10 | `*  /  %` |
| 11 | unary `-  ~` |
| 12 | postfix: call `f(x)`, index `a[i]`, field `a.b`, try `e?` |

* `and` / `or` short-circuit and require `Bool` operands. There is no truthiness: `if x:` with an `Int` is an error
  (write `x != 0`).
* `/` on integers is integer division truncating toward zero (`-7 / 2 == -3`, `-7 % 2 == -1`); on `Float` it is IEEE division.
  `%` is defined for integers only.
* `+` on `String` concatenates. `== != < <= > >=` work on numbers (same type), `String` (byte-wise order) and, for `==`/`!=`,
  `Bool`. Lists and structs cannot be compared.
* Bitwise operators `& | ^ ~ << >>` work on integers.
* **Ranges** appear only in `for` loops (§4): `a..b` is **half-open: it includes `a` and excludes `b`**; `a..=b` includes both ends.

---

## 4. Statements

### Variables

A variable is created by its first assignment; its type is inferred from the value, or given with an annotation:

```
name = "Alex"            # String
age = 11                 # Int
score = 100.5            # Float
ready: Bool = True
items: List[Int] = []    # annotation required for an empty list
age = age + 1            # later assignments must keep the type
age += 1                 # also -=  *=  /=  %=
```

**Scope:** variables are *block scoped*. A variable first assigned inside an `if`/`while`/`for` body is not visible after the
block. Assigning to a name that already exists in an enclosing block updates that variable (there is no shadowing). The one exception is a `for` loop variable: it is always a *new* variable that lives only inside the loop and hides an outer variable of the same name.
Functions cannot see the variables of the top-level script (there are no global variables in v0.1).

### if / elif / else

```
if age >= 18:
    say("Adult")
elif age >= 13:
    say("Teen")
else:
    say("Child")
```

### while, for, break, continue

```
while n < 10:
    n += 1
    if n % 2 == 0:
        continue
    if n > 7:
        break

for i in 0..10:          # 0, 1, ..., 9      (upper bound exclusive)
    say(i)
for i in 1..=10:         # 1, ..., 10        (inclusive)
    say(i)
for name in names:       # iterate a List
    say(name)
```

* The bounds of a range are evaluated once. Both bounds must have the same integer type. If `a >= b` the loop body does not run.
  `a..=b` is safe at the maximum value of the type (`0..=255` for `u8` terminates).
* Reassigning the loop variable inside the body does not affect the iteration.
* Looping over a list borrows it: the list cannot be modified or moved inside the loop (compile-time error).
  For lists of owned values (`String`, structs…) the loop variable is a read-only borrow of the element.

### return, pass

`return value` leaves a function; `pass` does nothing. `return` is not allowed in the top-level script (use `exit(code)`).

### Assignment targets

`name = e`, `s.field = e` (§6), `xs[i] = e` (§7).

---

## 5. Functions and type inference

```
fn add(a, b):                     # parameter and return types inferred per call
    return a + b

fn greet(name: String, times: Int) -> String:     # explicit annotations
    ...
```

### How inference works

* **Annotated parameters** have exactly that type; arguments must match (`3` is accepted for a `u8` parameter, but `3` for a
  `Float` parameter is an error).
* **Unannotated parameters** take the types of the arguments at each call. The compiler checks the body once for every distinct tuple of argument
  types (an *instance*), so `add(1, 2)`, `add(1.5, 2.5)` and `add("a", "b")` are three separately compiled and
  separately checked functions with return types `Int`, `Float`, `String`. Errors inside an instance are reported at the offending line together
  with a note naming the call that triggered it. Unannotated function bodies that are never called are still name-checked, but
  are type-checked only when used.
* **Return types** are inferred from the `return` statements: all of them must agree (`Inconsistent return types` otherwise),
  and every path must return (`can reach its end without returning a value`). A function without `return value` returns `None`.
* **Recursion:** the return type of a recursive function is inferred from the first `return` that does not depend on the
  recursive call (`fib` below). If it cannot be determined, the compiler asks for an annotation (`-> Int`).
* **Annotations are required** when inference has nothing to go on: an empty list literal (`items: List[Int] = []`),
  and `fail("…")` outside a function with a declared `Result[T]` return type.

```
fn fib(n):
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)
```

Functions may be used before they are defined. Calls can use keyword arguments (`greet(times=2, name="Alex")`).
`x.f(a, b)` is sugar for `f(x, a, b)` and works for built-ins and user functions alike
(`names.append("x")`, `text.upper()`).

### Parameter modes

| Declaration | Meaning |
|---|---|
| `fn f(p: T)` | **borrow** (default): read-only access for the duration of the call; the caller keeps ownership |
| `fn f(mut p: T)` | **mutable borrow**: the function may modify the argument's contents (`append`, field and element assignment); the argument must be a variable |
| `fn f(own p: T)` | **takes ownership**: the argument is moved into the function (the caller can no longer use it) |

For numbers, `Bool` and `None` the modes make no difference: they are copied in and a function may assign to its own copy of
the parameter. `mut` is allowed only for `String`, `List` and struct parameters, and a `mut` parameter cannot be replaced as a whole
(modify its contents instead). See §10.

### Function values and lambdas

`Fn(A, B) -> R` is the type of a function value. A named function or a `lambda` can be stored, passed and returned:

```
fn apply(f: Fn(Int) -> Int, x: Int) -> Int:
    return f(x)

fn double(x: Int) -> Int:
    return x * 2

fn make_adder(n: Int) -> Fn(Int) -> Int:
    return lambda x: x + n

say(apply(double, 21))                    # a named function as a value
say(apply(lambda x: x * 3, 2))            # lambda params: types come from the Fn type expected
add5 = make_adder(5)
say(add5(10))
```

* A lambda body is one expression. Its parameter types come from the `Fn` type it is passed to (or assigned to: `f: Fn(Int) -> Int = lambda x: x + 1`).
* A lambda may use numbers and `Bool` from the code around it; they are **copied in** when the lambda is created, so later changes aren't seen. Strings, lists and structs must be passed as parameters.
* Function values are owned (freed automatically) and can be struct fields (`op.run(1, 2)`), list items and return values. Parameters of function values borrow their arguments. Functions with `own`/`mut` parameters can't be used as values.

---

## 6. Structs

```
struct Player:
    name: String
    score: Int

player = Player(name="Alex", score=100)     # or positional: Player("Alex", 100)
say(player.name)
player.score = player.score + 10
```

* Every field needs a type annotation. All fields must be given at construction.
* Structs are **owned values**: assigning one moves it (`b = a` makes `a` unusable); use `copy(a)` for a deep copy.
* Struct values live on the heap and are freed automatically when the owner goes out of scope.
* Struct names are global across the modules of a program. See *Methods* below; `p.level_up()` also works for plain functions (it is sugar for `level_up(p)`).
* Layout: fields are stored in declaration order with natural alignment (documented in ARCHITECTURE.md).

### Methods

A function whose name is `Type.name` and whose first parameter is `self` is a method. Call it on a value (`p.level_up(5)`) or on the type
(`Player.new("Alex")`); both are the same call. `self` follows the usual parameter modes (`mut self` to change the value).

```
fn Player.new(own name: String) -> Player:
    return Player(name=name, score=0)

fn Player.level_up(mut self, amount: Int):
    self.score = self.score + amount

fn Player.describe(self) -> String:
    return f"{self.name} has {self.score} points"

p = Player.new("Alex")
p.level_up(60)
say(p.describe())
```

### Generic structs and enums

A type can have type parameters in square brackets. Each use with different arguments is its own concrete type (`Pair[Int, String]`),
checked and compiled like a hand-written one, so there is no runtime cost.

```
struct Pair[A, B]:
    first: A
    second: B

enum Option[T]:
    Some(value: T)
    Nothing

fn Pair.swap(self):
    return Pair(first=copy(self.second), second=copy(self.first))

p = Pair(first=1, second="one")          # Pair[Int, String]: inferred from the values
w: Option[String] = Some("hi")           # or from the type you write
```

The arguments are inferred from the constructor values or from the expected type (an annotated variable, parameter or return type).
When neither says enough (`x = Nothing`), the compiler asks you to annotate. Methods are written with the plain type name (`fn Pair.swap(self)`) and work for every instance.

### Tuples

`(a, b)` makes a tuple, `t.0` / `t.1` read an element, and a tuple can be unpacked into names. They are small anonymous structs (owned, moved on assignment), which makes returning several values easy:

```
fn min_max(xs: List[Int]) -> (Int, Int):
    ...
    return lo, hi

lo, hi = min_max(nums)
pair = (1, "one")
say(pair.0, pair.1)
```

### Enums and `match`

```
enum Shape:
    Circle(radius: Float)
    Rect(w: Float, h: Float)
    Empty

fn Shape.area(self) -> Float:
    match self:
        Circle(r):
            return 3.14 * r * r
        Rect(w, h):
            return w * h
        Empty:
            return 0.0

s = Shape.Circle(2.0)          # or just Circle(2.0) when the name is unique
```

* A variant can carry named fields; `match` binds them by position. Names bound by a pattern **borrow** from the matched value, so the value can't be changed or moved inside that `match`.
* `_` matches anything; literal patterns (`0`, `"yes"`) work on numbers and strings. A `match` over an enum must cover every variant (or have `_`).
* Enums are owned heap values and may refer to themselves (`Node(children: List[Tree])`).

---

## 7. Lists

```
numbers = [10, 20, 30]
say(numbers[0], len(numbers))
numbers[1] = 25
append(numbers, 40)                 # or numbers.append(40)
last = pop(numbers)                 # removes and returns the last element
empty: List[String] = []
zeros = fill(1000, 0)               # list of 1000 zeros (Copy element types only)
grid: List[List[Int]] = []          # lists nest
```

* Elements all have the same type; `List[T]` is generic over any element type.
* Indexing is **bounds checked**: an out-of-range index stops the program with `list index 5 is out of range for a list of length 3` (exit code 101).
  The compiler removes the check only when it can prove it unnecessary: the index is the variable of `for i in 0..len(xs)` and `xs` is not
  modified in the loop body.
* Negative indices are out of range (no Python-style wrap-around).
* `say(list)` prints `[1, 2, 3]`; strings inside lists are printed in quotes.
* Reading an *owned* element (a `String` or struct) out of a list into a variable would move it out and leave a hole, so it is
  rejected; read it inside an expression (`say(names[0])`, `len(names[0])`, `names[0] + "!"`) or copy it: `n = copy(names[0])`.

### Maps and sets

`Map[K, V]` and `Set[T]` are hash tables that remember insertion order. Keys are integers, `Bool` or `String`.

```
ages: Map[String, Int] = {}              # an empty {} needs a type
ages["ana"] = 31
scores = {"x": 1.5, "y": 2.5}            # {k: v, ...} is a Map
seen = {1, 2, 3}                         # {a, b, ...} is a Set

say(ages["ana"], ages.has("bo"), ages.get("bo", -1), len(ages))
ages.remove("ana")
for k in scores:                          # iterates keys (items of a Set) in insertion order
    say(k, scores[k])
say(ages.keys(), ages.values())
seen.add(4)                               # Set: add, has, remove, items, clear
```

Indexing a missing key is a runtime error (exit code 101); use `get(key, default)` or `has(key)`. Maps and sets are owned values like lists. They exist only in Flash's own back end (not the legacy Cranelift/LLVM ones).

---

## 8. Strings

* A `String` is an immutable sequence of bytes that is expected to be UTF-8. `len(s)` is the length in **bytes**.
* `s + t` creates a new string. Strings are compared byte-wise.
* Strings cannot be indexed (`s[0]` is an error); use `byte_at(s, i)` (a `u8`) or `substr(s, start, length)` (byte offsets, clamped to the string).
* String literals are stored in the executable's read-only data and never freed; every other string is freed by its owner.
* Library functions: `upper lower trim contains starts_with ends_with replace split join find substr byte_at parse_int parse_float str len` (§13).

---

## 9. Error handling: `Result` and `?`

Fallible operations return `Result[T]`: either a value of type `T` or an error message (a `String`).

```
fn load_number(path: String) -> Result[Int]:
    text = read_file(path)?              # on error: return the error from this function
    return parse_int(trim(text))         # a Result[Int] already

fn safe_div(a: Int, b: Int) -> Result[Int]:
    if b == 0:
        return fail("cannot divide by zero")
    return ok(a / b)

r = safe_div(10, 0)
if is_ok(r):
    say(unwrap(r))
else:
    say("failed:", error_message(r))
say(unwrap_or(safe_div(1, 0), -1))       # default on error
```

* `ok(value)` / `ok()` build a successful `Result`; `fail(message)` builds an error (its type comes from the declared return type).
* `e?` requires `e: Result[T]` and evaluates to the `T`. If `e` is an error, the **enclosing function returns that error**
  immediately (after releasing its owned values). The enclosing function must be declared `-> Result[U]` (any `U`).
  At the top level of a script, `?` on an error prints `Error: <message>` to stderr and exits with code **1**.
* `is_ok(r)`, `error_message(r)` borrow the result; `unwrap(r)` and `unwrap_or(r, default)` consume it. `unwrap` on an error stops
  the program (exit 101).
* `Result` is an owned type (move semantics, freed automatically). No exceptions exist in the language.
* Fallible built-ins: `read_file write_file append_file remove_file env_var parse_int parse_float`; native plugin functions can be declared
  fallible too (§12).

Limitation: if `?` returns early from the middle of an expression that has already built other owned temporaries which were
moved into the same expression (for example `f(make_string(), g()?)`), those temporaries are leaked rather than freed. This is memory-safe but not leak-free.

---

## 10. Ownership and memory safety

Flash has **no garbage collector and no reference counting**. Memory is released deterministically by a small ownership
discipline that the compiler enforces. This is a deliberately restricted model for v0.1, simpler than Rust's: there are
no user-visible reference types and no lifetime annotations.

### The rules

1. **Copy types** — `Int` and the other integers, `Float`, `Bool`, `None` — are copied on assignment and when passed to functions.
2. **Owned types** — `String`, `List[T]`, structs and `Result[T]` — have exactly one owner. Using one *by value* moves it:
   `b = a`, `append(xs, a)`, `Player(name=a)`, `return a`, or passing it to an `own` parameter. The moved-from variable cannot be used again
   (`The value of 'a' was moved earlier and can't be used again`). This is tracked through branches and loops: a value moved in one
   iteration of a loop is reported as moved in the next. Assigning to a moved variable re-initialises it.
3. **Borrows are temporary and cannot be stored.** Function parameters borrow by default, and `for x in list` borrows each element. A borrow can be read, or (for `mut`
   parameters) modified in place, but cannot be assigned to another variable, returned, stored in a list or struct, or moved. Because borrows can never outlive the call or loop that created them, they can never dangle.
4. **No aliasing of exclusive access.** A call cannot pass the same variable to a `mut`/`own` parameter and to another parameter. While a `for` loop reads a list,
   the list cannot be modified or moved inside the loop body (`Cannot change or move 'xs' while a for loop is reading it`).
5. **Cleanup is automatic and predictable.** An owned value is freed exactly once: at the end of the block where its owner was declared,
   when the owner is reassigned, or when ownership is transferred to a function that frees it. Early exits (`return`, `break`, `continue`, `?`) free everything
   that is still owned. Variables that are only *sometimes* moved get a runtime drop flag.
6. **`copy(x)`** makes an independent deep copy; it is the explicit way to keep using a value you also hand away.

### What this guarantees (and what it doesn't)

In safe Flash code (which is all of Flash v0.1):

* no use-after-free and no double-free (rules 2–5; verified by the test-suite with an allocation counter that must report `0` live allocations);
* list accesses are bounds checked; integer arithmetic has no undefined behaviour; division by zero is a defined runtime error;
* memory is never read before it is initialised.

Not covered:

* **Memory leaks are not impossible** — a leak is memory-safe (see the `?` limitation in §9).
* **Stack overflow** from unbounded recursion terminates the process with the operating system's stack-overflow signal. There is no recursion limit check.
* **Out-of-memory** terminates the program with `Flash runtime error: out of memory`.
* **Native plugins are trusted native code** and can break every guarantee (§12). Opaque handles handed out by plugins are plain integers; using one after the plugin freed it is the plugin's and caller's responsibility.
* On a runtime error (bounds, division by zero, `unwrap`, `assert`, `exit`) the process exits immediately **without** running destructors; the operating system reclaims memory.

### Data races and concurrency

Flash has threads and channels (Linux only for now; other systems report this at compile time). Threads **share nothing**: whatever a thread uses is
copied or moved into it, and values travel between threads through channels, so safe Flash code cannot have a data race.

```
results: Channel[Int] = channel()
tx = copy(results)                          # another handle to the same channel
t = spawn(lambda: send(tx, 6 * 7))          # runs on a new thread
t.join()                                    # wait for it
say(unwrap(recv(results)))                  # 42
```

* `spawn(f)` runs a function value of type `Fn()` on a new thread and returns a `Thread`; `t.join()` (or `join(t)`) waits for it and consumes the handle.
  A `lambda` written directly in `spawn(...)` **takes ownership** of the strings, lists, structs and channels it uses (they are moved in, so the code
  after `spawn` can no longer use them) and copies numbers and `Bool`. A borrowed parameter can't be moved into a thread: pass `copy(x)` or take it with `own`.
* `channel()` needs a type: `ch: Channel[T] = channel()`. `send(ch, v)` moves `v` into the channel; `recv(ch)` waits for a message and returns a `Result[T]`
  that is an error once the channel is closed and empty; `close(ch)` ends it. `for x in ch:` receives until then. `copy(ch)` makes another handle to the same channel
  (a channel is freed with its last handle, together with any messages nobody received).
* A thread that is never joined is simply left running until the program exits. Output written by threads is flushed a line at a time.
* A runtime error in any thread (an out-of-range index, a failed `assert`, ...) ends the whole program with exit code 101.
* Implementation: every thread has its own runtime state (so the allocator needs no locks); channels use atomic operations and futexes. On x86-64 and AArch64 Linux the code
  generators emit the `clone` call, the atomics and the thread-pointer access themselves; no C library is involved.

### `unsafe`

There is no `unsafe` block in v0.1; no raw pointers or manual allocation are exposed to Flash code. Low-level work is done in native plugins (§12).
A future `unsafe` block will be the only place where raw pointers and unchecked indexing are allowed.

---

## 11. Modules and the standard library

```
import helpers              # loads helpers.flash from the same directory
import std.math             # standard library module (stdlib/math.flash)

p = helpers.make_point(3, 4)
say(math.gcd(84, 36))
```

* `import a.b.c` loads `a/b/c.flash` relative to the importing file; the module is referred to by its last name segment (`c.f(x)`).
  Modules found through `FLASH_PATH` (set by `flashpkg` for package dependencies) are searched next.
* Functions of other modules are called as `module.function(...)`. Structs are global (their names must be unique across modules).
  Imported modules may only contain `fn`, `struct` and `import`; no top-level statements.
* Import cycles are rejected. Each file is compiled once even if several modules import it.
* Standard library modules (written in Flash, in `stdlib/`): `std.math` (`gcd lcm clamp factorial pow_int is_prime sum sum_float max_of min_of`),
  `std.text` (`repeat pad_left pad_right is_blank count reverse`), `std.bench` (`elapsed_ns elapsed_ms`). The core library is the set of built-ins in §13.

---

## 12. Native plugins

Flash can call functions in shared libraries written in Rust, C++ (or anything that exports a C ABI). The ABI is specified in
[PLUGIN_ABI.md](PLUGIN_ABI.md). In source code:

```
use rust_plugin "fast_math"
use cpp_plugin "geometry"

say(fast_math.add(40, 2))
say(geometry.distance(0.0, 0.0, 3.0, 4.0))
r = fast_math.checked_div(1, 0)         # fallible functions return Result[T]
```

* `use rust_plugin "name"` / `use cpp_plugin "name"` must be at top level. The compiler reads `name.flashplugin.toml`
  (searched next to the source, in `./plugins/**`, and in `FLASH_PLUGIN_PATH`), which declares each function's Flash signature; calls are
  type-checked at compile time.
* Plugin function parameter and return types are limited to the ABI-safe types: integers, `Float`, `Bool`, `String` and `None`. Opaque handles are `u64` values.
* The shared library is loaded when the program starts; the loader validates the ABI version, the plugin name and every function signature against what the
  compiler recorded, and prints a warning with the library path and its SHA-256 (silence with `FLASH_PLUGIN_QUIET=1`).
* Plugins execute with the full privileges of the program. See PLUGIN_ABI.md for the trust model.

### Plugin code inside the program

Instead of building a plugin by hand, write its source under a `when using` line. The compiler builds it (with `rustc` or your C++ compiler) the first time and caches the result:

```
when using rust plugin "mathx":
    fn add(a: i64, b: i64) -> i64 { a + b }
    fn safe_div(a: i64, b: i64) -> Result<i64, String> {
        if b == 0 { Err("divide by zero".to_string()) } else { Ok(a / b) }
    }

when using cpp plugin "textx":
    #include <algorithm>
    static std::string rev(std::string s) { std::reverse(s.begin(), s.end()); return s; }
    std::string flip(std::string s) { return rev(s); }
    double hyp(double a, double b) { return a * a + b * b; }

say(mathx.add(40, 2))
say(textx.flip("abc"))
```

* The header ends with `:` (a trailing `;` also works). Everything indented below it is the foreign code; the name in quotes is what you call (`mathx.add`).
* **Rust** blocks hold `fn` items (they become the `functions { }` of the plugin SDK's `flash_plugin!`). **C++** blocks are plain functions plus anything else you need (includes, helper types, `static` helpers); every non-`static` function at the top level is exported.
* Signatures are read from the code. Allowed types: Rust `i8…i64`, `u8…u64`, `f64`, `bool`, `&str` (parameters), `String` (return) and `Result<T, String>` (becomes a `Result[T]`); C++ `int8_t…int64_t`, `uint8_t…uint64_t`, `double`, `bool`, `std::string`, `void`.
* Compiler errors are shown with line numbers counted from the first line of the block. The built libraries live in `<cache dir>-plugins` (next to the build cache) and are rebuilt when the code changes.
* Like all plugins this needs a compiler built with `--features legacy-backends`, and the plugin runs with the program's full privileges (see PLUGIN_ABI.md). Building the program also runs `rustc`/`c++` on the block, so only compile programs you trust.

---

### Statically linked native libraries: `extern fn`

For plain C-ABI functions that you link into the executable (no manifest, no `dlopen`):

```
extern fn c_gcd(a: Int, b: Int) -> Int
extern fn c_count_byte(text: String, needle: u8) -> u64     # String = (pointer, byte length)
extern fn c_is_even(n: Int) -> Bool

say(c_gcd(84, 36))
```

```bash
flash run main.flash --link build/libmathlib.a -lstdc++     # --link <file>, -l<name>, -L<dir>, -Wl,<linker option>
```

* The declaration names the C symbol exactly and states its signature; **Flash cannot verify that the library really has that signature** — a wrong declaration is as unsafe as in any FFI. This is why the declaration is the trust point; there is no `unsafe` keyword yet.
* Parameters: integers (any width), `Float`, `Bool`, `String`. Integers are passed widened to 64 bits (sign- or zero-extended according to their Flash type), `Bool` as 0/1, a `String` as two arguments `(const uint8_t* ptr, uint64_t len)` that the C code may only read during the call and must not keep or free.
  Return types: integers, `Float`, `Bool` or nothing. Strings, lists and structs cannot cross this simple boundary (ownership conventions are what the plugin ABI is for).
* C++ code must be exposed through `extern "C"` functions and must not let exceptions escape; Rust libraries should be `#![no_std]` `staticlib`s (see `examples/static_link/rust_static.rs`) or, better, plugins.
* Names starting with `fl_` or `flash_` are reserved for the runtime. A missing symbol is reported by the linker with a hint.

## 13. Built-in functions

`T` stands for any printable type; "borrows" means the argument is not consumed.

| Function | Description |
|---|---|
| `say(a, b, …)` / `print(a, b, …)` | print values separated by spaces; `say` adds a newline. Accepts numbers, `Bool`, `String`, `None`, lists of those |
| `str(x)` | text of a number, `Bool`, `String` or `None` (`2.0` prints as `2.0`) |
| `int(x) i8…u64(x) float(x)` | explicit numeric conversion (§2) |
| `len(x)` | length of a `String` (bytes) or `List` |
| `append(list, v)`, `pop(list)`, `fill(n, v)` | list operations (§7); `append`/`pop` need a modifiable variable |
| `copy(x)` | deep copy of a `String`, `List` or struct |
| `abs(x)` `min(a, b)` `max(a, b)` | numeric helpers (same type for both arguments) |
| `sqrt floor ceil sin cos tan exp ln` `(Float)`, `pow(a, b)` | floating-point math |
| `upper lower trim` `(s)` | new strings |
| `contains starts_with ends_with` `(s, t)` → `Bool`; `find(s, t)` → `Int` (-1 if absent) | |
| `replace(s, from, to)`, `split(s, sep)` → `List[String]`, `join(list, sep)`, `substr(s, start, length)`, `byte_at(s, i)` → `u8` | |
| `parse_int(s)`, `parse_float(s)` | → `Result[Int]` / `Result[Float]` (surrounding spaces are ignored) |
| `read_file(path)` → `Result[String]`; `write_file(path, text)`, `append_file`, `remove_file` → `Result[None]`; `file_exists(path)` → `Bool` | |
| `path_join(a, b)`, `path_filename(p)`, `path_parent(p)`, `path_extension(p)` | path text helpers |
| `args()` → `List[String]` | command-line arguments (not including the program name) |
| `env_var(name)` → `Result[String]` | environment variable |
| `read_line()` → `String` | next line of standard input without the newline (empty at end of input) |
| `now_ns()` → `Int` | monotonic clock in nanoseconds since program start; `sleep_ms(n)` |
| `exit(code)` | flush output and exit immediately |
| `assert(cond)`, `assert(cond, message)` | on failure print `Assertion failed: …` and exit with code 101 |
| `ok(v)`, `fail(msg)`, `is_ok(r)`, `unwrap(r)`, `unwrap_or(r, d)`, `error_message(r)` | `Result` helpers (§9) |

Output is buffered and flushed at exit, before reading input, before sleeping and before any error message.
User functions cannot reuse built-in names.

---

## 14. Runtime errors and exit codes

| Situation | Message (stderr) | Exit code |
|---|---|---|
| list index out of range | `Flash runtime error: list index 5 is out of range for a list of length 3` | 101 |
| integer division or remainder by zero | `Flash runtime error: division by zero` | 101 |
| `pop` on an empty list, `byte_at` out of range | `Flash runtime error: …` | 101 |
| `unwrap` on an error | `Flash runtime error: unwrap() on an error: <message>` | 101 |
| failed `assert` | `Assertion failed: <message>` | 101 |
| `?` on an error at the top level | `Error: <message>` | 1 |
| `exit(n)` | — | `n` |
| plugin loading/validation failure | `Error: <reason>` | 1 |

Compiler diagnostics always exit with code 1 (`flash check/build/run`); usage errors exit with 2.

---

## 15. Grammar

```
file        = { item NEWLINE* } ;
item        = fn_def | extern_fn | struct_def | enum_def | import | use_plugin | inline_plugin | statement ;
extern_fn   = "extern" "fn" NAME "(" [ NAME ":" type { "," NAME ":" type } ] ")" [ "->" type ] ;
fn_def      = "fn" NAME "(" [ param { "," param } ] ")" [ "->" type ] block ;
param       = [ "mut" | "own" ] NAME [ ":" type ] ;
struct_def  = "struct" NAME [ "[" NAME { "," NAME } "]" ] ":" NEWLINE INDENT { NAME ":" type NEWLINE } DEDENT ;
enum_def    = "enum" NAME [ "[" NAME { "," NAME } "]" ] ":" NEWLINE INDENT { NAME [ "(" NAME ":" type { "," NAME ":" type } ")" ] NEWLINE } DEDENT ;
match       = "match" expr ":" NEWLINE INDENT { pattern block } DEDENT ;
pattern     = "_" | literal | NAME [ "(" NAME { "," NAME } ")" ] ;
lambda      = "lambda" [ NAME { "," NAME } | "(" NAME [ ":" type ] { "," NAME [ ":" type ] } ")" ] ":" expr ;
inline_plugin = "when" "using" ( "rust" | "cpp" ) "plugin" STRING ":" NEWLINE INDENT raw_code DEDENT ;
import      = "import" NAME { "." NAME } ;
use_plugin  = "use" ( "rust_plugin" | "cpp_plugin" ) STRING ;
type        = NAME [ "[" type { "," type } "]" ] | "(" type { "," type } ")" | "Fn" "(" [ type { "," type } ] ")" [ "->" type ] ;

block       = ":" ( simple_stmt | NEWLINE INDENT statement+ DEDENT ) ;
statement   = if | while | for | "break" | "continue" | "pass" | "return" [expr] | assignment | expr ;
if          = "if" expr block { "elif" expr block } [ "else" block ] ;
while       = "while" expr block ;
for         = "for" NAME "in" ( expr ( ".." | "..=" ) expr | expr ) block ;
assignment  = target ( "=" | "+=" | "-=" | "*=" | "/=" | "%=" ) expr
            | NAME ":" type "=" expr ;
target      = NAME | expr "." NAME | expr "[" expr "]" ;

expr        = or_expr ;                      (* precedence table in §3 *)
primary     = INT | FLOAT | STRING | FSTRING | "True" | "False" | "None" | NAME
            | "(" expr ")" | "[" [ expr { "," expr } ] "]" ;
postfix     = primary { "(" [ arg { "," arg } ] ")" | "[" expr "]" | "." NAME | "?" } ;
arg         = [ NAME "=" ] expr ;
```

---

## 16. Not implemented yet

These are part of the design direction but **do not exist** in v0.1; the compiler rejects or does not recognise them:

* ternary expressions; `try/except` (use `Result`);
* traits/interfaces, generic *functions* with explicit type parameters (plain functions are already generic by inference), operator overloading, closures that capture strings, lists or structs;
* string indexing and slicing syntax, character type, string formatting specs (`{x:.2f}`);
* global variables and constants; `while … else`; `del`; `with`;
* async/await, locks and shared mutable state between threads; threads and channels on macOS and Windows (they work on Linux only);
* `unsafe` blocks and raw pointers;
* reference locals (`b = bodies[i]` is rejected because it would move an element out; index each time or use `for`);
* statically linking of anything other than C-ABI `extern fn` functions (no automatic bindings, no strings/structs across `extern fn`);
* arbitrary-precision or overflow-checked integers; an interpreter or REPL;
* editor/LSP support; an online package registry.
