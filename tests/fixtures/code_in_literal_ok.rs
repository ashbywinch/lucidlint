fn corpus() -> &'static str {
    include_str!("../../tests/fixtures/code_in_literal_ok.rs")
}

fn messages(name: &str) {
    println!("fn f() {{}}");
    let _ = format!("def {name}:");
}
