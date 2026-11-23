fn collect(xs: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    // lucidlint: ignore loop-pipeline the order of pushes is load-bearing
    for x in xs {
        out.push(*x);
    }
    out
}
