fn score(xs: &[u32]) -> u32 {
    let mut best = 0;
    for x in xs {
        let doubled = *x * 2;
        let adj = doubled + 1;
        let capped = adj.min(u32::MAX);
        if capped > best {
            best = capped;
        }
    }
    best
}
