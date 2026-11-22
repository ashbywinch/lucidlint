fn local_walk(roots: &[u32]) -> Vec<u32> {
    let mut queue: Vec<u32> = roots.to_vec();
    let mut i = 0;
    while i < queue.len() {
        let x = queue[i];
        let y = x + 1;
        let z = y * 2;
        queue.push(z);
        i += 1;
    }
    queue
}

fn mut_arg_walk(roots: &[u32]) -> usize {
    let mut queue: Vec<u32> = roots.to_vec();
    let mut i = 0;
    while i < queue.len() {
        let x = queue[i];
        let y = x + 1;
        let z = y * 2;
        push_next(&mut queue, z);
        i += 1;
    }
    i
}

fn push_next(q: &mut Vec<u32>, v: u32) {
    q.push(v);
}

fn param_walk(queue: &mut Vec<u32>) {
    let mut i = 0;
    while i < queue.len() {
        queue.push(queue[i] + 1);
        i += 1;
    }
}

fn no_growth(xs: &[u32]) -> u32 {
    let mut total = 0;
    let mut i = 0;
    while i < xs.len() {
        let a = xs[i];
        let b = a + 1;
        let c = b * 2;
        total += c;
        i += 1;
    }
    total
}
