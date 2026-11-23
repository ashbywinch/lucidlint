struct A;
impl A { fn a(&self) { let _ = B; } }
struct B;
impl B { fn b(&self) { let _ = A; } }
struct C;
impl C { fn c(&self) { let _ = D; } }
struct D;
impl D { fn d(&self) { let _ = C; } }
fn g() {}
fn h() {}
fn i() {}
fn j() {}
