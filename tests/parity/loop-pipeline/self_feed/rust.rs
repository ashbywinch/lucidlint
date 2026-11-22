fn self_feed(xs: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    for x in std::mem::take(&mut out) {
        out.push(x);
    }
    out
}
