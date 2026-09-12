fn collect(xs: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    for x in xs {
        out.push(*x);
    }
    out
}
