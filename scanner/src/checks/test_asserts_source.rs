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

    /// A branch starts with the enclosing bindings. Its writes are committed
    /// only by `join`.
    fn branch(&self) -> Self {
        self.clone()
    }

    /// The must-source join: a name survives only when it is a source input
    /// on every path reaching this point. A conditional fixture load is a
    /// source input on one path and not the other, so it is not a finding
    /// after the join; a rebind to an output on one path removes the name for
    /// the same reason. This is the rule's false-positive guard: the message
    /// must be true on every path, not just the one the test happened to run.
    fn join(&mut self, branches: &[SourceInputs]) {
        let Some(first) = branches.first() else {
            return;
        };
        let mut intersection = first.source.clone();
        for branch in branches.iter().skip(1) {
            intersection.retain(|name| branch.contains(name));
        }
        self.source = intersection;
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
                let mut branches = Vec::new();
                for body in std::iter::once(&i.body).chain(i.elif_else_clauses.iter().map(|clause| &clause.body)) {
                    let mut branch = inputs.branch();
                    walk_body(state, body, source, function, &mut branch);
                    branches.push(branch);
                }
                if i.elif_else_clauses.is_empty() {
                    // no else: the fall-through path keeps the pre-if inputs
                    branches.push(inputs.branch());
                }
                inputs.join(&branches);
            }
            Stmt::For(f) => {
                // a loop reaches the join through zero iterations, a body
                // iteration, or the orelse; a name must be source on all three
                let mut branches = vec![inputs.branch()];
                let mut body = inputs.branch();
                walk_body(state, &f.body, source, function, &mut body);
                branches.push(body);
                let mut orelse = inputs.branch();
                walk_body(state, &f.orelse, source, function, &mut orelse);
                branches.push(orelse);
                inputs.join(&branches);
            }
            Stmt::While(w) => {
                // a loop reaches the join through zero iterations, a body
                // iteration, or the orelse; a name must be source on all three
                let mut branches = vec![inputs.branch()];
                let mut body = inputs.branch();
                walk_body(state, &w.body, source, function, &mut body);
                branches.push(body);
                let mut orelse = inputs.branch();
                walk_body(state, &w.orelse, source, function, &mut orelse);
                branches.push(orelse);
                inputs.join(&branches);
            }
            Stmt::With(w) => {
                // the body runs if the context manager enters
                let mut body = inputs.branch();
                walk_body(state, &w.body, source, function, &mut body);
                *inputs = body;
            }
            Stmt::Try(t) => {
                let pre = inputs.branch();
                let mut body = pre.clone();
                walk_body(state, &t.body, source, function, &mut body);
                walk_body(state, &t.orelse, source, function, &mut body);
                // a handler may run before or after a try-body rebind, so the
                // pre-try state is the only input every handler path shares
                let mut branches = vec![pre.clone(), body];
                for handler in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
                    let mut branch = pre.clone();
                    walk_body(state, &h.body, source, function, &mut branch);
                    branches.push(branch);
                }
                inputs.join(&branches);
                // the finally block runs on every path that leaves the try
                let mut finalbody = inputs.branch();
                walk_body(state, &t.finalbody, source, function, &mut finalbody);
                *inputs = finalbody;
            }
            Stmt::Match(m) => {
                // a wildcard is not proven here, so the pre-match path is a
                // reachable branch; a case-only rebind cannot be must-source
                let mut branches = vec![inputs.branch()];
                for case in &m.cases {
                    let mut branch = inputs.branch();
                    walk_body(state, &case.body, source, function, &mut branch);
                    branches.push(branch);
                }
                inputs.join(&branches);
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

/// A fixture helper, or a read of a path that names a fixtures directory —
/// the value is the test's INPUT. A raw `read_text` of a file the test or the
/// code under test wrote is the OUTPUT (`fixed`, `out`, `table`): flagging it
/// would tell a test author to stop asserting on the fixer's result.
fn is_source_read(value: &Expr) -> bool {
    let Expr::Call(call) = value else {
        return false;
    };
    match call.func.as_ref() {
        Expr::Name(name) => is_fixture_helper(name.id.as_str()),
        Expr::Attribute(attr) => {
            let name = attr.attr.as_str();
            if is_fixture_helper(name) {
                return true;
            }
            matches!(name, "read_text" | "read" | "read_bytes") && reads_a_fixture(&attr.value)
        }
        _ => false,
    }
}

fn is_fixture_helper(name: &str) -> bool {
    name == "fixture" || name == "_fixture" || name.ends_with("_fixture") || name.ends_with("fixture")
}

/// The path expression names a fixtures location: a `tests/fixtures/` string,
/// a name such as `FIXTURES`/`_CODE_FIXTURES`, or a path built from either.
/// The one expression walk handles every wrapper — a ternary, a comprehension,
/// an `await`, or a future node cannot hide the literal or name.
fn reads_a_fixture(path: &Expr) -> bool {
    let mut exprs = Vec::new();
    super::expr_walk::all_exprs(path, &mut exprs);
    exprs.iter().any(|expr| match expr {
        Expr::StringLiteral(literal) => literal.value.to_str().contains("fixtures"),
        Expr::Name(name) => name.id.as_str().to_ascii_lowercase().contains("fixture"),
        Expr::Attribute(attr) => attr.attr.as_str().contains("fixture"),
        _ => false,
    })
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
    fn the_join_keeps_a_name_source_on_every_path() {
        let mut inputs = SourceInputs::default();
        inputs.record("src", true);
        let left = inputs.branch();
        let mut right = inputs.branch();
        right.record("src", true);
        inputs.join(&[left, right]);
        assert!(inputs.contains("src"));
    }

    #[test]
    fn the_join_drops_a_name_source_on_only_one_path() {
        let mut inputs = SourceInputs::default();
        let mut loaded = inputs.branch();
        loaded.record("src", true);
        inputs.join(&[inputs.clone(), loaded]);
        assert!(
            !inputs.contains("src"),
            "a conditional fixture load is not a finding after the join"
        );
    }

    #[test]
    fn the_join_drops_a_name_rebound_to_an_output_on_one_path() {
        let mut inputs = SourceInputs::default();
        inputs.record("src", true);
        let mut output = inputs.branch();
        output.record("src", false);
        inputs.join(&[inputs.clone(), output]);
        assert!(
            !inputs.contains("src"),
            "the assertion is not on fixture text on every path"
        );
    }
}
