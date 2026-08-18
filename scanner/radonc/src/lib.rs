//! port of the Python radon library (MIT, see NOTICE).
pub mod complexity;
pub mod visitors;

// Re-export the primary API — the scan core uses these.
pub use complexity::{add_inner_blocks, average_complexity, cc_rank, cc_visit, cc_visit_ast, sorted_results, Order};
pub use visitors::{code2ast, get_complexity, ComplexityVisitor};

/// The scan core's radon-equivalent function CC — everything through the
/// mirrored radon API (visitors::function_cc walks one function body).
pub fn function_cc(f: &ruff_python_ast::StmtFunctionDef) -> i32 {
    let no_assert = false; // asserts count toward complexity (radon default)
    visitors::complex_counter(f, no_assert)
}
