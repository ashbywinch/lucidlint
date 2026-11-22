fn guarded(xs: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    for x in xs {
        if *x > 0 {
            out.push(*x);
        }
    }
    out
}
