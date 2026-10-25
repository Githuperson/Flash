//! Differential testing of code generation: random integer expressions are evaluated by a
//! reference evaluator in this test and by Flash compiled in every configuration (dev, release,
//! LLVM). Any disagreement means a bug in the constant folder, optimiser, inliner or a back end.

mod common;
use common::*;

#[derive(Clone, Copy, PartialEq, Debug)]
struct Ity {
    bits: u32,
    signed: bool,
    name: &'static str,
}

const I64: Ity = Ity { bits: 64, signed: true, name: "Int" };
const I32: Ity = Ity { bits: 32, signed: true, name: "i32" };
const U8: Ity = Ity { bits: 8, signed: false, name: "u8" };
const U64: Ity = Ity { bits: 64, signed: false, name: "u64" };
const I16: Ity = Ity { bits: 16, signed: true, name: "i16" };

fn wrap(t: Ity, v: i128) -> i128 {
    let m = 1i128 << t.bits;
    let mut r = v.rem_euclid(m);
    if t.signed && r >= m / 2 {
        r -= m;
    }
    r
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) ^ (self.0 >> 7)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

enum E {
    Lit(i128),
    Var(usize),
    Bin(&'static str, Box<E>, Box<E>),
    /// divisor is always `(d & 7) + 1`
    Div(&'static str, Box<E>, Box<E>),
    Shift(&'static str, Box<E>, Box<E>),
    Neg(Box<E>),
    Not(Box<E>),
    /// `sel(a < b, x, y)` style choice using a comparison
    Cmp(&'static str, Box<E>, Box<E>, Box<E>, Box<E>),
}

fn gen(r: &mut Rng, t: Ity, depth: u32, nvars: usize) -> E {
    if depth == 0 || r.below(5) == 0 {
        return if r.below(3) == 0 {
            let edges: [i128; 6] =
                [0, 1, -1, if t.signed { -(1i128 << (t.bits - 1)) } else { 0 }, (1i128 << (t.bits - (t.signed as u32))) - 1, 7];
            let v = if r.below(2) == 0 { edges[r.below(6) as usize] } else { wrap(t, (r.next() as i128) % 1000 - 300) };
            E::Lit(wrap(t, v))
        } else {
            E::Var(r.below(nvars as u64) as usize)
        };
    }
    let sub = |r: &mut Rng| Box::new(gen(r, t, depth - 1, nvars));
    match r.below(10) {
        0 => E::Bin("+", sub(r), sub(r)),
        1 => E::Bin("-", sub(r), sub(r)),
        2 => E::Bin("*", sub(r), sub(r)),
        3 => E::Bin(["&", "|", "^"][r.below(3) as usize], sub(r), sub(r)),
        4 => E::Div(["/", "%"][r.below(2) as usize], sub(r), sub(r)),
        5 => E::Shift(["<<", ">>"][r.below(2) as usize], sub(r), sub(r)),
        6 if t.signed => E::Neg(sub(r)),
        7 => E::Not(sub(r)),
        _ => {
            let a = sub(r);
            let mut b = sub(r);
            // A comparison of two literal-only expressions has no integer type from context (it
            // would be a comparison of `Int`s); keep at least one variable in it.
            if !has_var(&a) && !has_var(&b) {
                b = Box::new(E::Var(r.below(nvars as u64) as usize));
            }
            E::Cmp(["<", "<=", ">", ">=", "==", "!="][r.below(6) as usize], a, b, sub(r), sub(r))
        }
    }
}

fn has_var(e: &E) -> bool {
    match e {
        E::Lit(_) => false,
        E::Var(_) => true,
        E::Bin(_, a, b) | E::Div(_, a, b) | E::Shift(_, a, b) => has_var(a) || has_var(b),
        E::Neg(a) | E::Not(a) => has_var(a),
        E::Cmp(_, a, b, x, y) => has_var(a) || has_var(b) || has_var(x) || has_var(y),
    }
}

fn eval(e: &E, t: Ity, vars: &[i128]) -> i128 {
    match e {
        E::Lit(v) => *v,
        E::Var(i) => vars[*i],
        E::Bin(op, a, b) => {
            let (x, y) = (eval(a, t, vars), eval(b, t, vars));
            wrap(
                t,
                match *op {
                    "+" => x + y,
                    "-" => x - y,
                    "*" => x.wrapping_mul(y),
                    "&" => x & y,
                    "|" => x | y,
                    _ => x ^ y,
                },
            )
        }
        E::Div(op, a, b) => {
            let (x, y) = (eval(a, t, vars), (eval(b, t, vars) & 7) + 1);
            wrap(t, if *op == "/" { x / y } else { x % y })
        }
        E::Shift(op, a, b) => {
            let (x, y) = (eval(a, t, vars), (eval(b, t, vars) & 7) as u32);
            wrap(t, if *op == "<<" { x << y } else { x >> y })
        }
        E::Neg(a) => wrap(t, -eval(a, t, vars)),
        E::Not(a) => wrap(t, !eval(a, t, vars)),
        E::Cmp(op, a, b, x, y) => {
            let (p, q) = (eval(a, t, vars), eval(b, t, vars));
            let c = match *op {
                "<" => p < q,
                "<=" => p <= q,
                ">" => p > q,
                ">=" => p >= q,
                "==" => p == q,
                _ => p != q,
            };
            if c {
                eval(x, t, vars)
            } else {
                eval(y, t, vars)
            }
        }
    }
}

fn show(e: &E, _t: Ity) -> String {
    match e {
        E::Lit(v) => {
            if *v < 0 {
                format!("({})", v)
            } else {
                v.to_string()
            }
        }
        E::Var(i) => format!("v{}", i),
        E::Bin(op, a, b) => format!("({} {} {})", show(a, _t), op, show(b, _t)),
        E::Div(op, a, b) => format!("({} {} (({} & 7) + 1))", show(a, _t), op, show(b, _t)),
        E::Shift(op, a, b) => format!("({} {} ({} & 7))", show(a, _t), op, show(b, _t)),
        E::Neg(a) => format!("(-{})", show(a, _t)),
        E::Not(a) => format!("(~{})", show(a, _t)),
        E::Cmp(op, a, b, x, y) => format!("pick({} {} {}, {}, {})", show(a, _t), op, show(b, _t), show(x, _t), show(y, _t)),
    }
}

fn program(seed: u64, t: Ity, literal_args: bool) -> (String, Vec<i128>) {
    let mut r = Rng(seed);
    let nvars = 3;
    let vals: Vec<i128> =
        (0..nvars).map(|_| wrap(t, (r.next() as i128 % 2000) - 1000 + if r.below(4) == 0 { 1 << (t.bits - 1) } else { 0 })).collect();
    let exprs: Vec<E> = (0..10).map(|_| gen(&mut r, t, 4, nvars)).collect();
    let expected: Vec<i128> = exprs.iter().map(|e| eval(e, t, &vals)).collect();
    let n = t.name;
    let mut src = format!("fn pick(c: Bool, own a: {n}, own b: {n}) -> {n}:\n    if c:\n        return a\n    return b\n\n");
    src.push_str(&format!("fn run(v0: {n}, v1: {n}, v2: {n}):\n"));
    // A typed binding gives literal-only expressions their integer type (otherwise they would be Int).
    for (i, e) in exprs.iter().enumerate() {
        src.push_str(&format!("    r{i}: {n} = {e}\n    say(r{i})\n", i = i, n = n, e = show(e, t)));
    }
    src.push('\n');
    if literal_args {
        let lit = |v: i128| if v < 0 { format!("({})", v) } else { v.to_string() };
        // literals must fit the type; use a conversion for negative unsigned (never happens: wrap is non-negative)
        src.push_str(&format!("run({}, {}, {})\n", lit(vals[0]), lit(vals[1]), lit(vals[2])));
    } else {
        src.push_str("a = args()\n");
        for i in 0..3 {
            let conv = if t.name == "Int" { "int" } else { t.name };
            src.push_str(&format!("v{i}: {n} = {c}(unwrap(parse_int(a[{i}])))\n", i = i, n = n, c = conv));
        }
        // u64 values above i64::MAX cannot be parsed; they are kept small by construction below
        src.push_str("run(v0, v1, v2)\n");
    }
    (src, expected)
}

fn run_config(src: &str, args: &[String], release: bool, backend: Option<&str>, tag: &str) -> String {
    let dir = scratch_dir(tag);
    let path = dir.join("fuzz.flash");
    std::fs::write(&path, src).unwrap();
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let (b, r) = build_and_run_with(&path, release, backend, tag, &a);
    assert!(b.status.success(), "compile failed:\n{}\n--- source ---\n{}", text(&b.stderr), src);
    let r = r.unwrap();
    assert!(r.status.success(), "run failed: {}\n--- source ---\n{}", text(&r.stderr), src);
    text(&r.stdout)
}

#[test]
fn random_integer_programs_agree_with_the_reference_evaluator_in_every_configuration() {
    let types = [I64, I32, U8, I16, U64];
    let mut configs: Vec<(bool, Option<&str>)> = vec![(false, None), (true, None)];
    if cfg!(feature = "legacy-backends") && clang_available() {
        configs.push((true, Some("llvm")));
    }
    let mut total = 0;
    for (ti, t) in types.iter().enumerate() {
        for seed in 0..4u64 {
            for literal_args in [false, true] {
                let s = seed * 7919 + ti as u64 * 104729 + 17;
                let (src, expected) = program(s, *t, literal_args);
                let want: String = expected.iter().map(|v| format!("{}\n", v)).collect();
                // runtime arguments (re-derive the values the same way as in `program`)
                let mut r = Rng(s);
                let vals: Vec<i128> = (0..3)
                    .map(|_| wrap(*t, (r.next() as i128 % 2000) - 1000 + if r.below(4) == 0 { 1 << (t.bits - 1) } else { 0 }))
                    .collect();
                let args: Vec<String> = vals.iter().map(|v| v.to_string()).collect();
                if !literal_args && vals.iter().any(|v| *v > i64::MAX as i128) {
                    continue;
                }
                for (ci, (release, backend)) in configs.iter().enumerate() {
                    let got = run_config(&src, &args, *release, *backend, &format!("fuzz-{}-{}-{}-{}", ti, seed, literal_args, ci));
                    assert_eq!(
                        got,
                        want,
                        "type {} seed {} literal_args {} config {:?}\n--- source ---\n{}",
                        t.name,
                        seed,
                        literal_args,
                        (release, backend),
                        src
                    );
                    total += 1;
                }
            }
        }
    }
    assert!(total >= 60, "only {} programs were checked", total);
}
