fn split(xs: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let mut evens = Vec::new();
    let mut odds = Vec::new();
    for x in xs {
        evens.push(*x);
        odds.push(*x + 1);
    }
    (evens, odds)
}
