struct A;
impl A { fn a(&self) { let _ = B; } }
struct B;
impl B { fn b(&self) { let _ = C; } }
struct C;
impl C { fn c(&self) { let _ = D; } }
struct D;
impl D { fn d(&self) { let _ = E; } }
struct E;
impl E { fn e(&self) { let _ = F; } }
struct F;
impl F { fn f(&self) { let _ = A; } }
fn g() {}
fn h() {}
fn i() {}
fn j() {}
