//! The two visit entry points — radon's `cc_visit` and `cc_visit_ast`.

use crate::visitors::{Block, ComplexityVisitor};

/// Visit code and return its blocks — radon's `cc_visit(code)`.
pub fn cc_visit(code: &str) -> Vec<Block> {
    let visitor = ComplexityVisitor::from_code(code);
    visitor.blocks()
}

/// Visit an AST node — radon's `cc_visit_ast(ast_node)`.
pub fn cc_visit_ast(mod_: &ruff_python_ast::ModModule) -> Vec<Block> {
    let visitor = ComplexityVisitor::from_ast(mod_);
    visitor.blocks()
}
