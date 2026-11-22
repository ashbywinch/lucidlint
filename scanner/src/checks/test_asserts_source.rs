//! `test-asserts-source`: a test asserts on the source text it fed the code
//! instead of on the parsed finding or the code's output. A source assertion
//! pins the input, so it passes even when the code under test does nothing.
//!
//! The origin model is `SourceInputs`: a name is a fixture input at an
//! assertion when its last UNCONDITIONAL binding in the enclosing scope was a
//! fixture/source read. A branch body sees its own copy of the inputs, so a
//! binding there is visible to assertions inside that branch and cannot
//! rewrite the origin for statements after it — a conditional rebind has no
//! single origin at the join, and guessing one is how a source assertion gets
//! missed or an output assertion gets flagged.

use ruff_python_ast::{CmpOp, Expr, Stmt};
use ruff_text_size::Ranged;
use std::collections::HashSet;

use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

const MESSAGE: &str = "this test asserts on the source text it fed the code, not on the behaviour under test — a source assertion pins the input, so it passes even when the code does nothing. Assert the parsed finding/output instead.";

/// The test-quality pass: pair each assertion with the names whose last
/// unconditional binding was a fixture/source read in the same scope.
pub fn test_asserts_source_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    walk_body(state, body, source, "", &mut SourceInputs::default());
}

/// The names whose last unconditional binding was a fixture/source read — the
/// test's INPUT. Every other binding (a fixer's `fixed`, a parser's `out`, a
/// parsed finding) is an OUTPUT and never enters the set.
#[derive(Clone, Default)]
struct SourceInputs {
    source: HashSet<String>,
}

impl SourceInputs {
    fn record(&mut self, name: &str, is_source: bool) {
        if is_source {
            self.source.insert(name.to_string());
        } else {
            self.source.remove(name);
        }
    }

    fn contains(&self, name: &str) -> bool {
        self.source.contains(name)
    }

    /// A branch sees the enclosing bindings but its writes stay local: the
    /// clone is walked and dropped at the join.
    fn branch(&self) -> Self {
        self.clone()
    }

    fn is_source(&self, expr: &Expr) -> bool {
        matches!(expr, Expr::Name(name) if self.contains(name.id.as_str()))
    }
}

fn walk_body(state: &mut ScanState, body: &[Stmt], source: &str, function: &str, inputs: &mut SourceInputs) {
    for stmt in body {
        match stmt {
            Stmt::Assign(a) => {
                if let [Expr::Name(name)] = a.targets.as_slice() {
                    inputs.record(name.id.as_str(), is_source_read(&a.value));
                }
            }
            Stmt::AnnAssign(a) => {
                if let Expr::Name(name) = a.target.as_ref() {
                    match a.value.as_deref() {
                        Some(value) => inputs.record(name.id.as_str(), is_source_read(value)),
                        None => inputs.record(name.id.as_str(), false),
                    }
                }
            }
            Stmt::Assert(a) if assertion_reads_source(&a.test, inputs) => {
                record(state, source, a.range().start(), function);
            }
            // an assertion helper imported by name is an expression statement,
            // not an `assert` (unittest's `assertEqual(src, "…")`)
            Stmt::Expr(e) if assertion_reads_source(&e.value, inputs) => {
                record(state, source, e.range().start(), function);
            }
            Stmt::FunctionDef(f) => {
                let mut local = inputs.branch();
                walk_body(state, &f.body, source, f.name.as_str(), &mut local);
            }
            Stmt::ClassDef(c) => {
                let mut class = inputs.branch();
                walk_body(state, &c.body, source, function, &mut class);
            }
            Stmt::If(i) => {
                for body in std::iter::once(&i.body).chain(i.elif_else_clauses.iter().map(|clause| &clause.body)) {
                    let mut branch = inputs.branch();
                    walk_body(state, body, source, function, &mut branch);
                }
            }
            Stmt::For(f) => {
                let mut branch = inputs.branch();
                walk_body(state, &f.body, source, function, &mut branch);
                let mut orelse = inputs.branch();
                walk_body(state, &f.orelse, source, function, &mut orelse);
            }
            Stmt::While(w) => {
                let mut branch = inputs.branch();
                walk_body(state, &w.body, source, function, &mut branch);
                let mut orelse = inputs.branch();
                walk_body(state, &w.orelse, source, function, &mut orelse);
            }
            Stmt::With(w) => {
                let mut branch = inputs.branch();
                walk_body(state, &w.body, source, function, &mut branch);
            }
            Stmt::Try(t) => {
                let mut body = inputs.branch();
                walk_body(state, &t.body, source, function, &mut body);
                for handler in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
                    let mut branch = inputs.branch();
                    walk_body(state, &h.body, source, function, &mut branch);
                }
                let mut orelse = inputs.branch();
                walk_body(state, &t.orelse, source, function, &mut orelse);
                let mut finalbody = inputs.branch();
                walk_body(state, &t.finalbody, source, function, &mut finalbody);
            }
            Stmt::Match(m) => {
                for case in &m.cases {
                    let mut branch = inputs.branch();
                    walk_body(state, &case.body, source, function, &mut branch);
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

/// The assertion inspects a fixture-input name as text, rather than feeding it
/// to the code under test. `fixed`, `out`, and parsed findings are outputs and
/// legitimately strings; `SourceInputs` never records them.
fn assertion_reads_source(test: &Expr, inputs: &SourceInputs) -> bool {
    match test {
        Expr::Compare(cmp) if cmp.ops.len() == 1 => compare_reads_source(cmp, inputs),
        Expr::Call(call) => call_reads_source(call, inputs),
        Expr::UnaryOp(unary) => assertion_reads_source(&unary.operand, inputs),
        Expr::BoolOp(boolean) => boolean.values.iter().any(|v| assertion_reads_source(v, inputs)),
        _ => false,
    }
}

fn compare_reads_source(cmp: &ruff_python_ast::ExprCompare, inputs: &SourceInputs) -> bool {
    let right = &cmp.comparators[0];
    match cmp.ops[0] {
        CmpOp::In | CmpOp::NotIn => {
            inputs.is_source(right) && (is_text_literal(&cmp.left) || inputs.is_source(&cmp.left))
        }
        CmpOp::Eq | CmpOp::NotEq => {
            (inputs.is_source(&cmp.left) && is_text_literal(right))
                || (is_text_literal(&cmp.left) && inputs.is_source(right))
        }
        _ => false,
    }
}

/// unittest's assertion methods and helpers that inspect their argument:
/// `assertIn(member, src)`, `assertEqual(src, literal)`, `assertRegex(src, ..)`,
/// and `assertTrue(<expr>)` recurse into the expression.
fn call_reads_source(call: &ruff_python_ast::ExprCall, inputs: &SourceInputs) -> bool {
    let method = match call.func.as_ref() {
        Expr::Attribute(attribute) => attribute.attr.as_str(),
        // an assertion helper imported by name (`from helpers import assertEqual`)
        Expr::Name(name) => name.id.as_str(),
        _ => return false,
    };
    let args = call.arguments.args.as_ref();
    match method {
        "assertIn" | "assertNotIn" => args.get(1).is_some_and(|arg| inputs.is_source(arg)),
        "assertEqual" | "assertNotEqual" | "assertRegex" | "assertNotRegex" | "assertStartsWith" | "assertEndsWith" => {
            args.first().is_some_and(|arg| inputs.is_source(arg))
        }
        "assertTrue" | "assertFalse" => args.first().is_some_and(|arg| assertion_reads_source(arg, inputs)),
        _ => false,
    }
}

fn is_text_literal(expr: &Expr) -> bool {
    matches!(expr, Expr::StringLiteral(_) | Expr::BytesLiteral(_) | Expr::FString(_))
}

#[cfg(test)]
mod tests {
    use super::SourceInputs;

    #[test]
    fn an_unconditional_output_binding_clears_the_input() {
        let mut inputs = SourceInputs::default();
        inputs.record("src", true);
        inputs.record("src", false);
        assert!(!inputs.contains("src"));
    }

    #[test]
    fn a_source_binding_after_an_output_is_the_input_again() {
        let mut inputs = SourceInputs::default();
        inputs.record("src", false);
        inputs.record("src", true);
        assert!(inputs.contains("src"));
    }

    #[test]
    fn a_branch_binding_does_not_rewrite_the_enclosing_scope() {
        let mut inputs = SourceInputs::default();
        inputs.record("src", true);
        let mut conditionally_rebound = inputs.branch();
        conditionally_rebound.record("src", false);
        assert!(inputs.contains("src"), "the join keeps the enclosing binding");
        assert!(
            !conditionally_rebound.contains("src"),
            "the branch sees its own output rebind"
        );
    }
}
