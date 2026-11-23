fn outer() -> u32 {
    fn inner(x: u32) -> u32 { if x > 0 { 1 } else { 0 } }
    if inner(1) > 0 { 1 } else { 0 }
}
