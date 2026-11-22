fn outer() -> u32 {
    fn inner() -> u32 { return 1; }
    inner()
}
