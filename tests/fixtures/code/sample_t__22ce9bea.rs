#[test]
fn t() {
    let dir = std::env::temp_dir();
    std::fs::write(dir.join("x"), b"x");
}
