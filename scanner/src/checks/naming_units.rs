// lucidlint: ignore-file boolean-arg the flag is walk_expr_deep's API contract (which
// position the node sits in), not an unnamed mode — Rust has no named arguments
// lucidlint: ignore-file closures the three local visits are one-purpose readers of
// the same body — a class here would own nothing

//! The unit vocabulary of a name: which word in `timeout_seconds` states the
//! unit, and which numeric literals a name already explains.

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use std::collections::HashSet;

use super::ast::{function_param_names, stmt_exprs, walk_expr_deep};

pub(crate) fn sub_name(s: &ruff_python_ast::ExprSubscript) -> String {
    match s.value.as_ref() {
        Expr::Name(n) => n.id.to_string(),
        _ => String::new(),
    }
}

/// The word components of an identifier — snake_case separators and
/// camelCase humps (`max_distance_km` -> [max, distance, km];
/// `maxDistanceKm` -> the same). Whole-word matching avoids the substring
/// trap: a bare `m` must be its OWN component, not a letter inside `name`.
fn name_components(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if c == '_' || c == '-' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
        } else if c.is_ascii_uppercase() {
            if prev_lower {
                out.push(std::mem::take(&mut cur));
            }
            cur.push(c.to_ascii_lowercase());
            prev_lower = false;
        } else {
            cur.push(c);
            prev_lower = true;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The DISTINCT unit tokens an identifier states — a component matching a
/// token or its plural (`minutes`, `hours`, `miles`, `seconds`, ...).
pub fn unit_tokens_of_name(name: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for comp in name_components(name) {
        for tok in UNIT_TOKENS {
            if is_unit_token(comp.as_str(), tok) && !out.contains(tok) {
                out.push(tok);
            }
        }
    }
    out
}

/// The first unit token an identifier states, if any.
pub fn unit_token_of_name(name: &str) -> Option<&'static str> {
    unit_tokens_of_name(name).into_iter().next()
}

/// The name component is a unit token, or its plural.
fn is_unit_token(component: &str, token: &str) -> bool {
    component == token
        || (component.len() == token.len() + 1 && component.starts_with(token) && component.ends_with('s'))
}

/// (P2) Magic-number literal offsets exempted by a unit-stating NAME: the
/// nearest enclosing assignment target (or any enclosing function's
/// parameter) states the unit, so the value is named — `max_walk_km = 2 *
/// 30` fires nothing. The module-constant-RHS, octal/hex/binary, subscript,
/// and data-table exemptions (B8) are untouched.
pub fn magic_unit_named_offsets(body: &[Stmt]) -> HashSet<usize> {
    fn bind_target(t: &Expr) -> Option<String> {
        match t {
            Expr::Name(n) => Some(n.id.to_string()),
            Expr::Attribute(a) => Some(a.attr.to_string()),
            _ => None,
        }
    }
    fn scan_literals(e: &Expr, out: &mut HashSet<usize>) {
        walk_expr_deep(e, false, &mut |x, _| {
            if let Expr::NumberLiteral(n) = x {
                out.insert(n.range().start().to_usize());
            }
        });
    }
    fn walk(stmts: &[Stmt], params: &HashSet<String>, out: &mut HashSet<usize>) {
        for s in stmts {
            match s {
                Stmt::FunctionDef(f) => {
                    let mut p = params.clone();
                    function_param_names(&f.parameters, &mut p);
                    walk(&f.body, &p, out);
                }
                Stmt::ClassDef(c) => walk(&c.body, params, out),
                Stmt::For(f) => {
                    walk(&f.body, params, out);
                    walk(&f.orelse, params, out);
                }
                Stmt::While(w) => {
                    walk(&w.body, params, out);
                    walk(&w.orelse, params, out);
                }
                Stmt::If(i) => {
                    walk(&i.body, params, out);
                    for cl in &i.elif_else_clauses {
                        walk(&cl.body, params, out);
                    }
                }
                Stmt::Try(t) => {
                    walk(&t.body, params, out);
                    for h in &t.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                        walk(&eh.body, params, out);
                    }
                    walk(&t.orelse, params, out);
                    walk(&t.finalbody, params, out);
                }
                Stmt::With(w) => walk(&w.body, params, out),
                Stmt::Match(m) => {
                    for case in &m.cases {
                        walk(&case.body, params, out);
                    }
                }
                _ => {
                    let target_unit = match s {
                        Stmt::Assign(a) => a
                            .targets
                            .iter()
                            .any(|t| bind_target(t).is_some_and(|n| name_states_unit(&n))),
                        Stmt::AnnAssign(a) => bind_target(&a.target).is_some_and(|n| name_states_unit(&n)),
                        Stmt::AugAssign(a) => bind_target(&a.target).is_some_and(|n| name_states_unit(&n)),
                        _ => false,
                    };
                    if target_unit || params.iter().any(|p| name_states_unit(p)) {
                        for e in stmt_exprs(s) {
                            scan_literals(e, out);
                        }
                    }
                }
            }
        }
    }
    let empty: HashSet<String> = HashSet::new();
    let mut out = HashSet::new();
    walk(body, &empty, &mut out);
    out
}

/// Unit tokens — words that state a physical unit (plan P2). A name or
/// comment carrying one states the value's unit: the literal is either
/// named by its surroundings (the assignment-target/parameter exemption) or
/// deserves the pint-suggestion message.
pub(crate) const UNIT_TOKENS: &[&str] = &[
    "km", "mile", "m", "cm", "mm", "min", "minute", "sec", "second", "hour", "day", "week", "month", "year", "mph",
    "kmh", "kg", "watt",
];

/// Does an identifier state a unit (P2): the nearest enclosing assignment
/// target or parameter such as `max_walk_km`, `distance_m`.
pub(crate) fn name_states_unit(name: &str) -> bool {
    unit_token_of_name(name).is_some()
}
