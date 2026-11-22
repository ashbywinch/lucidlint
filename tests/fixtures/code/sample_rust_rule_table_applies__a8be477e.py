pub fn check(a: &M, who: &str) -> Vec<&'static str> {
    let mut out = vec![];
    if a.get(1) { out.push("v1"); }
    if a.get(2) { out.push("v2"); }
    if a.get(3) { out.push("v3"); }
    out
}
