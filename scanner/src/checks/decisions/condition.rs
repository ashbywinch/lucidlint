//! `unnamed-condition`: three or more facts joined inline by `and`/`or` are a
//! decision with no name — the name is the fix.

use std::collections::HashSet;

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

use super::super::over_abstraction::OverAbstraction;
use super::facts::{AnswerShape, ConditionScope, DecisionFacts};

/// Where a condition sits: the source text, the enclosing scope, and the
/// module's imported aliases (a name the file imports is vocabulary, not a
/// decision noun).
#[derive(Clone, Copy)]
struct ConditionContext<'a> {
    source: &'a str,
    scope: ConditionScope<'a>,
    imported: &'a HashSet<String>,
}

/// Where the expression sits: the value of a `return`, or a test the code
/// branches on. The walk knows this; it never has to search the body.
#[derive(Clone, Copy, PartialEq)]
enum Returned {
    Value,
    Test,
}

/// The unnamed-condition rule.
pub struct UnnamedCondition;

impl UnnamedCondition {
    /// A condition is a decision the code makes. Written inline as three or more
    /// facts joined by `and`/`or`, the reader has to reconstruct what the
    /// combination means — the name is the fix, and the object the name belongs
    /// to is a fact the AST already holds.
    pub fn findings(state: &mut ScanState, body: &[Stmt], source: &str) {
        let imported: HashSet<String> = OverAbstraction::collect_imports(body, source)
            .into_iter()
            .map(|i| i.alias)
            .collect();
        let ctx = ConditionContext {
            source,
            scope: ConditionScope::default(),
            imported: &imported,
        };
        Self::walk_conditions(state, body, ctx);
    }

    fn walk_conditions(state: &mut ScanState, stmts: &[Stmt], ctx: ConditionContext<'_>) {
        for s in stmts {
            match s {
                Stmt::If(i) => {
                    Self::condition_finding(state, &i.test, ctx, Returned::Test);
                    Self::walk_conditions(state, &i.body, ctx);
                    for cl in &i.elif_else_clauses {
                        Self::walk_conditions(state, &cl.body, ctx);
                    }
                }
                Stmt::While(w) => {
                    Self::condition_finding(state, &w.test, ctx, Returned::Test);
                    Self::walk_conditions(state, &w.body, ctx);
                    Self::walk_conditions(state, &w.orelse, ctx);
                }
                Stmt::Return(r) => {
                    if let Some(v) = &r.value {
                        Self::condition_finding(state, v, ctx, Returned::Value);
                    }
                }
                Stmt::Assert(a) => Self::condition_finding(state, &a.test, ctx, Returned::Test),
                Stmt::Match(m) => {
                    for c in &m.cases {
                        if let Some(g) = &c.guard {
                            Self::condition_finding(state, g, ctx, Returned::Test);
                        }
                        Self::walk_conditions(state, &c.body, ctx);
                    }
                }
                Stmt::FunctionDef(f) => {
                    let inner = ConditionScope {
                        fn_name: f.name.as_str(),
                        class: ctx.scope.class,
                        answer: DecisionFacts::answer_shape(&f.body),
                        single_chain: DecisionFacts::body_is_one_chain(&f.body),
                    };
                    Self::walk_conditions(
                        state,
                        &f.body,
                        ConditionContext {
                            source: ctx.source,
                            scope: inner,
                            imported: ctx.imported,
                        },
                    );
                }
                Stmt::ClassDef(c) => {
                    let inner = ConditionScope {
                        fn_name: "",
                        class: Some(c.name.as_str()),
                        answer: AnswerShape::NoAnswer,
                        single_chain: false,
                    };
                    Self::walk_conditions(
                        state,
                        &c.body,
                        ConditionContext {
                            source: ctx.source,
                            scope: inner,
                            imported: ctx.imported,
                        },
                    );
                }
                Stmt::For(f) => {
                    Self::walk_conditions(state, &f.body, ctx);
                    Self::walk_conditions(state, &f.orelse, ctx);
                }
                Stmt::With(w) => Self::walk_conditions(state, &w.body, ctx),
                Stmt::Try(t) => {
                    Self::walk_conditions(state, &t.body, ctx);
                    for h in &t.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                        Self::walk_conditions(state, &eh.body, ctx);
                    }
                    Self::walk_conditions(state, &t.orelse, ctx);
                    Self::walk_conditions(state, &t.finalbody, ctx);
                }
                _ => {}
            }
        }
    }

    fn condition_finding(state: &mut ScanState, expr: &Expr, ctx: ConditionContext<'_>, placement: Returned) {
        let Some(operands) = Self::bool_operands(expr) else {
            return;
        };
        if operands.len() < 3 {
            return; // two facts joined is a condition, not an unnamed decision
        }
        if placement == Returned::Value && ctx.scope.answer == AnswerShape::SingleAnswer {
            return; // the name already exists: this function IS the predicate
        }
        if Self::is_named_decision(ctx.scope.fn_name) {
            return; // __eq__ and friends ARE the decision: the name exists
        }
        let (unit, nouns) = Self::choose_condition_unit(expr);
        let nouns: Vec<String> = nouns
            .into_iter()
            .filter(|n| !Self::names_a_type_or_constant(state, n) && !ctx.imported.contains(n))
            .collect();
        if !Self::names_a_decision(&nouns) {
            return;
        }
        let facts = Self::bool_operands(unit).map_or(operands.len(), |v| v.len());
        let message = Self::condition_message(facts, &nouns, ctx.scope.class);
        state.findings.push(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            col: 0,
            file: state.file.to_string(),
            line: Source::line_of(ctx.source, unit.range().start()),
            function: ctx.scope.fn_name.to_string(),
            kind: "unnamed-condition".into(),
            severity: "warn".into(),
            message,
        });
    }

    /// The unit to name: a parenthesised group that is about one noun and about
    /// nothing the rest of the condition is about is the decision — the reader
    /// already drew that boundary. Otherwise the whole condition is.
    fn choose_condition_unit(expr: &Expr) -> (&Expr, Vec<String>) {
        let top = DecisionFacts::unwrap_not(expr);
        if let Expr::BoolOp(b) = top {
            let mut groups = Vec::new();
            for v in &b.values {
                let child = DecisionFacts::unwrap_not(v);
                if !matches!(child, Expr::BoolOp(_)) {
                    continue;
                }
                let inner = Self::condition_nouns(child);
                if inner.len() != 1 || Self::bool_operands(child).is_none_or(|v| v.len() < 3) {
                    continue;
                }
                let outside: HashSet<String> = b
                    .values
                    .iter()
                    .filter(|other| !std::ptr::eq(*other, v))
                    .flat_map(Self::condition_nouns)
                    .collect();
                if !outside.contains(&inner[0]) {
                    groups.push((child, inner));
                }
            }
            if let [group] = groups.as_slice() {
                return (group.0, group.1.clone());
            }
        }
        (top, Self::condition_nouns(top))
    }

    /// The message names the object and the action; the signature is the
    /// extraction's business, so it is never prescribed.
    fn condition_message(facts: usize, nouns: &[String], class: Option<&str>) -> String {
        let owner = class.unwrap_or("the class this code belongs to");
        let display: Vec<String> = nouns
            .iter()
            .map(|n| {
                if n == "self" {
                    format!("this {owner}")
                } else {
                    n.clone()
                }
            })
            .collect();
        let named: Vec<&str> = display.iter().map(String::as_str).collect();
        let subject = match named.as_slice() {
            [only] => only.to_string(),
            [a, b] => format!("{a} and {b}"),
            _ => {
                let Some((last, rest)) = named.split_last() else {
                    return String::new();
                };
                format!("{}, and {last}", rest.join(", "))
            }
        };
        if nouns.len() == 1 {
            format!(
                "{facts} facts about {subject} are joined here, and nothing states what they mean together. \
             Name the decision as a predicate over {subject}, and call it here."
            )
        } else if nouns.iter().any(|n| n == "self") {
            format!(
                "{facts} facts about {subject} are joined here, and nothing states what they mean together. \
             Name the decision as a method on {owner}, and call it here."
            )
        } else {
            format!(
                "{facts} facts about {subject} are joined here, and nothing states what they mean together. \
             Name the decision as a predicate, and call it here."
            )
        }
    }

    /// The method name already states the decision: a comparison or containment
    /// operator's body IS the definition, not an unnamed condition.
    fn is_named_decision(fn_name: &str) -> bool {
        matches!(
            fn_name,
            "__eq__" | "__ne__" | "__lt__" | "__le__" | "__gt__" | "__ge__" | "__hash__" | "__contains__" | "__bool__"
        )
    }

    /// The operands name something a decision can be about: at least one noun, and
    /// not `self` alone (internal state on an object that already has a name).
    fn names_a_decision(nouns: &[String]) -> bool {
        !nouns.is_empty() && !(nouns.len() == 1 && nouns[0] == "self")
    }

    /// A noun that names a type or a constant (`Mark`, `PageKinds`, `_OUTCODE_RE`)
    /// is the vocabulary a condition is written in, not a thing it decides about —
    /// "facts about Mark" tells the reader nothing.
    fn names_a_type_or_constant(state: &ScanState, noun: &str) -> bool {
        let bare = noun.trim_start_matches('_');
        if bare.chars().next().is_some_and(|c| c.is_uppercase()) {
            return true;
        }
        let all_caps = noun.chars().all(|c| c.is_uppercase() || c.is_ascii_digit() || c == '_')
            && noun.chars().any(|c| c.is_uppercase());
        all_caps || state.class_defs.iter().any(|(name, _)| name == noun)
    }

    /// The operands of a boolean expression used as a condition, or None when the
    /// expression is not a boolean combination at all (a single test, a call, a
    /// name — already one fact).
    fn bool_operands(expr: &Expr) -> Option<Vec<&Expr>> {
        let top = DecisionFacts::unwrap_not(expr);
        if !matches!(top, Expr::BoolOp(_)) {
            return None;
        }
        let mut out = Vec::new();
        Self::flatten_bool(top, &mut out);
        Some(out)
    }

    fn flatten_bool<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
        match DecisionFacts::unwrap_not(expr) {
            Expr::BoolOp(b) => {
                for v in &b.values {
                    Self::flatten_bool(v, out);
                }
            }
            other => out.push(other),
        }
    }

    /// The distinct nouns a boolean expression is about. A free call's noun is its
    /// first argument (`len(text)` is about `text`); a method call's is its
    /// receiver (`row.get(...)` is about `row`); a comparison's is its subject.
    fn condition_nouns(expr: &Expr) -> Vec<String> {
        let Some(operands) = Self::bool_operands(expr) else {
            return Self::operand_nouns(expr);
        };
        let mut nouns: Vec<String> = Vec::new();
        for op in operands {
            for n in Self::operand_nouns(op) {
                if !nouns.contains(&n) {
                    nouns.push(n);
                }
            }
        }
        nouns.sort();
        nouns
    }

    /// Every noun one operand is about: a comparison is about both sides, an
    /// arithmetic operand about both of its terms (`self.low - tol` is about the
    /// line and the tolerance).
    fn operand_nouns(expr: &Expr) -> Vec<String> {
        let mut out = Vec::new();
        match DecisionFacts::unwrap_not(expr) {
            Expr::Compare(c) => {
                out.extend(Self::operand_nouns(&c.left));
                for comp in &c.comparators {
                    out.extend(Self::operand_nouns(comp));
                }
            }
            Expr::BinOp(b) => {
                out.extend(Self::operand_nouns(&b.left));
                out.extend(Self::operand_nouns(&b.right));
            }
            // reachable through a comparison or arithmetic operand: `a > (b or c)`
            // is one operand at the top level and three nouns inside it
            Expr::BoolOp(b) => {
                for v in &b.values {
                    out.extend(Self::operand_nouns(v));
                }
            }
            other => out.extend(Self::operand_noun(other)),
        }
        out
    }

    fn operand_noun(expr: &Expr) -> Option<String> {
        match DecisionFacts::unwrap_not(expr) {
            Expr::Name(n) => Some(n.id.to_string()),
            Expr::Attribute(_) | Expr::Subscript(_) => Self::chain_root(expr),
            Expr::Call(c) => {
                if matches!(c.func.as_ref(), Expr::Attribute(_)) {
                    Self::operand_noun(&c.func)
                } else {
                    c.arguments.args.first().and_then(Self::operand_noun)
                }
            }
            Expr::Compare(c) => Self::operand_noun(&c.left),
            Expr::UnaryOp(u) => Self::operand_noun(&u.operand),
            _ => None,
        }
    }

    /// The root name of an attribute/subscript chain (`a.b[c].d` -> `a`).
    fn chain_root(expr: &Expr) -> Option<String> {
        match expr {
            Expr::Name(n) => Some(n.id.to_string()),
            Expr::Attribute(a) => Self::chain_root(&a.value),
            Expr::Subscript(s) => Self::chain_root(&s.value),
            _ => None,
        }
    }
}
