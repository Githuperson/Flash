fn main() {
    let mut total: i64 = 0;
    for i in 0..400_000_000i64 {
        total += (i * i) % 7 + (i & 3);
    }
    println!("{}", total);
}
