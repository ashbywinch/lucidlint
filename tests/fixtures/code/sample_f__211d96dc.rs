fn f(xs: &[u32]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    for x in xs {
        let scaled = *x * 2;
        if scaled > 1 {
            out.push((|y: u32| { let z = y * 2; z })(scaled));
        }
    }
    out
}
