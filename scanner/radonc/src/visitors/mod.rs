//! Visitors and blocks for cyclomatic complexity — a Rust port of
//! `radon.visitors` (the complexity visitor). Mirrors the original module's
//! structure and public names so it can be diffed against upstream radon
//! and updated when radon changes. See the crate's NOTICE for the radon
//! attribution.
//!
//! Each radon member is its own module — `Function`, `Class`, `Block`, the
//! `ComplexityVisitor` and the `CountVisitor` that counts decisions — and this
//! root re-exports them under the radon names plus the two free functions that
//! are the module's API (`code2ast`, `get_complexity`).
//!
//! The counting rules are radon-equivalent: if/elif count (+1 each, trailing
//! else 0), for/while + else, try handlers + else, assert +1 (non-recursive),
//! match minus wildcard, boolop operands-1, ternary +1, comprehension
//! generators+ifs, lambda +0 body walked. Nested functions and class bodies
//! contribute 0 to the enclosing scope.

use ruff_python_ast::ModModule;
use ruff_python_parser::{parse_module, Parsed};

mod block;
mod class;
mod complexity_visitor;
mod count_visitor;
mod function;

pub use block::Block;
pub use class::Class;
pub use complexity_visitor::ComplexityVisitor;
pub use count_visitor::CountVisitor;
pub use function::Function;

/// Parse Python source into an AST module (radon's `code2ast`).
pub fn code2ast(code: &str) -> Parsed<ModModule> {
    // the panic mirrors radon's code2ast raising SyntaxError on invalid
    // source (R26) — a parse failure here is a programming error
    // lucidlint: ignore debug-artifact the panic mirrors radon's raise (R26)
    parse_module(code).expect("invalid Python source")
}

/// The complexity of a block (function/method/class) — radon's `GET_COMPLEXITY`.
pub fn get_complexity(b: &Block) -> i32 {
    b.complexity()
}
