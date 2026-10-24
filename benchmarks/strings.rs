fn main() {
    let mut words: Vec<String> = Vec::new();
    for i in 0..300000 {
        words.push(format!("w{}", i));
    }
    let text = words.join(" ");
    let parts: Vec<String> = text.split(' ').map(|s| s.to_string()).collect();
    let mut total = 0usize;
    let mut sevens = 0;
    for p in &parts {
        total += p.len();
        if p.contains('7') {
            sevens += 1;
        }
    }
    println!("{} {} {} {}", text.len(), parts.len(), total, sevens);
}
