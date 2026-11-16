//! `duplicate-expression` (#104): the same substantive expression in two
//! places. One copy can drift from the others, so the shape earns a name —
//! a helper, a predicate, or a type alias.
// lucidlint: ignore-file complexity the expression-shape walker is ONE exhaustive
// dispatch over every Expr variant — the arm count is the AST's surface, not
// branching, and splitting it would scatter a single mapping
// lucidlint: ignore-file boolean-arg the two flags are the walker's API contract
// (keep the callee's name, abstract the literal key), not unnamed modes — Rust
// has no named arguments
// lucidlint: ignore-file large-function the key walker stays one mapping so every
// Expr variant is handled in one place — the alternative is ten partial walkers

use std::collections::HashSet;

use ruff_python_ast::{Expr, ExprCall, Operator, Stmt, StmtFunctionDef};
use ruff_text_size::Ranged;

use crate::{finding::Finding, source::col_of, source::line_of, ScanState};

use super::shared_path::{fstring_shape, path_call, source_text_of};

/// One substantive expression's occurrence: the canonical shape, the source
/// text for the message, and the extent for the parent/child dedup.
#[derive(Clone)]
pub struct ExpressionOccurrence {
    pub key: String,
    pub line: usize,
    pub function: String,
    pub text: String,
    pub nodes: usize,
    pub start: (usize, usize),
    pub end: (usize, usize),
}

impl ExpressionOccurrence {
    fn starts_before(&self, other: &Self) -> bool {
        self.start.0 < other.start.0 || (self.start.0 == other.start.0 && self.start.1 <= other.start.1)
    }

    fn ends_after(&self, other: &Self) -> bool {
        self.end.0 > other.end.0 || (self.end.0 == other.end.0 && self.end.1 >= other.end.1)
    }

    fn contains(&self, other: &Self) -> bool {
        self.starts_before(other) && self.ends_after(other)
    }
}

/// Collect a function's substantive expressions. The merge resolves the same
/// shape across the repo; this pass only walks the body (nested defs/classes
/// are their own scopes).
pub fn collect_duplicate_expressions(state: &mut ScanState, f: &StmtFunctionDef) {
    use ruff_python_ast::visitor::source_order::SourceOrderVisitor;
    if state.is_test {
        return;
    }
    let mut probe = ExprProbe {
        occurrences: Vec::new(),
        source: state.source,
        function: f.name.to_string(),
    };
    for stmt in &f.body {
        probe.visit_stmt(stmt);
    }
    state.duplicate_expressions.extend(probe.occurrences);
}

struct ExprProbe<'a> {
    occurrences: Vec<ExpressionOccurrence>,
    source: &'a str,
    function: String,
}

impl<'a> ruff_python_ast::visitor::source_order::SourceOrderVisitor<'a> for ExprProbe<'a> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        if !is_path_root(expr) && expression_is_substantive(expr) {
            let key = expression_key(expr, self.source, false, false);
            if !key.starts_with('<') {
                let range = expr.range();
                self.occurrences.push(ExpressionOccurrence {
                    key,
                    line: line_of(self.source, range.start()),
                    function: self.function.clone(),
                    text: collapse_ws(&source_text_of(expr, self.source)),
                    nodes: expr_node_count(expr),
                    start: (line_of(self.source, range.start()), col_of(self.source, range.start())),
                    end: (line_of(self.source, range.end()), col_of(self.source, range.end())),
                });
            }
        }
        ruff_python_ast::visitor::source_order::walk_expr(self, expr);
    }

    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        if matches!(stmt, Stmt::FunctionDef(_) | Stmt::ClassDef(_) | Stmt::Expr(_)) {
            return;
        }
        ruff_python_ast::visitor::source_order::walk_stmt(self, stmt);
    }
}

/// A call to a language conversion is a step, but not the domain step that
/// makes an expression worth a name.
fn generic_call(call: &ExprCall) -> bool {
    const GENERIC: &[&str] = &[
        "str",
        "int",
        "float",
        "bool",
        "len",
        "list",
        "dict",
        "tuple",
        "set",
        "round",
        "sum",
        "any",
        "all",
        "next",
        "min",
        "max",
        "abs",
        "repr",
        "bytes",
        "type",
        "isinstance",
        "hasattr",
        "getattr",
        "range",
    ];
    let name = call_name(call);
    GENERIC.contains(&name)
}

fn call_name(call: &ExprCall) -> &str {
    match call.func.as_ref() {
        Expr::Name(n) => n.id.as_str(),
        Expr::Attribute(a) => a.attr.as_str(),
        _ => "",
    }
}

/// The expression-complexity floor (#104): a repeated expression is reported
/// only when it is substantial enough that a name states something. One-liners
/// (`json.loads(path.read_text())`) stay out; the complex shapes — the 25+
/// node formulas, builders and predicates — are the target. The RULES.md row
/// describes this floor; this constant is the executable source.
const EXPRESSION_NODE_FLOOR: usize = 15;

/// The agreed gate for #104. The ticket's original filter (a literal or a
/// call chain, >=2 modules, >=3 uses) kept one-step reads and missed the
/// complex duplicates that live in two modules. Substance is the floor;
/// frequency is not a gate: any two occurrences, in any files or functions.
/// The node floor keeps a one-liner (`json.loads(path.read_text())`) out;
/// the target is the expression complex enough that a name states something.
fn expression_is_substantive(expr: &Expr) -> bool {
    use ruff_python_ast::visitor::source_order::{walk_expr, SourceOrderVisitor};
    struct Steps {
        calls: usize,
        logic: usize,
    }
    impl<'a> SourceOrderVisitor<'a> for Steps {
        fn visit_expr(&mut self, expr: &'a Expr) {
            match expr {
                Expr::Call(c) if !generic_call(c) => self.calls += 1,
                Expr::Compare(_) | Expr::BinOp(_) | Expr::UnaryOp(_) | Expr::If(_) => self.logic += 1,
                Expr::BoolOp(b) => {
                    if b.values
                        .iter()
                        .any(|v| matches!(v, Expr::Compare(_) | Expr::UnaryOp(_) | Expr::BoolOp(_)))
                    {
                        self.logic += 1;
                    }
                }
                Expr::ListComp(c) => {
                    self.logic += 1;
                    if c.generators.iter().any(|g| !g.ifs.is_empty()) {
                        self.logic += 1;
                    }
                }
                Expr::SetComp(c) => {
                    self.logic += 1;
                    if c.generators.iter().any(|g| !g.ifs.is_empty()) {
                        self.logic += 1;
                    }
                }
                Expr::Generator(c) => {
                    self.logic += 1;
                    if c.generators.iter().any(|g| !g.ifs.is_empty()) {
                        self.logic += 1;
                    }
                }
                Expr::DictComp(c) => {
                    self.logic += 1;
                    if c.generators.iter().any(|g| !g.ifs.is_empty()) {
                        self.logic += 1;
                    }
                }
                _ => {}
            }
            walk_expr(self, expr);
        }
    }
    let mut steps = Steps { calls: 0, logic: 0 };
    steps.visit_expr(expr);
    let substantive = steps.calls >= 2
        || steps.logic >= 2
        || (steps.calls >= 1 && steps.logic >= 1 && !matches!(expr, Expr::BoolOp(_) | Expr::If(_)));
    substantive && expr_node_count(expr) >= EXPRESSION_NODE_FLOOR
}

fn expr_node_count(expr: &Expr) -> usize {
    use ruff_python_ast::visitor::source_order::{walk_expr, SourceOrderVisitor};
    struct Count(usize);
    impl<'a> SourceOrderVisitor<'a> for Count {
        fn visit_expr(&mut self, expr: &'a Expr) {
            self.0 += 1;
            walk_expr(self, expr);
        }
    }
    let mut count = Count(0);
    count.visit_expr(expr);
    count.0
}

/// A path expression is #60's finding; a nested path inside a larger
/// expression is still part of that expression's shape.
fn is_path_root(expr: &Expr) -> bool {
    match expr {
        Expr::Call(call) => path_call(call),
        Expr::BinOp(b) if matches!(b.op, Operator::Div) => {
            contains_path_literal(&b.left)
                || contains_path_literal(&b.right)
                || contains_path_call(&b.left)
                || contains_path_call(&b.right)
        }
        _ => false,
    }
}

fn contains_path_literal(expr: &Expr) -> bool {
    use ruff_python_ast::visitor::source_order::{walk_expr, SourceOrderVisitor};
    struct Find(bool);
    impl<'a> SourceOrderVisitor<'a> for Find {
        fn visit_expr(&mut self, expr: &'a Expr) {
            if matches!(expr, Expr::StringLiteral(_) | Expr::FString(_)) {
                self.0 = true;
            }
            walk_expr(self, expr);
        }
    }
    let mut find = Find(false);
    find.visit_expr(expr);
    find.0
}

fn contains_path_call(expr: &Expr) -> bool {
    use ruff_python_ast::visitor::source_order::{walk_expr, SourceOrderVisitor};
    struct Find(bool);
    impl<'a> SourceOrderVisitor<'a> for Find {
        fn visit_expr(&mut self, expr: &'a Expr) {
            if let Expr::Call(call) = expr {
                if path_call(call) {
                    self.0 = true;
                }
            }
            walk_expr(self, expr);
        }
    }
    let mut find = Find(false);
    find.visit_expr(expr);
    find.0
}

fn collapse_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The expression's shape. `callee` keeps a function's name; `key` abstracts
/// the literal a helper would take as a parameter (`.get("id")`, `x["id"]`).
fn expression_key(expr: &Expr, source: &str, callee: bool, key: bool) -> String {
    match expr {
        Expr::Name(n) => {
            if callee {
                n.id.to_string()
            } else {
                "$".to_string()
            }
        }
        Expr::Attribute(a) => {
            format!("{}.{}", expression_key(&a.value, source, false, false), a.attr)
        }
        Expr::Call(call) => {
            let name = call_name(call);
            let func = expression_key(&call.func, source, true, false);
            let args: Vec<String> = call
                .arguments
                .args
                .iter()
                .enumerate()
                .map(|(i, arg)| expression_key(arg, source, false, name == "get" && i == 0))
                .collect();
            let kwargs: Vec<String> = call
                .arguments
                .keywords
                .iter()
                .map(|kw| {
                    let arg = kw.arg.as_ref().map(|n| n.id.as_str()).unwrap_or("");
                    format!("{arg}={}", expression_key(&kw.value, source, false, false))
                })
                .collect();
            let mut all = args;
            all.extend(kwargs);
            format!("{func}({})", all.join(", "))
        }
        Expr::StringLiteral(_) if key => "<key>".to_string(),
        Expr::FString(f) => fstring_shape(f),
        Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::EllipsisLiteral(_) => source_text_of(expr, source),
        Expr::BinOp(b) => format!(
            "({} {:?} {})",
            expression_key(&b.left, source, false, false),
            b.op,
            expression_key(&b.right, source, false, false)
        ),
        Expr::BoolOp(b) => {
            let parts: Vec<String> = b
                .values
                .iter()
                .map(|v| expression_key(v, source, false, false))
                .collect();
            format!("({:?} {})", b.op, parts.join(" "))
        }
        Expr::UnaryOp(u) => format!("({:?} {})", u.op, expression_key(&u.operand, source, false, false)),
        Expr::Compare(c) => {
            let mut parts = vec![expression_key(&c.left, source, false, false)];
            for (op, comparator) in c.ops.iter().zip(&c.comparators) {
                parts.push(format!("{op:?}"));
                parts.push(expression_key(comparator, source, false, false));
            }
            format!("({})", parts.join(" "))
        }
        Expr::Subscript(s) => {
            let slice = match s.slice.as_ref() {
                Expr::StringLiteral(_) => "<key>".to_string(),
                other => expression_key(other, source, false, false),
            };
            format!("{}[{}]", expression_key(&s.value, source, false, false), slice)
        }
        Expr::If(i) => format!(
            "({} if {} else {})",
            expression_key(&i.body, source, false, false),
            expression_key(&i.test, source, false, false),
            expression_key(&i.orelse, source, false, false)
        ),
        Expr::Await(a) => format!("await {}", expression_key(&a.value, source, false, false)),
        Expr::Named(n) => format!(
            "({} := {})",
            expression_key(&n.target, source, false, false),
            expression_key(&n.value, source, false, false)
        ),
        Expr::Starred(s) => format!("*{}", expression_key(&s.value, source, false, false)),
        Expr::Lambda(l) => {
            let mut params = Vec::new();
            if let Some(p) = &l.parameters {
                for a in p.posonlyargs.iter().chain(&p.args) {
                    params.push(a.parameter.name.to_string());
                }
                if let Some(v) = &p.vararg {
                    params.push(format!("*{}", v.name));
                }
                for a in &p.kwonlyargs {
                    params.push(a.parameter.name.to_string());
                }
                if let Some(k) = &p.kwarg {
                    params.push(format!("**{}", k.name));
                }
            }
            format!(
                "lambda({}): {}",
                params.join(","),
                expression_key(&l.body, source, false, false)
            )
        }
        Expr::ListComp(c) => comprehension_key(&expression_key(&c.elt, source, false, false), &c.generators, source),
        Expr::SetComp(c) => comprehension_key(&expression_key(&c.elt, source, false, false), &c.generators, source),
        Expr::Generator(c) => comprehension_key(&expression_key(&c.elt, source, false, false), &c.generators, source),
        Expr::DictComp(c) => {
            let elt = match (&c.key, &c.value) {
                (Some(k), v) => format!(
                    "{}: {}",
                    expression_key(k, source, false, false),
                    expression_key(v, source, false, false)
                ),
                (None, v) => format!("**{}", expression_key(v, source, false, false)),
            };
            comprehension_key(&elt, &c.generators, source)
        }
        Expr::List(l) => format!(
            "[{}]",
            l.elts
                .iter()
                .map(|e| expression_key(e, source, false, false))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Tuple(t) => format!(
            "({})",
            t.elts
                .iter()
                .map(|e| expression_key(e, source, false, false))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Set(s) => format!(
            "{{{}}}",
            s.elts
                .iter()
                .map(|e| expression_key(e, source, false, false))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Dict(d) => {
            let items: Vec<String> = d
                .items
                .iter()
                .map(|item| match &item.key {
                    Some(k) => format!(
                        "{}: {}",
                        expression_key(k, source, false, false),
                        expression_key(&item.value, source, false, false)
                    ),
                    None => format!("**{}", expression_key(&item.value, source, false, false)),
                })
                .collect();
            format!("{{{}}}", items.join(", "))
        }
        _ => format!("<{}>", expr_kind(expr)),
    }
}

fn comprehension_key(elt: &str, gens: &[ruff_python_ast::Comprehension], source: &str) -> String {
    let mut parts = vec![elt.to_string()];
    for gen in gens {
        parts.push(format!(
            "for {} in {}",
            expression_key(&gen.target, source, false, false),
            expression_key(&gen.iter, source, false, false)
        ));
        for cond in &gen.ifs {
            parts.push(format!("if {}", expression_key(cond, source, false, false)));
        }
    }
    format!("comp({})", parts.join(" "))
}

fn expr_kind(expr: &Expr) -> &'static str {
    match expr {
        Expr::Yield(_) => "yield",
        Expr::YieldFrom(_) => "yieldfrom",
        _ => "expr",
    }
}

struct ExprOccurrenceRef<'a> {
    rel: &'a str,
    occ: &'a ExpressionOccurrence,
}

/// One finding per duplicated expression. The parent/child dedup keeps the
/// larger expression when a smaller one is wholly inside it and every file
/// the smaller one appears in also holds the larger one.
pub fn duplicate_expression_findings(scans: &[(String, Vec<ExpressionOccurrence>)]) -> Vec<Finding> {
    let mut groups: std::collections::HashMap<String, Vec<ExprOccurrenceRef>> = std::collections::HashMap::new();
    for (rel, exprs) in scans {
        for occ in exprs {
            groups
                .entry(occ.key.clone())
                .or_default()
                .push(ExprOccurrenceRef { rel, occ });
        }
    }
    let mut candidates: Vec<Vec<ExprOccurrenceRef>> = groups.into_values().filter(|occs| occs.len() >= 2).collect();
    candidates.sort_by(|a, b| b[0].occ.nodes.cmp(&a[0].occ.nodes));
    let mut selected: Vec<Vec<ExprOccurrenceRef>> = Vec::new();
    for occs in candidates {
        let first = &occs[0];
        let covered = selected.iter().any(|kept| {
            let parent = &kept[0];
            if parent.rel != first.rel || parent.occ.nodes <= first.occ.nodes {
                return false;
            }
            if !parent.occ.contains(first.occ) {
                return false;
            }
            let files: HashSet<&str> = kept.iter().map(|o| o.rel).collect();
            occs.iter().all(|o| files.contains(o.rel))
        });
        if !covered {
            selected.push(occs);
        }
    }
    selected
        .into_iter()
        .map(|occs| duplicate_expression_finding(&occs))
        .collect()
}

fn duplicate_expression_finding(occs: &[ExprOccurrenceRef]) -> Finding {
    let first = &occs[0];
    let count = occs.len();
    let text = truncate_text(&first.occ.text, 120);
    let mut files: Vec<&str> = occs.iter().map(|o| o.rel).collect();
    files.sort_unstable();
    files.dedup();
    let file_word = if files.len() == 1 { "file" } else { "files" };
    let where_text = if files.len() <= 3 {
        files.join(", ")
    } else {
        format!("{}, {}, and {} more", files[0], files[1], files.len() - 2)
    };
    Finding {
        logical_start: None,
        seam_members: Vec::new(),
        col: 0,
        file: first.rel.to_string(),
        line: first.occ.line,
        function: first.occ.function.clone(),
        kind: "duplicate-expression".into(),
        severity: "warn".into(),
        message: format!(
            "The same expression appears {count} times across {} {file_word} ({where_text}): `{text}`. One copy can drift from the others; make one helper, predicate, or type alias.",
            files.len()
        ),
    }
}

fn truncate_text(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}
