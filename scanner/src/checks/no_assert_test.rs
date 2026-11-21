//! `no-assert-test`: a test body with no assertion — it proves the code runs,
//! not that it is right.

use ruff_python_ast::{AnyNodeRef, Expr, ExprAttribute, Stmt};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::skeleton::{skel_children, Q};
use crate::source::Source;
use crate::ScanState;

/// Walk a test function's full body (nested functions included) for an
/// assertion: an `assert` statement or a call to `pytest.raises` /
/// `pytest.fail` / bare `fail(`.
fn has_assertion(body: &[Stmt]) -> bool {
    let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    let mut qi = 0usize;
    while qi < queue.len() {
        if let Q::N(n) = queue[qi] {
            match n {
                AnyNodeRef::StmtAssert(_) => return true,
                AnyNodeRef::ExprCall(call) => match call.func.as_ref() {
                    Expr::Attribute(a) => {
                        // unittest: self.assertEqual/assertTrue/assertRaises/...
                        // and pytest: pytest.raises / self.fail
                        if is_assert_attr(a) {
                            return true;
                        }
                    }
                    Expr::Name(nm) if nm.id.as_str() == "fail" => return true,
                    _ => {}
                },
                _ => {}
            }
            skel_children(n, &mut queue);
        }
        qi += 1;
    }
    false
}

/// A function named `test_*` whose body contains no assertion can never
/// fail — it is a test that tests nothing.
pub fn no_assert_test_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    let mut qi = 0usize;
    while qi < queue.len() {
        if let Q::N(n) = queue[qi] {
            if let AnyNodeRef::StmtFunctionDef(f) = n {
                if f.name.as_str().starts_with("test_") && !has_assertion(&f.body) {
                    state.findings.push(Finding { logical_start: None, seam_members: Vec::new(),
                    col: 0,
                    file: state.file.to_string(),
                    line: Source::line_of(source, f.name.range().start()),
                    function: f.name.to_string(),
                    kind: "no-assert-test".into(),
                    severity: "fail".into(),
                    message: "test has no assertion — it can never fail, so it proves nothing. Add an assertion or delete the test.".to_string(), });
                }
            }
            skel_children(n, &mut queue);
        }
        qi += 1;
    }
}

/// A pytest assertion attribute (`assert*`, `raises`, `fail`).
fn is_assert_attr(attr: &ExprAttribute) -> bool {
    attr.attr.as_str().starts_with("assert") || attr.attr.as_str() == "raises" || attr.attr.as_str() == "fail"
}
