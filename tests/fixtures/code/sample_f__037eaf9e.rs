fn f(xs: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    for x in xs {
        let n = out.len();
        out.push(*x + n as u32);
    }
    out
}
