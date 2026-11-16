//! The report's own words: the header the JSON contract carries.

/// The report header — printed on every CLI run (text banner AND a `header`
/// field in --json; never under the LSP). It states the AIM so agents read
/// the intent before the findings: findings are pointers, fix rather than
/// suppress, and make every suppression reviewer-checkable (Fourth-pass).
pub const REPORT_HEADER: &str = "lucidlint — the aim is code that is maintainable, lucid, and obviously correct. Findings and suggested fixes are pointers, not orders: for each, judge the best way to make this code more maintainable, lucid, and obviously correct. Fix findings instead of suppressing them; give every suppression a why a reviewer can check";

#[cfg(test)]
mod header_tests {
    use super::REPORT_HEADER;

    #[test]
    fn report_header_names_the_three_aims() {
        assert!(REPORT_HEADER.contains("lucid"));
        assert!(REPORT_HEADER.contains("obviously correct"));
        assert!(REPORT_HEADER.contains("why a reviewer can check"));
        assert!(REPORT_HEADER.contains("pointers, not orders"));
    }
}
