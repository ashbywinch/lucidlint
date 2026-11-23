fn f(xs: &[u32]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for x in xs {
        let doubled = *x * 2;
        if doubled > 1 {
            out.push(format!("({})", doubled));
        }
    }
    out
}
