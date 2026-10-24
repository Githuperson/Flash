fn make(n: usize, seed: usize) -> Vec<Vec<f64>> {
    let mut m = Vec::new();
    for i in 0..n {
        let mut row = Vec::new();
        for j in 0..n {
            row.push(((i * 31 + j * 17 + seed) % 100) as f64 / 10.0);
        }
        m.push(row);
    }
    m
}
fn multiply(a: &Vec<Vec<f64>>, b: &Vec<Vec<f64>>, n: usize) -> Vec<Vec<f64>> {
    let mut c = Vec::new();
    for i in 0..n {
        let mut row = vec![0.0f64; n];
        for k in 0..n {
            let aik = a[i][k];
            for j in 0..n {
                row[j] = row[j] + aik * b[k][j];
            }
        }
        c.push(row);
    }
    c
}
fn main() {
    let n = 300;
    let a = make(n, 1);
    let b = make(n, 2);
    let c = multiply(&a, &b, n);
    let mut trace = 0.0f64;
    for i in 0..n {
        trace = trace + c[i][i];
    }
    println!("{}", (trace * 1000.0) as i64);
}
