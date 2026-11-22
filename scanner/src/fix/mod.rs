//! The Rust fix surface — extract-method, dispatch-registry, rule-table,
//! and the loop family (loop-pipeline, loop-sequence, loop-hoist) for
//! `.rs` targets.
//!
//! The Python orchestrator's fix engine is libcst (Python-only). For a Rust
//! finding the rewrite runs here, as a lossless TEXT edit: syn gives
//! spans, we slice the original source and splice the replacement in, so
//! comments and formatting survive (rustfmt remains the house formatter).
//!
//! Scope — refuse anything more, honestly: extract-method needs a seam
//! whose free variables are a subset of the function's PARAMETERS (their
//! types come from the signature; deriving types for locals would need a
//! type checker) with no out-variables and no control-flow exit; the loop
//! fixes need the detector's exact single-accumulator shape (each refusal
//! names why, never a silent no-op).

mod dispatch;
mod extract_method;
mod loop_hoist;
mod loop_pipeline;
mod loop_sequence;
mod rule_table;
mod seam;
mod text;

pub use dispatch::fix_dispatch_registry;
pub use extract_method::fix_extract_method;
pub(crate) use loop_hoist::LoopHoist;
pub(crate) use loop_pipeline::LoopPipeline;
pub use loop_sequence::{fix_loop_sequence, loop_sequence_fixable};
pub use rule_table::RuleTable;

#[cfg(test)]
mod loop_tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing fixture {name}"))
    }

    #[test]
    fn pipeline_plain_push_becomes_map_collect() {
        let src = fixture("loop_pipeline_rs.rs");
        let out = LoopPipeline::fix(&src, 3).expect("fix applies");
        assert!(
            out.contains("let out: Vec<_> = xs.iter().map(|x| *x).collect();"),
            "{out}"
        );
        assert!(!out.contains("for x in xs"), "{out}");
        let file: syn::File = syn::parse_str(&out).expect("fixed source parses");
        assert_eq!(file.items.len(), 1);
    }

    #[test]
    fn pipeline_if_filter_becomes_filter_map() {
        let src = fixture("loop_pipeline_rs_filtered.rs");
        let out = LoopPipeline::fix(&src, 3).expect("fix applies");
        assert!(
            out.contains("let out: Vec<_> = xs.iter().filter(|x| *x > 0).map(|x| *x).collect();"),
            "{out}"
        );
    }

    #[test]
    fn sequence_shared_accumulator_becomes_concatenation() {
        let src = fixture("loop_sequence_rs_fix.rs");
        let out = fix_loop_sequence(&src, 1).expect("fix applies");
        assert!(out.contains("let out: Vec<_> = xs.iter().map(|x| *x).collect::<Vec<_>>() + xs.iter().map(|y| *y + 1).collect::<Vec<_>>();"), "{out}");
        assert!(!out.contains("for x in xs"), "{out}");
        let file: syn::File = syn::parse_str(&out).expect("fixed source parses");
        assert_eq!(file.items.len(), 1);
    }

    #[test]
    fn sequence_refuses_interleaved_statements() {
        let src = include_str!("../../../tests/fixtures/code/sample_f__af27ece8.rs");
        assert!(fix_loop_sequence(src, 1).is_err());
    }

    #[test]
    fn pipeline_fixes_a_loop_in_a_later_function() {
        // the review finding: the fixer resolved the FIRST fn, so a loop
        // in a later fn rewrote through the wrong accumulator (or refused
        // with "no for-loop at line N"). The finding line selects the
        // enclosing fn.
        let src = include_str!("../../../tests/fixtures/code/sample_collect__9a9d88be.rs");
        let out = LoopPipeline::fix(src, 7).expect("fix applies in the later fn");
        assert!(
            out.contains("let out: Vec<_> = xs.iter().map(|x| *x).collect();"),
            "{out}"
        );
        assert!(out.contains("fn first() -> u32"), "{out}");
    }
}

#[cfg(test)]
mod hoist_tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing fixture {name}"))
    }

    #[test]
    fn hoist_extracts_helper_and_flat_map() {
        let src = fixture("loop_hoist_rs_fix.rs");
        let out = LoopHoist::fix(&src, 3, "contribution").expect("fix applies");
        assert!(out.contains("fn _contribution("), "{out}");
        assert!(out.contains("filter_map"), "{out}");
        assert!(!out.contains("for x in xs"), "{out}");
        let file: syn::File = syn::parse_str(&out).expect("fixed source parses");
        assert_eq!(file.items.len(), 2);
    }

    #[test]
    fn hoist_requires_a_name_and_refuses_mid_read() {
        let src = include_str!("../../../tests/fixtures/code/sample_f__cd21b710.rs");
        assert!(LoopHoist::fix(src, 3, "").is_err());
        let read = include_str!("../../../tests/fixtures/code/sample_f__037eaf9e.rs");
        assert!(LoopHoist::fix(read, 3, "h").is_err());
    }

    #[test]
    fn hoist_option_arg_with_nested_parens_and_string() {
        // the paren scan this replaced would stop at the inner `)` of
        // `format!("({})", x)` — the span slice keeps the whole argument
        let src = include_str!("../../../tests/fixtures/code/sample_f__9939c1a6.rs");
        let out = LoopHoist::fix(src, 3, "label").expect("fix applies");
        assert!(out.contains("filter_map"), "{out}");
        assert!(out.contains("return Some(format!(\"({})\", doubled));"), "{out}");
        syn::parse_str::<syn::File>(&out).expect("fixed source parses");
    }

    #[test]
    fn hoist_never_retargets_inside_a_string_literal() {
        // the retarget is a span rewrite over the moved body, never a text
        // pass: a message naming the accumulator must survive verbatim, and
        // the literal must not read as a second push site (that flipped the
        // helper to the Option shape: filter_map + a dead `None` where an
        // unconditional single push returns T directly)
        let src = include_str!("../../../tests/fixtures/code/sample_f__cdb4f647.rs");
        let out = LoopHoist::fix(src, 3, "label").expect("fix applies");
        assert!(out.contains("\"out.push({})\""), "{out}");
        assert!(out.contains("fn _label(x: String) -> String"), "{out}");
        assert!(!out.contains("filter_map"), "{out}");
        assert!(!out.contains("None"), "{out}");
        syn::parse_str::<syn::File>(&out).expect("fixed source parses");
    }

    #[test]
    fn hoist_option_arg_with_a_block_containing_a_semicolon() {
        // the rebuild found each push by text and then skipped to the next
        // `;` — a block in the argument ends that scan mid-argument, leaving
        // the push (and an undeclared helper-local) in the helper
        let src = include_str!("../../../tests/fixtures/code/sample_f__211d96dc.rs");
        let out = LoopHoist::fix(src, 3, "label").expect("fix applies");
        assert!(
            out.contains("return Some((|y: u32| { let z = y * 2; z })(scaled));"),
            "{out}"
        );
        syn::parse_str::<syn::File>(&out).expect("fixed source parses");
    }
}
