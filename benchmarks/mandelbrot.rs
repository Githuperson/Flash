fn mandel(cx: f64, cy: f64, max_iter: i64) -> i64 {
    let (mut x, mut y, mut i) = (0.0f64, 0.0f64, 0i64);
    while i < max_iter && x * x + y * y <= 4.0 {
        let t = x * x - y * y + cx;
        y = 2.0 * x * y + cy;
        x = t;
        i += 1;
    }
    i
}
fn main() {
    let size = 700i64;
    let mut total = 0i64;
    for py in 0..size {
        for px in 0..size {
            let cx = px as f64 / size as f64 * 3.0 - 2.0;
            let cy = py as f64 / size as f64 * 2.0 - 1.0;
            total += mandel(cx, cy, 300);
        }
    }
    println!("{}", total);
}
