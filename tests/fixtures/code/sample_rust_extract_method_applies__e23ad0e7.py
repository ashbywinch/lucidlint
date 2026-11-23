pub fn enrich(items: &mut Vec<i32>, factor: i32) -> () {
    if items.is_empty() {
        return;
    }
    for it in items.iter_mut() {
        *it *= factor;
    }
}
