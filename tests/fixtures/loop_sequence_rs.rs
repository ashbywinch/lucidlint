fn two_passes(xs: &[u32]) -> Vec<u32> {
    let mut tmp = Vec::new();
    for x in xs {
        tmp.push(*x + 1);
    }
    let mut out = Vec::new();
    for t in &tmp {
        out.push(*t * 2);
    }
    out
}
