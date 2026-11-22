//! `code-in-literal`: a string literal carries a code block, or a snippet is
//! passed to a scan/compile/exec helper. Code lives in a real file a linter
//! and a test can read, never in a string.

use std::collections::HashSet;

use ruff_python_ast::{AnyNodeRef, Expr, Stmt};
use ruff_text_size::Ranged;

use crate::code_text::{is_code_block, is_code_helper, is_code_snippet};
use crate::finding::Finding;
use crate::skeleton::{skel_children, Q};
use crate::source::Source;
use crate::ScanState;

const MESSAGE: &str =
    "this string is code a reader cannot lint or run; move it to a real file under tests/fixtures/ and read the file.";

/// Every string literal in the module except docstrings: a multi-line code
/// block, or a literal handed to a scan/compile/exec helper.
pub fn code_in_literal_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    let mut docstrings = HashSet::new();
    mark_docstring(body, &mut docstrings);
    let mut helper_strings = HashSet::new();
    let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    let mut qi = 0usize;
    while qi < queue.len() {
        if let Q::N(node) = queue[qi] {
            match node {
                AnyNodeRef::StmtFunctionDef(f) => mark_docstring(&f.body, &mut docstrings),
                AnyNodeRef::StmtClassDef(c) => mark_docstring(&c.body, &mut docstrings),
                AnyNodeRef::ExprCall(call) => {
                    // the snippet is the helper's FIRST argument; `compile`'s
                    // filename and mode follow it and are not code
                    if is_helper(call.func.as_ref()) {
                        if let Some(Expr::StringLiteral(literal)) = call.arguments.args.first() {
                            if is_code_snippet(literal.value.to_str()) {
                                helper_strings.insert(literal.range().start().to_usize());
                            }
                        }
                    }
                }
                AnyNodeRef::ExprStringLiteral(literal) => {
                    let start = literal.range().start().to_usize();
                    if !docstrings.contains(&start)
                        && (helper_strings.contains(&start) || is_code_block(literal.value.to_str()))
                    {
                        state.findings.push(Finding {
                            logical_start: None,
                            seam_members: Vec::new(),
                            col: 0,
                            file: state.file.to_string(),
                            line: Source::line_of(source, literal.range().start()),
                            function: String::new(),
                            kind: "code-in-literal".into(),
                            severity: "warn".into(),
                            message: MESSAGE.to_string(),
                        });
                        helper_strings.remove(&start);
                    }
                }
                _ => {}
            }
            skel_children(node, &mut queue);
        }
        qi += 1;
    }
}

fn mark_docstring(body: &[Stmt], docstrings: &mut HashSet<usize>) {
    if let Some(Stmt::Expr(expr)) = body.first() {
        if let Expr::StringLiteral(literal) = expr.value.as_ref() {
            docstrings.insert(literal.range().start().to_usize());
        }
    }
}

fn is_helper(func: &Expr) -> bool {
    matches!(func, Expr::Name(name) if is_code_helper(name.id.as_str()))
}
