struct Source {
    items: Vec<u32>,
}

struct Sink {
    items: Vec<u32>,
}

impl Sink {
    fn push(&mut self, value: u32) {
        self.items.push(value);
    }
}

fn rebuild() -> Vec<u32> {
    let mut out = vec![1];
    for x in std::mem::take(&mut out) {
        out.push(x);
    }
    out
}

fn rebuild_from_snapshot() -> Vec<u32> {
    let mut out = vec![1];
    for x in out.iter().cloned().collect::<Vec<_>>() {
        out.push(x);
    }
    out
}

fn rebuild_from_field() -> Sink {
    let mut out = Sink { items: vec![1] };
    for x in out.items.clone() {
        out.push(x);
    }
    out
}

fn collect_other(other: &Source) -> Vec<u32> {
    let mut out = Vec::new();
    for x in &other.items {
        out.push(*x);
    }
    out
}
