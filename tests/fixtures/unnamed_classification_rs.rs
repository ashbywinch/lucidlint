enum Op {
    Add,
    Sub,
    Mul,
}

fn describe(op: &Op) -> &'static str {
    let label = "op";
    match op {
        Op::Add => "Add",
        Op::Sub => "Sub",
        Op::Mul => "Mul",
    }
}

fn token(op: &Op) -> &'static str {
    match op {
        Op::Add => "Add",
        Op::Sub => "Sub",
        Op::Mul => "Mul",
    }
}
