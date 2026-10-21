fn two(xs: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    for x in xs {
        out.push(*x);
    }
    for y in xs {
        out.push(*y + 1);
    }
    out
}
