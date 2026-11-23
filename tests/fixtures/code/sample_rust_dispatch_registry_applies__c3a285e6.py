pub fn route(sel: &str, n: i32) -> i32 {
    if sel == "a" {
        return n + 1;
    }
    if sel == "b" {
        return n * 2;
    }
    if sel == "c" {
        return n - 1;
    }
    -1
}
