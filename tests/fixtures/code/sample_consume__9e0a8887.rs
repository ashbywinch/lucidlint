struct BigRecord { a: u32, b: u32, c: u32, d: u32, e: u32 }
fn consume(r: &BigRecord) -> u32 { r.a }
