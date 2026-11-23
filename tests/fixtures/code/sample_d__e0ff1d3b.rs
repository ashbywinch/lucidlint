struct State { x: i32 }
fn a(s: &Box<State>) {}
fn b(s: std::sync::Arc<State>) {}
fn c(s: Box<Box<State>>) {}
fn d(e: Box<Expr>) {}
