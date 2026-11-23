fn f(xs: &[u32], a: bool, b: bool) -> u32 {
    let mut n = 0;
    for x in xs {
        if a && b { n += 1; }
    }
    while n < 10 { n += 1; }
    loop { n += 1; if n > 20 { break; } }
    assert!(a || b);
    let g = |y: u32| if y > 1 { y } else { 0 };
    n + g(1)
}
