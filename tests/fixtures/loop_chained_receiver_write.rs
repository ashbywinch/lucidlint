use std::collections::HashMap;

fn split(xs: &[(u32, u32)], line: &mut HashMap<u32, Vec<u32>>, file: &mut HashMap<u32, u32>) {
    for (k, v) in xs {
        line.entry(*k).or_default().push(*v);
        file.insert(*k, *v);
        let a = *v + 1;
        let b = a * 2;
    }
}

fn fill(xs: &[(u32, u32)], line: &mut HashMap<u32, Vec<u32>>) {
    for (k, v) in xs {
        line.entry(*k).or_default().push(*v);
        let a = *v + 1;
        let b = a * 2;
        let c = b + 1;
    }
}
