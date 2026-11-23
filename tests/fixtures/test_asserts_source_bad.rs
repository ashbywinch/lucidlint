#[cfg(test)]
mod tests {
    fn fixture(name: &str) -> String {
        std::fs::read_to_string(name).unwrap()
    }

    #[test]
    fn inventory_has_the_loop_shape() {
        let src = fixture("loop_sequence_sibling_branches.rs");
        assert!(src.contains("fn same_block("));
    }
}
