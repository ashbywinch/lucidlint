fn f(xs: &[u32]) -> u32 {
    let a = |x: u32| x + 1;
    let b = |x: u32| x * 2;
    xs.iter().map(a).map(b).sum()
}
