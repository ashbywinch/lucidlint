fn main() {
    let mut v = vec![1, 2, 3, 4];
    enrich(&mut v, 3);
    println!("out: {}", v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","));
}
