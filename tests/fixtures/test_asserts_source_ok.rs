#[cfg(test)]
mod tests {
    fn load_source() -> String {
        String::new()
    }

    fn scan(src: &str) -> Vec<Finding> {
        let _ = src;
        Vec::new()
    }

    #[test]
    fn findings_are_parsed() {
        let src = load_source();
        let findings = scan(&src);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].kind, "magic-number");
    }

    #[test]
    fn prose_needle_in_log() {
        let stderr = "error: bad";
        assert!(stderr.contains("could not parse"));
    }

    #[test]
    fn finding_message_is_output() {
        let findings = scan("x");
        assert!(findings[0].message.contains("fn f("));
    }
}
