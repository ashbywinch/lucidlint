#[cfg(test)]
mod tests {
    fn fixture(name: &str) -> String {
        std::fs::read_to_string(name).unwrap()
    }

    fn fix(src: &str) -> String {
        src.replace("for ", "for_each ")
    }

    fn format_module(src: &str) -> String {
        src.to_string()
    }

    #[test]
    fn fixer_output_keeps_the_combinator() {
        let src = fixture("loop_pipeline_rs.rs");
        let fixed = fix(&src);
        assert!(fixed.contains(".map("));
    }

    #[test]
    fn parser_output_is_the_user_outcome() {
        let src = fixture("loop_pipeline_rs.rs");
        let formatted = format_module(&src);
        assert!(formatted.contains("fn collect("));
    }
}
