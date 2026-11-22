fn if_else(flag: bool, xs: &[u32], ys: &[u32]) -> u32 {
    let mut a = 0;
    if flag {
        for x in xs {
            a += *x;
        }
    } else {
        for y in ys {
            a += *y;
        }
    }
    a
}

fn same_block(xs: &[u32], ys: &[u32]) -> u32 {
    let mut a = 0;
    for x in xs {
        a += *x;
    }
    for y in ys {
        a += *y;
    }
    a
}

fn else_if(n: u32, xs: &[u32], ys: &[u32]) -> u32 {
    let mut a = 0;
    if n == 0 {
        for x in xs {
            a += *x;
        }
    } else if n == 1 {
        for y in ys {
            a += *y;
        }
    }
    a
}
