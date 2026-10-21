fn pos(xs: &[i32]) -> Vec<i32> {
    let mut out = Vec::new();
    for x in xs {
        if *x > 0 {
            out.push(*x);
        }
    }
    out
}
