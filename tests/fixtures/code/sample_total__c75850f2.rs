fn total(stack: &Vec<u32>) -> u32 {
    let mut sum = 0;
    let mut seen = 0;
    while Vec::pop(&stack).is_some() {
        sum += 1;
        seen += 2;
    }
    sum + seen
}
