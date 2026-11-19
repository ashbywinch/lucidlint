//! The one-statement placeholder body the signature rules share: `pass`, a
//! bare expression, a lone `raise`, or a `return` of nothing / None. A 6-param
//! function that does nothing is a protocol stub, not a param-list smell; a
//! stub method never touches instance state by design.

use ruff_python_ast::{Expr, Stmt};

/// The stub-body predicate, shared by `detached-method` and `long-param-list`.
pub struct StubBody;

impl StubBody {
    /// Whether the body is a protocol placeholder, not behaviour.
    pub fn is_trivial(body: &[Stmt]) -> bool {
        if body.len() > 1 {
            return false;
        }
        match body.first() {
            None => true,
            Some(Stmt::Pass(_)) => true,
            Some(Stmt::Expr(_)) => true,
            Some(Stmt::Raise(_)) => true,
            Some(Stmt::Return(r)) => match r.value.as_ref() {
                None => true,
                Some(e) => matches!(e.as_ref(), Expr::NoneLiteral(_)),
            },
            _ => false,
        }
    }
}
