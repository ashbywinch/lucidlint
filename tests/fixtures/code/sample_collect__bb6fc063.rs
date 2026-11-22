fn collect(n: u32, xs: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    match n {
        0 => for x in xs {
            out.push(*x);
        },
        _ => {}
    }
    out
}
