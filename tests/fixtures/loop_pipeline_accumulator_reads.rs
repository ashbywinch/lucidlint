fn push_reads_len(xs: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    for _ in xs {
        out.push(out.len() as u32);
    }
    out
}

fn guard_reads_len(xs: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    for x in xs {
        if out.len() < 3 {
            out.push(*x);
        }
    }
    out
}

fn store_value_reads(xs: &[u32]) -> Vec<u32> {
    let mut out = vec![0; xs.len()];
    for (i, _) in xs.iter().enumerate() {
        out[i] = out.get(i).copied().unwrap_or(0);
    }
    out
}

fn store_index_reads(xs: &[u32]) -> Vec<u32> {
    let mut out = vec![0; xs.len() + 1];
    for x in xs {
        out[out.len() - 1] = *x;
    }
    out
}

fn compute(v: &[u32]) -> u32 {
    v.len() as u32
}

fn call_reads_accumulator(xs: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    for _ in xs {
        out.push(compute(&out));
    }
    out
}

fn call_reads_other(xs: &[u32], other: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    for _ in xs {
        out.push(compute(other));
    }
    out
}

fn guard_reads_other(xs: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    for x in xs {
        if *x > 0 {
            out.push(*x);
        }
    }
    out
}
