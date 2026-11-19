//! `unnamed-classification`: an inline chain returning one constant per branch
//! is a classification with no name — the name is the fix.

use ruff_python_ast::{CmpOp, Expr, Stmt};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

use super::super::conditional_polymorphism::dispatch_key;
use super::facts::{AnswerShape, ConditionScope, DecisionFacts};

/// Whether the chain is the function's whole body or a branch inside one.
#[derive(Clone, Copy, PartialEq)]
enum Level {
    Top,
    Nested,
}

/// Where a classification sits: the source text and the enclosing scope.
#[derive(Clone, Copy)]
struct ClassificationContext<'a> {
    source: &'a str,
    scope: ConditionScope<'a>,
}

/// The unnamed-classification rule.
pub struct UnnamedClassification;

impl UnnamedClassification {
    /// A chain that sorts one value into cases IS the domain's classification.
    /// Inline, the cases have no name, so every reader reconstructs the kinds from
    /// the tests. The fix is the one #88 took: name the cases as one kind.
    pub fn findings(state: &mut ScanState, body: &[Stmt], source: &str) {
        let ctx = ClassificationContext {
            source,
            scope: ConditionScope::default(),
        };
        Self::walk_classifications(state, body, ctx, Level::Top);
    }

    fn walk_classifications(state: &mut ScanState, stmts: &[Stmt], ctx: ClassificationContext<'_>, level: Level) {
        for s in stmts {
            match s {
                Stmt::If(i) => {
                    Self::classification_chain(state, i, ctx, level);
                    Self::walk_classifications(state, &i.body, ctx, Level::Nested);
                    for cl in &i.elif_else_clauses {
                        Self::walk_classifications(state, &cl.body, ctx, Level::Nested);
                    }
                }
                Stmt::Match(m) => {
                    Self::classification_match(state, m, ctx, level);
                    for c in &m.cases {
                        Self::walk_classifications(state, &c.body, ctx, Level::Nested);
                    }
                }
                Stmt::FunctionDef(f) => {
                    let inner = ConditionScope {
                        fn_name: f.name.as_str(),
                        class: ctx.scope.class,
                        answer: DecisionFacts::answer_shape(&f.body),
                        single_chain: DecisionFacts::body_is_one_chain(&f.body),
                    };
                    Self::walk_classifications(
                        state,
                        &f.body,
                        ClassificationContext {
                            source: ctx.source,
                            scope: inner,
                        },
                        Level::Top,
                    );
                }
                Stmt::ClassDef(c) => {
                    let inner = ConditionScope {
                        fn_name: "",
                        class: Some(c.name.as_str()),
                        answer: AnswerShape::NoAnswer,
                        single_chain: false,
                    };
                    Self::walk_classifications(
                        state,
                        &c.body,
                        ClassificationContext {
                            source: ctx.source,
                            scope: inner,
                        },
                        Level::Top,
                    );
                }
                Stmt::For(f) => {
                    Self::walk_classifications(state, &f.body, ctx, Level::Nested);
                    Self::walk_classifications(state, &f.orelse, ctx, Level::Nested);
                }
                Stmt::While(w) => {
                    Self::walk_classifications(state, &w.body, ctx, Level::Nested);
                    Self::walk_classifications(state, &w.orelse, ctx, Level::Nested);
                }
                Stmt::With(w) => Self::walk_classifications(state, &w.body, ctx, Level::Nested),
                Stmt::Try(t) => {
                    Self::walk_classifications(state, &t.body, ctx, Level::Nested);
                    for h in &t.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                        Self::walk_classifications(state, &eh.body, ctx, Level::Nested);
                    }
                    Self::walk_classifications(state, &t.orelse, ctx, Level::Nested);
                    Self::walk_classifications(state, &t.finalbody, ctx, Level::Nested);
                }
                _ => {}
            }
        }
    }

    /// An if/elif chain of >= 3 arms, every test `<subject> == <constant>`, every
    /// arm returning a constant/name/attribute: a classification with no name.
    fn classification_chain(
        state: &mut ScanState,
        head: &ruff_python_ast::StmtIf,
        ctx: ClassificationContext<'_>,
        level: Level,
    ) {
        if level == Level::Top && ctx.scope.single_chain {
            return; // the chain is the whole function: its name is the name
        }
        let mut tests: Vec<&Expr> = vec![&head.test];
        let mut bodies: Vec<&[Stmt]> = vec![&head.body];
        for cl in &head.elif_else_clauses {
            if let Some(t) = &cl.test {
                tests.push(t);
                bodies.push(&cl.body);
            }
        }
        if tests.len() < 3 {
            return;
        }
        let mut subject = None;
        for t in &tests {
            let Expr::Compare(c) = DecisionFacts::unwrap_not(t) else {
                return;
            };
            if c.ops.len() != 1 || !matches!(c.ops[0], CmpOp::Eq) {
                return;
            }
            if !c.comparators.first().is_some_and(DecisionFacts::is_literal) {
                return;
            }
            let text = DecisionFacts::source_text(ctx.source, &c.left);
            if subject.as_ref().is_some_and(|s| s != &text) {
                return;
            }
            subject = Some(text);
        }
        if !bodies.iter().all(|b| DecisionFacts::returns_one_constant(b)) {
            return;
        }
        // a >=4-arm chain on one value is conditional-polymorphism's ruling; one
        // chain gets one verdict
        if tests.len() >= 4 {
            let keys: Vec<Option<String>> = tests.iter().map(|t| dispatch_key(t)).collect();
            if keys.iter().all(|k| k.is_some()) && keys.windows(2).all(|w| w[0] == w[1]) {
                return;
            }
        }
        Self::push_classification(
            state,
            ctx,
            head.range().start().to_usize(),
            tests.len(),
            subject.unwrap_or_default(),
        );
    }

    /// A match over one value whose cases each return a constant/name/attribute —
    /// the same classification, written with patterns.
    fn classification_match(
        state: &mut ScanState,
        m: &ruff_python_ast::StmtMatch,
        ctx: ClassificationContext<'_>,
        level: Level,
    ) {
        if level == Level::Top && ctx.scope.single_chain {
            return;
        }
        if m.cases.len() < 3 || !matches!(m.subject.as_ref(), Expr::Name(_) | Expr::Attribute(_)) {
            return;
        }
        if m.cases
            .iter()
            .any(|c| c.guard.is_some() || !DecisionFacts::returns_one_constant(&c.body))
        {
            return;
        }
        let subject = DecisionFacts::source_text(ctx.source, &m.subject);
        Self::push_classification(state, ctx, m.range().start().to_usize(), m.cases.len(), subject);
    }

    fn push_classification(
        state: &mut ScanState,
        ctx: ClassificationContext<'_>,
        start: usize,
        cases: usize,
        subject: String,
    ) {
        state.findings.push(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            col: 0,
            file: state.file.to_string(),
            line: Source::line_of(ctx.source, ruff_text_size::TextSize::new(start as u32)),
            function: ctx.scope.fn_name.to_string(),
            kind: "unnamed-classification".into(),
            severity: "warn".into(),
            message: format!(
                "{cases} cases of {subject} are classified here, and the classification has no name. \
             Name the cases as one kind, and return that kind."
            ),
        });
    }
}
