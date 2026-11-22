fn f(xs: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    for x in xs {
        out.push(*x);
    }
    let n = out.len();
    for y in xs {
        out.push(*y);
    }
    out
}
