fn f() -> u32 {
    fn a() -> u32 { 1 }
    fn b() -> u32 { 2 }
    a() + b()
}
