use std::collections::HashMap;

fn chained_target(xs: &[u32]) -> HashMap<u32, Vec<u32>> {
    let mut out: HashMap<u32, Vec<u32>> = HashMap::new();
    for x in xs {
        out.entry(*x).or_default().push(*x);
    }
    out
}
