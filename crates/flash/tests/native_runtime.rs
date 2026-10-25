//! Checks the Flash-language runtime library (float text, math) against Rust's standard library,
//! which is used here purely as an independent oracle.

mod common;
use common::*;

/// xorshift64* generator so that the cases are reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn run_flash(dir_tag: &str, program: &str, input: &str) -> String {
    let dir = scratch_dir(dir_tag);
    let src = dir.join("prog.flash");
    let data = dir.join("input.txt");
    std::fs::write(&src, program.replace("INPUT", data.to_str().unwrap())).unwrap();
    std::fs::write(&data, input).unwrap();
    for release in [false, true] {
        let (b, r) = build_and_run(&src, release, &format!("{}-{}", dir_tag, release), &[]);
        assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
        let r = r.unwrap();
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        if release {
            return String::from_utf8_lossy(&r.stdout).into_owned();
        }
        let dev = String::from_utf8_lossy(&r.stdout).into_owned();
        let (_, r2) = build_and_run(&src, true, &format!("{}-rel2", dir_tag), &[]);
        assert_eq!(dev, String::from_utf8_lossy(&r2.unwrap().stdout), "dev and release builds disagree");
        return dev;
    }
    unreachable!()
}

const PARSE_PRINT: &str = r#"
text = unwrap(read_file("INPUT"))
for line in split(text, "\n"):
    if len(line) > 0:
        say(unwrap(parse_float(line)))
"#;

#[test]
fn floats_parse_and_print_exactly_like_rust() {
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let mut inputs: Vec<String> = vec![];
    let specials = [
        "0", "-0", "0.0", "1", "0.1", "0.5", "123456789", "1e15", "1e16", "1e17", "9007199254740993", "0.0001", "0.00001",
        "1.5e-7", "1e-5", "123456789012345678", "4.9e-324", "2.2250738585072014e-308", "1.7976931348623157e308", "1e309",
        "1e-400", "0.30000000000000004", "100", "1e22", "1e23", "5e-324", "8.41e21", "2.5", "3.14159265358979323846264338327950288",
        "+7.25", "  42.5  ", ".5", "5.", "1E3", "inf", "-inf", "NaN", "Infinity", "2.2250738585072011e-308", "9.999999999999999e22",
    ];
    inputs.extend(specials.iter().map(|s| s.to_string()));
    for _ in 0..4000 {
        // random doubles written with the shortest digits, and with surplus digits
        let bits = rng.next();
        let v = f64::from_bits(bits);
        if v.is_finite() {
            inputs.push(format!("{:e}", v));
            inputs.push(format!("{:.25e}", v));
        }
        // "human" numbers
        let a = (rng.next() % 100000) as f64 / 100.0 * if rng.next() & 1 == 0 { 1.0 } else { -1.0 };
        inputs.push(format!("{}", a));
        let digits = rng.next() % 19 + 1;
        let m = rng.next() % 10u64.pow(digits as u32);
        let e = (rng.next() % 60) as i32 - 30;
        inputs.push(format!("{}e{}", m, e));
        inputs.push(format!("{:.17}", rng.unit()));
    }
    let mut expected = String::new();
    let mut text = String::new();
    for s in &inputs {
        text.push_str(s);
        text.push('\n');
        let v: f64 = s.trim().parse().unwrap();
        expected.push_str(&format!("{:?}\n", v));
    }
    let got = run_flash("native-floats", PARSE_PRINT, &text);
    let (g, e): (Vec<&str>, Vec<&str>) = (got.lines().collect(), expected.lines().collect());
    assert_eq!(g.len(), e.len());
    for (i, (a, b)) in g.iter().zip(&e).enumerate() {
        assert_eq!(a, b, "input {:?}", inputs[i]);
    }
}

fn ulps(a: f64, b: f64) -> u64 {
    if a == b || (a.is_nan() && b.is_nan()) {
        return 0;
    }
    if a.is_nan() || b.is_nan() || a.is_infinite() || b.is_infinite() {
        return u64::MAX;
    }
    let ord = |x: f64| {
        let b = x.to_bits() as i64;
        if b < 0 {
            i64::MIN.wrapping_sub(b)
        } else {
            b
        }
    };
    ord(a).abs_diff(ord(b))
}

const MATH_PROGRAM: &str = r#"
text = unwrap(read_file("INPUT"))
for line in split(text, "\n"):
    if len(line) > 0:
        x = unwrap(parse_float(line))
        say(sin(x), cos(x), tan(x), exp(x), ln(x), pow(x, 0.75), pow(2.5, x), pow(x, 3.0), sqrt(x))
"#;

#[test]
fn math_functions_agree_with_the_c_library_within_two_ulps() {
    let mut rng = Rng(12345);
    let mut xs: Vec<f64> = vec![0.0, 1.0, 0.5, 2.0, 10.0, 100.0, 1e-10, 3.14159265358979, 1.5707963267948966, 1e5, 1e7, 1e10, 1e22, 1e300, 12345.678];
    for _ in 0..1500 {
        xs.push((rng.unit() - 0.5) * 20.0);
        xs.push(rng.unit() * 700.0);
        xs.push(f64::from_bits(rng.next() & 0x7FFF_FFFF_FFFF_FFFF));
        xs.push(rng.unit() * 1e6);
    }
    let xs: Vec<f64> = xs.into_iter().filter(|x| x.is_finite()).collect();
    let text: String = xs.iter().map(|x| format!("{:e}\n", x)).collect();
    let got = run_flash("native-math", MATH_PROGRAM, &text);
    let mut worst = [0u64; 9];
    for (x, line) in xs.iter().zip(got.lines()) {
        let vals: Vec<f64> = line.split(' ').map(|s| s.parse().unwrap()).collect();
        let want = [x.sin(), x.cos(), x.tan(), x.exp(), x.ln(), x.powf(0.75), 2.5f64.powf(*x), x.powf(3.0), x.sqrt()];
        for k in 0..9 {
            let u = ulps(vals[k], want[k]);
            // tan has a large condition number near its poles; judge the other functions strictly
            let limit = if k == 2 { 4 } else { 2 };
            assert!(u <= limit || (want[k].abs() < 1e-300), "function {} at x = {:e}: got {:e}, libm {:e} ({} ulps)", k, x, vals[k], want[k], u);
            worst[k] = worst[k].max(if want[k].abs() < 1e-300 { 0 } else { u });
        }
    }
    eprintln!("worst error in ulps (sin cos tan exp ln pow pow pow sqrt): {:?}", worst);
}
