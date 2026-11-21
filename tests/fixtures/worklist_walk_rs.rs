fn reachable(roots: &[usize], edges: &[Vec<usize>]) -> Vec<usize> {
    let mut stack: Vec<usize> = roots.to_vec();
    let mut seen: Vec<usize> = Vec::new();
    while let Some(node) = stack.pop() {
        if seen.contains(&node) {
            continue;
        }
        seen.push(node);
        for next in &edges[node] {
            stack.push(*next);
        }
    }
    seen
}
