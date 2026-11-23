struct A;
struct B;
struct C;
struct D;
fn g() {}
fn h() {}
fn i() {}
fn j() {}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a() { let _ = A; let _ = B; let _ = C; let _ = D; let _ = g(); let _ = h(); let _ = i(); let _ = j(); }
}
