struct Big { a: u32, b: u32, c: u32, d: u32, e: u32 }
impl Big { fn total(&self) -> u32 { self.a } }
fn use_big(b: &Big) -> u32 { b.total() }
