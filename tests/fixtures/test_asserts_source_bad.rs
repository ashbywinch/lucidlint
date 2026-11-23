#[cfg(test)]
mod tests {
    fn load_source() -> String {
        String::new()
    }

    fn fix(src: &str) -> String {
        src.to_string()
    }

    #[test]
    fn needle_is_code() {
        let src = load_source();
        assert!(src.contains("fn f("));
    }

    #[test]
    fn output_text_is_code() {
        let src = load_source();
        let out = fix(&src);
        assert!(out.contains("struct Foo"));
    }

    #[test]
    fn code_text_variable_prose_needle() {
        let text = std::fs::read_to_string("src/lib.rs").unwrap();
        assert!(text.contains("filter_map"));
    }

    #[test]
    fn code_literal_comparison() {
        let out = fix("src");
        assert_eq!(out, "fn f() {\n    let x = 1;\n}\n");
    }

    #[test]
    fn two_code_literals() {
        assert_eq!("fn f()", "fn f()");
    }
}
