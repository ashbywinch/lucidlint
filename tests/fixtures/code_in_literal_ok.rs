fn corpus() -> &'static str {
    include_str!("../../tests/fixtures/code_in_literal_ok.rs")
}

fn messages(name: &str) {
    println!("fn f() {{}}");
    let _ = format!("def {name}:");
    // a macro argument's semantics are not knowable to the literal detector
    my_macro!("fn f() {\n    let x = 1;\n}");
}
