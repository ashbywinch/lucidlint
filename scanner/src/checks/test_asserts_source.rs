//! `test-asserts-source`: a test asserts on the source text it fed the code
//! instead of on the parsed finding or the code's output. A source assertion
//! pins the input, so it passes even when the code under test does nothing.

use ruff_python_ast::{CmpOp, Expr, Stmt};
use ruff_text_size::Ranged;
use std::collections::HashSet;

use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

const MESSAGE: &str = "this test asserts on the source text it fed the code, not on the behaviour under test — a source assertion pins the input, so it passes even when the code does nothing. Assert the parsed finding/output instead.";

/// The test-quality pass: pair each assertion with the names that were bound
/// directly from a fixture/source read in the same function scope.
pub fn test_asserts_source_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    walk_body(state, body, source, "", &mut HashSet::new());
}

fn walk_body(state: &mut ScanState, body: &[Stmt], source: &str, function: &str, origins: &mut HashSet<String>) {
    for stmt in body {
        match stmt {
            Stmt::Assign(a) => {
                if let [Expr::Name(name)] = a.targets.as_slice() {
                    bind(name.id.as_str(), &a.value, origins);
                }
            }
            Stmt::AnnAssign(a) => {
                if let Expr::Name(name) = a.target.as_ref() {
                    match a.value.as_deref() {
                        Some(value) => bind(name.id.as_str(), value, origins),
                        None => {
                            origins.remove(name.id.as_str());
                        }
                    }
                }
            }
            Stmt::Assert(a) if assertion_reads_source(&a.test, origins) => {
                record(state, source, a.range().start(), function);
            }
            // an assertion helper imported by name is an expression statement,
            // not an `assert` (unittest's `assertEqual(src, "…")`)
            Stmt::Expr(e) if assertion_reads_source(&e.value, origins) => {
                record(state, source, e.range().start(), function);
            }
            Stmt::FunctionDef(f) => {
                walk_body(state, &f.body, source, f.name.as_str(), &mut HashSet::new());
            }
            Stmt::ClassDef(c) => {
                walk_body(state, &c.body, source, function, origins);
            }
            Stmt::If(i) => {
                walk_body(state, &i.body, source, function, origins);
                for clause in &i.elif_else_clauses {
                    walk_body(state, &clause.body, source, function, origins);
                }
            }
            Stmt::For(f) => {
                walk_body(state, &f.body, source, function, origins);
                walk_body(state, &f.orelse, source, function, origins);
            }
            Stmt::While(w) => {
                walk_body(state, &w.body, source, function, origins);
                walk_body(state, &w.orelse, source, function, origins);
            }
            Stmt::With(w) => walk_body(state, &w.body, source, function, origins),
            Stmt::Try(t) => {
                walk_body(state, &t.body, source, function, origins);
                for handler in &t.handlers {
                    match handler {
                        ruff_python_ast::ExceptHandler::ExceptHandler(h) => {
                            walk_body(state, &h.body, source, function, origins);
                        }
                    }
                }
                walk_body(state, &t.orelse, source, function, origins);
                walk_body(state, &t.finalbody, source, function, origins);
            }
            Stmt::Match(m) => {
                for case in &m.cases {
                    walk_body(state, &case.body, source, function, origins);
                }
            }
            _ => {}
        }
    }
}

fn record(state: &mut ScanState, source: &str, offset: ruff_text_size::TextSize, function: &str) {
    state.findings.push(Finding {
        logical_start: None,
        seam_members: Vec::new(),
        col: 0,
        file: state.file.to_string(),
        line: Source::line_of(source, offset),
        function: function.to_string(),
        kind: "test-asserts-source".into(),
        severity: "warn".into(),
        message: MESSAGE.to_string(),
    });
}

/// Bind `name` to the source-origin set when the value is a fixture/source
/// read; any other assignment clears it, so an output variable named like the
/// input cannot inherit the input's origin. Branch reachability is not
/// modelled: a rebind in a branch that did not run makes this rule silent,
/// which is the side to err on — flagging an output the test legitimately
/// asserts is the false positive the rule's contract forbids.
fn bind(name: &str, value: &Expr, origins: &mut HashSet<String>) {
    if is_source_read(value) {
        origins.insert(name.to_string());
    } else {
        origins.remove(name);
    }
}

/// A fixture helper or a file read — the value is the test's INPUT.
fn is_source_read(value: &Expr) -> bool {
    let Expr::Call(call) = value else {
        return false;
    };
    match call.func.as_ref() {
        Expr::Name(name) => {
            let name = name.id.as_str();
            name == "fixture" || name == "_fixture" || name.ends_with("_fixture") || name.ends_with("fixture")
        }
        Expr::Attribute(attr) => {
            let name = attr.attr.as_str();
            matches!(name, "read_text" | "read" | "read_bytes") || name.ends_with("fixture")
        }
        _ => false,
    }
}

/// The assertion inspects a source-origin name as text, rather than feeding it
/// to the code under test. `fixed`, `out`, and parsed findings are outputs and
/// legitimately strings; they are never bound by `bind`.
fn assertion_reads_source(test: &Expr, origins: &HashSet<String>) -> bool {
    match test {
        Expr::Compare(cmp) if cmp.ops.len() == 1 => compare_reads_source(cmp, origins),
        Expr::Call(call) => call_reads_source(call, origins),
        Expr::UnaryOp(unary) => assertion_reads_source(&unary.operand, origins),
        Expr::BoolOp(boolean) => boolean.values.iter().any(|v| assertion_reads_source(v, origins)),
        _ => false,
    }
}

fn compare_reads_source(cmp: &ruff_python_ast::ExprCompare, origins: &HashSet<String>) -> bool {
    let right = &cmp.comparators[0];
    match cmp.ops[0] {
        CmpOp::In | CmpOp::NotIn => {
            is_source_name(right, origins) && (is_text_literal(&cmp.left) || is_source_name(&cmp.left, origins))
        }
        CmpOp::Eq | CmpOp::NotEq => {
            (is_source_name(&cmp.left, origins) && is_text_literal(right))
                || (is_text_literal(&cmp.left) && is_source_name(right, origins))
        }
        _ => false,
    }
}

/// unittest's assertion methods and helpers that inspect their argument:
/// `assertIn(member, src)`, `assertEqual(src, literal)`, `assertRegex(src, ..)`,
/// and `assertTrue(<expr>)` recurse into the expression.
fn call_reads_source(call: &ruff_python_ast::ExprCall, origins: &HashSet<String>) -> bool {
    let method = match call.func.as_ref() {
        Expr::Attribute(attribute) => attribute.attr.as_str(),
        // an assertion helper imported by name (`from helpers import assertEqual`)
        Expr::Name(name) => name.id.as_str(),
        _ => return false,
    };
    let args = call.arguments.args.as_ref();
    match method {
        "assertIn" | "assertNotIn" => args.get(1).is_some_and(|arg| is_source_name(arg, origins)),
        "assertEqual" | "assertNotEqual" | "assertRegex" | "assertNotRegex" | "assertStartsWith" | "assertEndsWith" => {
            args.first().is_some_and(|arg| is_source_name(arg, origins))
        }
        "assertTrue" | "assertFalse" => args.first().is_some_and(|arg| assertion_reads_source(arg, origins)),
        _ => false,
    }
}

fn is_source_name(expr: &Expr, origins: &HashSet<String>) -> bool {
    matches!(expr, Expr::Name(name) if origins.contains(name.id.as_str()))
}

fn is_text_literal(expr: &Expr) -> bool {
    matches!(expr, Expr::StringLiteral(_) | Expr::BytesLiteral(_) | Expr::FString(_))
}
