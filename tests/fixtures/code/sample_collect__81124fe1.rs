fn collect(stack: &mut Vec<u32>) -> Vec<u32> {
    let mut out = Vec::new();
    while let Some(x) = stack.pop() {
        let a = x * 2;
        let b = a + 1;
        let c = b - 1;
        let d = c * 2;
        out.push(d);
    }
    out
}
