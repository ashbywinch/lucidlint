fn f(xs: &[u32]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for x in xs {
        out.push(format!("out.push({})", x));
    }
    out
}
