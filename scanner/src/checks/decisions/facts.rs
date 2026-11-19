//! The decision families' shared vocabulary: the enclosing scope, the answer
//! shape, and the return/literal readers both rules ask for.

use ruff_python_ast::{Expr, Stmt, UnaryOp};
use ruff_text_size::Ranged;

/// Where a condition sits: the function that owns it (for the bare-return
/// exemption) and the class that owns that function (so the action can name
/// the object instead of asking the reader to find it).
#[derive(Clone, Copy, Default)]
pub(super) struct ConditionScope<'a> {
    pub(super) fn_name: &'a str,
    pub(super) class: Option<&'a str>,
    /// The enclosing function's answer shape, computed once from its body —
    /// the walk never compares the condition against the returns to find out
    /// whether it is one of them.
    pub(super) answer: AnswerShape,
    /// The function's body is exactly one if/elif chain or match: that chain
    /// is the function, so the function's name names the classification.
    pub(super) single_chain: bool,
}

/// What a function's `return` statements say about its answer.
#[derive(Clone, Copy, Default, PartialEq)]
pub(super) enum AnswerShape {
    /// No returns, or every return is a literal: the function decides nothing.
    #[default]
    NoAnswer,
    /// Exactly one return carries a non-literal value — that return IS the
    /// function's answer, and the function's name names it.
    SingleAnswer,
}

/// The return and literal readers both decision rules ask for.
pub(super) struct DecisionFacts;

impl DecisionFacts {
    pub(super) fn body_is_one_chain(body: &[Stmt]) -> bool {
        matches!(body, [Stmt::If(_) | Stmt::Match(_)])
    }

    pub(super) fn answer_shape(body: &[Stmt]) -> AnswerShape {
        let mut returns: Vec<&Expr> = Vec::new();
        Self::collect_returns(body, &mut returns);
        let valued = returns
            .into_iter()
            .filter(|v| !Self::is_literal(Self::unwrap_not(v)) && !matches!(Self::unwrap_not(v), Expr::NoneLiteral(_)))
            .count();
        if valued == 1 {
            AnswerShape::SingleAnswer
        } else {
            AnswerShape::NoAnswer
        }
    }

    fn collect_returns<'a>(body: &'a [Stmt], out: &mut Vec<&'a Expr>) {
        for s in body {
            match s {
                Stmt::Return(r) => {
                    if let Some(v) = &r.value {
                        out.push(v);
                    }
                }
                Stmt::If(i) => {
                    Self::collect_returns(&i.body, out);
                    for cl in &i.elif_else_clauses {
                        Self::collect_returns(&cl.body, out);
                    }
                }
                Stmt::For(f) => {
                    Self::collect_returns(&f.body, out);
                    Self::collect_returns(&f.orelse, out);
                }
                Stmt::While(w) => {
                    Self::collect_returns(&w.body, out);
                    Self::collect_returns(&w.orelse, out);
                }
                Stmt::With(w) => Self::collect_returns(&w.body, out),
                Stmt::Try(t) => {
                    Self::collect_returns(&t.body, out);
                    for h in &t.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                        Self::collect_returns(&eh.body, out);
                    }
                    Self::collect_returns(&t.orelse, out);
                    Self::collect_returns(&t.finalbody, out);
                }
                Stmt::Match(m) => {
                    for c in &m.cases {
                        Self::collect_returns(&c.body, out);
                    }
                }
                // a nested def's returns belong to that def, not this one
                Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {}
                _ => {}
            }
        }
    }

    /// The block is exactly `return <constant|name|attribute>`.
    pub(super) fn returns_one_constant(body: &[Stmt]) -> bool {
        let real: Vec<&Stmt> = body
        .iter()
        .filter(|s| !matches!(s, Stmt::Expr(e) if matches!(e.value.as_ref(), Expr::StringLiteral(_) | Expr::EllipsisLiteral(_))))
        .collect();
        let [Stmt::Return(r)] = real.as_slice() else {
            return false;
        };
        r.value
            .as_deref()
            .is_some_and(|v| matches!(Self::unwrap_not(v), Expr::Name(_) | Expr::Attribute(_)) || Self::is_literal(v))
    }

    /// A literal value: the arms of a classification yield these.
    pub(super) fn is_literal(expr: &Expr) -> bool {
        matches!(
            expr,
            Expr::StringLiteral(_)
                | Expr::BytesLiteral(_)
                | Expr::NumberLiteral(_)
                | Expr::BooleanLiteral(_)
                | Expr::NoneLiteral(_)
                | Expr::EllipsisLiteral(_)
                | Expr::FString(_)
        )
    }

    pub(super) fn source_text(source: &str, expr: &Expr) -> String {
        let range = expr.range();
        let (start, end) = (range.start().to_usize(), range.end().to_usize());
        source.get(start..end).unwrap_or_default().trim().to_string()
    }

    pub(super) fn unwrap_not(expr: &Expr) -> &Expr {
        match expr {
            Expr::UnaryOp(u) if matches!(u.op, UnaryOp::Not) => Self::unwrap_not(&u.operand),
            _ => expr,
        }
    }
}
