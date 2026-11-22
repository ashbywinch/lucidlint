fn check(a: &M) -> Vec<&str> {
    let mut out = vec![];
    if a.get(1) { out.push("x"); }
    if a.get(2) { out.push("y"); }
    if a.get(3) { out.push("z"); }
    out
}
