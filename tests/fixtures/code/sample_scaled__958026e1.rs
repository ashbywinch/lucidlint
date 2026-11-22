fn scaled(xs: &[u32]) -> Vec<u32> {
    let mut out: Vec<u32> = vec![0; xs.len()];
    for (i, x) in xs.iter().enumerate() {
        out[i] = *x * 2;
    }
    out
}
