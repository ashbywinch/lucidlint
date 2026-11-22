fn traversal(seeds: &[u32]) -> Vec<u32> {
    let mut work: Vec<u32> = seeds.to_vec();
    let mut seen = Vec::new();
    while let Some(node) = work.pop() {
        seen.push(node);
        work.push(node);
    }
    seen
}
