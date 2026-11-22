fn drain(seeds: &[usize]) -> Vec<usize> {
    let mut stack: Vec<usize> = seeds.to_vec();
    let mut seen = Vec::new();
    while let Some(node) = Vec::pop(&mut stack) {
        seen.push(node);
    }
    seen
}
