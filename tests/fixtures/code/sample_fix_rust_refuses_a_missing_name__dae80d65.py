pub fn f(xs: &[i32]) -> Vec<i32> {
    let mut out = Vec::new();
    for x in xs {
        out.push(*x);
    }
    out
}
