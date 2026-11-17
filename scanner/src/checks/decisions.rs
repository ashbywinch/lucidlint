//! The decision families: an inline compound condition and an inline
//! classification chain are decisions with no name — the fix names them.
// lucidlint: ignore-file complexity the condition and classification walkers are
// dispatch tables over the AST's surface — not branching
// lucidlint: ignore-file boolean-arg the two flags are the walker's API contract
// (top / includes-nested), not unnamed modes
// lucidlint: ignore-file long-param-list the condition walker threads the scope it
// reports in (owner, class, source) — grouping adds a hop for its one caller

use std::collections::HashSet;

use ruff_python_ast::{CmpOp, Expr, Stmt, UnaryOp};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::{line_of, ScanState};

use super::advisory::dispatch_key;
use super::collect_imports;

/// Where a condition sits: the function that owns it (for the bare-return
/// exemption) and the class that owns that function (so the action can name
/// the object instead of asking the reader to find it).
#[derive(Clone, Copy, Default)]
struct ConditionScope<'a> {
    fn_name: &'a str,
    class: Option<&'a str>,
    /// The enclosing function's answer shape, computed once from its body —
    /// the walk never compares the condition against the returns to find out
    /// whether it is one of them.
    answer: AnswerShape,
    /// The function's body is exactly one if/elif chain or match: that chain
    /// is the function, so the function's name names the classification.
    single_chain: bool,
}

/// What a function's `return` statements say about its answer.
#[derive(Clone, Copy, Default, PartialEq)]
enum AnswerShape {
    /// No returns, or every return is a literal: the function decides nothing.
    #[default]
    NoAnswer,
    /// Exactly one return carries a non-literal value — that return IS the
    /// function's answer, and the function's name names it.
    SingleAnswer,
}

fn body_is_one_chain(body: &[Stmt]) -> bool {
    matches!(body, [Stmt::If(_) | Stmt::Match(_)])
}

fn answer_shape(body: &[Stmt]) -> AnswerShape {
    let mut returns: Vec<&Expr> = Vec::new();
    collect_returns(body, &mut returns);
    let valued = returns
        .into_iter()
        .filter(|v| !is_literal(unwrap_not(v)) && !matches!(unwrap_not(v), Expr::NoneLiteral(_)))
        .count();
    if valued == 1 {
        AnswerShape::SingleAnswer
    } else {
        AnswerShape::NoAnswer
    }
}

/// A condition is a decision the code makes. Written inline as three or more
/// facts joined by `and`/`or`, the reader has to reconstruct what the
/// combination means — the name is the fix, and the object the name belongs
/// to is a fact the AST already holds.
pub fn unnamed_condition_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    let imported: HashSet<String> = collect_imports(body, source).into_iter().map(|i| i.alias).collect();
    walk_conditions(state, body, source, ConditionScope::default(), &imported);
}

fn walk_conditions(
    state: &mut ScanState,
    stmts: &[Stmt],
    source: &str,
    scope: ConditionScope<'_>,
    imported: &HashSet<String>,
) {
    for s in stmts {
        match s {
            Stmt::If(i) => {
                condition_finding(state, &i.test, source, scope, imported, Returned::Test);
                walk_conditions(state, &i.body, source, scope, imported);
                for cl in &i.elif_else_clauses {
                    walk_conditions(state, &cl.body, source, scope, imported);
                }
            }
            Stmt::While(w) => {
                condition_finding(state, &w.test, source, scope, imported, Returned::Test);
                walk_conditions(state, &w.body, source, scope, imported);
                walk_conditions(state, &w.orelse, source, scope, imported);
            }
            Stmt::Return(r) => {
                if let Some(v) = &r.value {
                    condition_finding(state, v, source, scope, imported, Returned::Value);
                }
            }
            Stmt::Assert(a) => condition_finding(state, &a.test, source, scope, imported, Returned::Test),
            Stmt::Match(m) => {
                for c in &m.cases {
                    if let Some(g) = &c.guard {
                        condition_finding(state, g, source, scope, imported, Returned::Test);
                    }
                    walk_conditions(state, &c.body, source, scope, imported);
                }
            }
            Stmt::FunctionDef(f) => {
                let inner = ConditionScope {
                    fn_name: f.name.as_str(),
                    class: scope.class,
                    answer: answer_shape(&f.body),
                    single_chain: body_is_one_chain(&f.body),
                };
                walk_conditions(state, &f.body, source, inner, imported);
            }
            Stmt::ClassDef(c) => {
                let inner = ConditionScope {
                    fn_name: "",
                    class: Some(c.name.as_str()),
                    answer: AnswerShape::NoAnswer,
                    single_chain: false,
                };
                walk_conditions(state, &c.body, source, inner, imported);
            }
            Stmt::For(f) => {
                walk_conditions(state, &f.body, source, scope, imported);
                walk_conditions(state, &f.orelse, source, scope, imported);
            }
            Stmt::With(w) => walk_conditions(state, &w.body, source, scope, imported),
            Stmt::Try(t) => {
                walk_conditions(state, &t.body, source, scope, imported);
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                    walk_conditions(state, &eh.body, source, scope, imported);
                }
                walk_conditions(state, &t.orelse, source, scope, imported);
                walk_conditions(state, &t.finalbody, source, scope, imported);
            }
            _ => {}
        }
    }
}

/// Where the expression sits: the value of a `return`, or a test the code
/// branches on. The walk knows this; it never has to search the body.
#[derive(Clone, Copy, PartialEq)]
enum Returned {
    Value,
    Test,
}

fn condition_finding(
    state: &mut ScanState,
    expr: &Expr,
    source: &str,
    scope: ConditionScope<'_>,
    imported: &HashSet<String>,
    placement: Returned,
) {
    let Some(operands) = bool_operands(expr) else { return };
    if operands.len() < 3 {
        return; // two facts joined is a condition, not an unnamed decision
    }
    if placement == Returned::Value && scope.answer == AnswerShape::SingleAnswer {
        return; // the name already exists: this function IS the predicate
    }
    if is_named_decision(scope.fn_name) {
        return; // __eq__ and friends ARE the decision: the name exists
    }
    let (unit, nouns) = choose_condition_unit(expr);
    let nouns: Vec<String> = nouns
        .into_iter()
        .filter(|n| !names_a_type_or_constant(state, n) && !imported.contains(n))
        .collect();
    if !names_a_decision(&nouns) {
        return;
    }
    let facts = bool_operands(unit).map_or(operands.len(), |v| v.len());
    let message = condition_message(facts, &nouns, scope.class);
    state.findings.push(Finding {
        logical_start: None,
        seam_members: Vec::new(),
        col: 0,
        file: state.file.to_string(),
        line: line_of(source, unit.range().start()),
        function: scope.fn_name.to_string(),
        kind: "unnamed-condition".into(),
        severity: "warn".into(),
        message,
    });
}

/// The unit to name: a parenthesised group that is about one noun and about
/// nothing the rest of the condition is about is the decision — the reader
/// already drew that boundary. Otherwise the whole condition is.
fn choose_condition_unit(expr: &Expr) -> (&Expr, Vec<String>) {
    let top = unwrap_not(expr);
    if let Expr::BoolOp(b) = top {
        let mut groups = Vec::new();
        for v in &b.values {
            let child = unwrap_not(v);
            if !matches!(child, Expr::BoolOp(_)) {
                continue;
            }
            let inner = condition_nouns(child);
            if inner.len() != 1 || bool_operands(child).is_none_or(|v| v.len() < 3) {
                continue;
            }
            let outside: HashSet<String> = b
                .values
                .iter()
                .filter(|other| !std::ptr::eq(*other, v))
                .flat_map(condition_nouns)
                .collect();
            if !outside.contains(&inner[0]) {
                groups.push((child, inner));
            }
        }
        if let [group] = groups.as_slice() {
            return (group.0, group.1.clone());
        }
    }
    (top, condition_nouns(top))
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

fn unwrap_not(expr: &Expr) -> &Expr {
    match expr {
        Expr::UnaryOp(u) if matches!(u.op, UnaryOp::Not) => unwrap_not(&u.operand),
        _ => expr,
    }
}

/// The operands of a boolean expression used as a condition, or None when the
/// expression is not a boolean combination at all (a single test, a call, a
/// name — already one fact).
fn bool_operands(expr: &Expr) -> Option<Vec<&Expr>> {
    let top = unwrap_not(expr);
    if !matches!(top, Expr::BoolOp(_)) {
        return None;
    }
    let mut out = Vec::new();
    flatten_bool(top, &mut out);
    Some(out)
}

fn flatten_bool<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    match unwrap_not(expr) {
        Expr::BoolOp(b) => {
            for v in &b.values {
                flatten_bool(v, out);
            }
        }
        other => out.push(other),
    }
}

/// The distinct nouns a boolean expression is about. A free call's noun is its
/// first argument (`len(text)` is about `text`); a method call's is its
/// receiver (`row.get(...)` is about `row`); a comparison's is its subject.
fn condition_nouns(expr: &Expr) -> Vec<String> {
    let Some(operands) = bool_operands(expr) else {
        return operand_nouns(expr);
    };
    let mut nouns: Vec<String> = Vec::new();
    for op in operands {
        for n in operand_nouns(op) {
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
    match unwrap_not(expr) {
        Expr::Compare(c) => {
            out.extend(operand_nouns(&c.left));
            for comp in &c.comparators {
                out.extend(operand_nouns(comp));
            }
        }
        Expr::BinOp(b) => {
            out.extend(operand_nouns(&b.left));
            out.extend(operand_nouns(&b.right));
        }
        // reachable through a comparison or arithmetic operand: `a > (b or c)`
        // is one operand at the top level and three nouns inside it
        Expr::BoolOp(b) => {
            for v in &b.values {
                out.extend(operand_nouns(v));
            }
        }
        other => out.extend(operand_noun(other)),
    }
    out
}

fn operand_noun(expr: &Expr) -> Option<String> {
    match unwrap_not(expr) {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Attribute(_) | Expr::Subscript(_) => chain_root(expr),
        Expr::Call(c) => {
            if matches!(c.func.as_ref(), Expr::Attribute(_)) {
                operand_noun(&c.func)
            } else {
                c.arguments.args.first().and_then(operand_noun)
            }
        }
        Expr::Compare(c) => operand_noun(&c.left),
        Expr::UnaryOp(u) => operand_noun(&u.operand),
        _ => None,
    }
}

/// The root name of an attribute/subscript chain (`a.b[c].d` -> `a`).
fn chain_root(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Attribute(a) => chain_root(&a.value),
        Expr::Subscript(s) => chain_root(&s.value),
        _ => None,
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
                collect_returns(&i.body, out);
                for cl in &i.elif_else_clauses {
                    collect_returns(&cl.body, out);
                }
            }
            Stmt::For(f) => {
                collect_returns(&f.body, out);
                collect_returns(&f.orelse, out);
            }
            Stmt::While(w) => {
                collect_returns(&w.body, out);
                collect_returns(&w.orelse, out);
            }
            Stmt::With(w) => collect_returns(&w.body, out),
            Stmt::Try(t) => {
                collect_returns(&t.body, out);
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                    collect_returns(&eh.body, out);
                }
                collect_returns(&t.orelse, out);
                collect_returns(&t.finalbody, out);
            }
            Stmt::Match(m) => {
                for c in &m.cases {
                    collect_returns(&c.body, out);
                }
            }
            // a nested def's returns belong to that def, not this one
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {}
            _ => {}
        }
    }
}

// ------------------------------------------------------- unnamed-classification

/// A chain that sorts one value into cases IS the domain's classification.
/// Inline, the cases have no name, so every reader reconstructs the kinds from
/// the tests. The fix is the one #88 took: name the cases as one kind.
pub fn unnamed_classification_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    walk_classifications(state, body, source, ConditionScope::default(), true);
}

fn walk_classifications(state: &mut ScanState, stmts: &[Stmt], source: &str, scope: ConditionScope<'_>, top: bool) {
    for s in stmts {
        match s {
            Stmt::If(i) => {
                classification_chain(state, i, source, scope, top);
                walk_classifications(state, &i.body, source, scope, false);
                for cl in &i.elif_else_clauses {
                    walk_classifications(state, &cl.body, source, scope, false);
                }
            }
            Stmt::Match(m) => {
                classification_match(state, m, source, scope, top);
                for c in &m.cases {
                    walk_classifications(state, &c.body, source, scope, false);
                }
            }
            Stmt::FunctionDef(f) => {
                let inner = ConditionScope {
                    fn_name: f.name.as_str(),
                    class: scope.class,
                    answer: answer_shape(&f.body),
                    single_chain: body_is_one_chain(&f.body),
                };
                walk_classifications(state, &f.body, source, inner, true);
            }
            Stmt::ClassDef(c) => {
                let inner = ConditionScope {
                    fn_name: "",
                    class: Some(c.name.as_str()),
                    answer: AnswerShape::NoAnswer,
                    single_chain: false,
                };
                walk_classifications(state, &c.body, source, inner, true);
            }
            Stmt::For(f) => {
                walk_classifications(state, &f.body, source, scope, false);
                walk_classifications(state, &f.orelse, source, scope, false);
            }
            Stmt::While(w) => {
                walk_classifications(state, &w.body, source, scope, false);
                walk_classifications(state, &w.orelse, source, scope, false);
            }
            Stmt::With(w) => walk_classifications(state, &w.body, source, scope, false),
            Stmt::Try(t) => {
                walk_classifications(state, &t.body, source, scope, false);
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                    walk_classifications(state, &eh.body, source, scope, false);
                }
                walk_classifications(state, &t.orelse, source, scope, false);
                walk_classifications(state, &t.finalbody, source, scope, false);
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
    source: &str,
    scope: ConditionScope<'_>,
    top: bool,
) {
    if top && scope.single_chain {
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
        let Expr::Compare(c) = unwrap_not(t) else { return };
        if c.ops.len() != 1 || !matches!(c.ops[0], CmpOp::Eq) {
            return;
        }
        if !c.comparators.first().is_some_and(is_literal) {
            return;
        }
        let text = source_text(source, &c.left);
        if subject.as_ref().is_some_and(|s| s != &text) {
            return;
        }
        subject = Some(text);
    }
    if !bodies.iter().all(|b| returns_one_constant(b)) {
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
    push_classification(
        state,
        source,
        head.range().start().to_usize(),
        tests.len(),
        subject.unwrap_or_default(),
        scope,
    );
}

/// A match over one value whose cases each return a constant/name/attribute —
/// the same classification, written with patterns.
fn classification_match(
    state: &mut ScanState,
    m: &ruff_python_ast::StmtMatch,
    source: &str,
    scope: ConditionScope<'_>,
    top: bool,
) {
    if top && scope.single_chain {
        return;
    }
    if m.cases.len() < 3 || !matches!(m.subject.as_ref(), Expr::Name(_) | Expr::Attribute(_)) {
        return;
    }
    if m.cases
        .iter()
        .any(|c| c.guard.is_some() || !returns_one_constant(&c.body))
    {
        return;
    }
    let subject = source_text(source, &m.subject);
    push_classification(
        state,
        source,
        m.range().start().to_usize(),
        m.cases.len(),
        subject,
        scope,
    );
}

fn push_classification(
    state: &mut ScanState,
    source: &str,
    start: usize,
    cases: usize,
    subject: String,
    scope: ConditionScope<'_>,
) {
    state.findings.push(Finding {
        logical_start: None,
        seam_members: Vec::new(),
        col: 0,
        file: state.file.to_string(),
        line: line_of(source, ruff_text_size::TextSize::new(start as u32)),
        function: scope.fn_name.to_string(),
        kind: "unnamed-classification".into(),
        severity: "warn".into(),
        message: format!(
            "{cases} cases of {subject} are classified here, and the classification has no name. \
             Name the cases as one kind, and return that kind."
        ),
    });
}

/// The block is exactly `return <constant|name|attribute>`.
fn returns_one_constant(body: &[Stmt]) -> bool {
    let real: Vec<&Stmt> = body
        .iter()
        .filter(|s| !matches!(s, Stmt::Expr(e) if matches!(e.value.as_ref(), Expr::StringLiteral(_) | Expr::EllipsisLiteral(_))))
        .collect();
    let [Stmt::Return(r)] = real.as_slice() else {
        return false;
    };
    r.value
        .as_deref()
        .is_some_and(|v| matches!(unwrap_not(v), Expr::Name(_) | Expr::Attribute(_)) || is_literal(v))
}

/// A literal value: the arms of a classification yield these.
fn is_literal(expr: &Expr) -> bool {
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

fn source_text(source: &str, expr: &Expr) -> String {
    let range = expr.range();
    let (start, end) = (range.start().to_usize(), range.end().to_usize());
    source.get(start..end).unwrap_or_default().trim().to_string()
}
