fn total(xs: &[u32]) -> u32 {
    xs.iter().map(|x| x + 1).sum()
}
