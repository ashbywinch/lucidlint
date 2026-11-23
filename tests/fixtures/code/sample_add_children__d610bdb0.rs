fn add_children(edges: &[Vec<usize>], stack: &mut Vec<usize>) {
    for next in &edges[0] {
        stack.push(*next);
    }
}
