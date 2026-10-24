struct Node {
    value: i64,
    label: String,
}
fn main() {
    let mut total: i64 = 0;
    for _ in 0..200000 {
        let mut nodes: Vec<Node> = Vec::new();
        for j in 0..20 {
            nodes.push(Node { value: j, label: format!("n{}", j) });
        }
        for nd in &nodes {
            total += nd.value + nd.label.len() as i64;
        }
    }
    println!("{}", total);
}
