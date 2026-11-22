fn beta(y: u32) -> u32 {
    let mut m = y;
    for j in 0..10 {
        m += j;
    }
    m
}
