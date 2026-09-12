fn score(xs: &[u32]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    for x in xs {
        let doubled = *x * 2;
        let adj = doubled + 1;
        if adj > 0 {
            out.push(adj);
        }
    }
    out
}
