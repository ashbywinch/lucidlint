//! The magic-number family's vocabulary and census: which literals are magic
//! at all, which positions exempt them, and the unit tokens that name a value.

// lucidlint: ignore-file boolean-arg the flags are the walker's API contract (is-int /
// include the RHS), not unnamed modes
// lucidlint: ignore-file long-param-list the census threads the groups it fills
// (groups, labels, source) — grouping adds a hop for the one caller

use std::collections::HashSet;

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use super::expr_walk::{push_stmt_children, stmt_exprs, walk_expr_deep};

/// Offsets of numeric literals that are data-table entries, exempt from
/// magic-number: inside a single collection literal (dict/list/set/tuple,
/// nested collections allowed) where at least 3 SAME-KIND numeric siblings
/// (int vs float) sit — directly as elements, or under a BinOp/UnaryOp/
/// Compare. The literal IS the data there — `{"A": 6/9, "B": 7/9, ...}`,
/// `{"UB": (51.4, 51.6, -0.5, 0.0), ...}`, and a 4x12 grid pair in one dict
/// initializer (B8) — naming each would destroy the table. A number is
/// exempt when ANY enclosing collection's sibling group reaches the bar
/// (the geo case only clears at the dict level, not per row).
pub fn magic_table_exempt_offsets(body: &[Stmt]) -> HashSet<usize> {
    use ruff_python_ast::visitor::source_order::{walk_stmt, SourceOrderVisitor};
    struct CollectionFinder<'a> {
        collections: Vec<&'a Expr>,
    }
    impl<'a> SourceOrderVisitor<'a> for CollectionFinder<'a> {
        fn visit_expr(&mut self, e: &'a Expr) {
            if matches!(e, Expr::Dict(_) | Expr::List(_) | Expr::Set(_) | Expr::Tuple(_)) {
                self.collections.push(e);
            }
            walk_expr(self, e);
        }
    }
    fn walk_expr<'a, V: SourceOrderVisitor<'a>>(v: &mut V, e: &'a Expr) {
        ruff_python_ast::visitor::source_order::walk_expr(v, e);
    }
    let mut finder = CollectionFinder {
        collections: Vec::new(),
    };
    for s in body {
        walk_stmt(&mut finder, s);
    }
    let mut exempt: HashSet<usize> = HashSet::new();
    for c in finder.collections {
        let mut groups: std::collections::HashMap<bool, Vec<usize>> = std::collections::HashMap::new();
        census_collection(c, &mut groups);
        for offsets in groups.values() {
            if offsets.len() >= 3 {
                exempt.extend(offsets.iter().copied());
            }
        }
    }
    exempt
}

/// Offsets of the numeric literals in a module-level named constant's OWN
/// definition: the entire RHS of `X = <expr>` at module scope. The name IS
/// the why — flagging `RATE = 2 * 60` or `MARGIN = -12` would demand a
/// second name for a value the constant already names (B8).
pub fn magic_const_rhs_offsets(body: &[Stmt]) -> HashSet<usize> {
    let mut out = HashSet::new();
    for s in body {
        let value: Option<&Expr> = match s {
            Stmt::Assign(a) => Some(a.value.as_ref()),
            Stmt::AnnAssign(a) => a.value.as_deref(),
            _ => None,
        };
        let Some(v) = value else { continue };
        walk_expr_deep(v, false, &mut |e, _| {
            if let Expr::NumberLiteral(n) = e {
                out.insert(n.range().start().to_usize());
            }
        });
    }
    out
}

/// Offsets of the numeric comparators of a `len(...) OP N` length guard —
/// a tuple/collection length check is structural, not domain magic (B8).
pub fn magic_len_guard_offsets(body: &[Stmt]) -> HashSet<usize> {
    use ruff_python_ast::visitor::source_order::{walk_stmt, SourceOrderVisitor};
    struct GuardFinder {
        offsets: HashSet<usize>,
    }
    impl SourceOrderVisitor<'_> for GuardFinder {
        fn visit_expr(&mut self, e: &Expr) {
            if let Expr::Compare(c) = e {
                if let Expr::Call(call) = c.left.as_ref() {
                    if matches!(call.func.as_ref(), Expr::Name(n) if n.id.as_str() == "len") {
                        for cmp in &c.comparators {
                            if let Expr::NumberLiteral(n) = cmp {
                                self.offsets.insert(n.range().start().to_usize());
                            }
                        }
                    }
                }
            }
            walk_expr(self, e);
        }
    }
    fn walk_expr<'a, V: SourceOrderVisitor<'a>>(v: &mut V, e: &'a Expr) {
        ruff_python_ast::visitor::source_order::walk_expr(v, e);
    }
    let mut finder = GuardFinder {
        offsets: HashSet::new(),
    };
    for s in body {
        walk_stmt(&mut finder, s);
    }
    finder.offsets
}

/// Census a collection literal's subtree: numbers whose op-parent chain up to
/// this collection passes only through collection literals.
fn census_collection(e: &Expr, groups: &mut std::collections::HashMap<bool, Vec<usize>>) {
    match e {
        Expr::Dict(d) => {
            for item in &d.items {
                if let Some(k) = &item.key {
                    census_value(k, groups);
                }
                census_value(&item.value, groups);
            }
        }
        Expr::List(l) => {
            for el in &l.elts {
                census_value(el, groups);
            }
        }
        Expr::Set(s) => {
            for el in &s.elts {
                census_value(el, groups);
            }
        }
        Expr::Tuple(t) => {
            for el in &t.elts {
                census_value(el, groups);
            }
        }
        _ => {}
    }
}

/// G4: offsets of the numeric literals that ARE magic-number candidates —
pub fn magic_number_candidates(body: &[Stmt], source: &str) -> HashSet<usize> {
    let mut out = HashSet::new();
    let mut sq: Vec<&Stmt> = body.iter().collect();
    while let Some(s) = sq.pop() {
        for e in stmt_exprs(s) {
            walk_expr_deep(e, true, &mut |x, _| {
                if let Expr::NumberLiteral(n) = x {
                    let text = source[n.range()].to_string();
                    if text.len() >= 2 {
                        let prefix = &text[..2];
                        if is_radix_prefix(prefix) {
                            return;
                        }
                    }
                    let value = match &n.value {
                        ruff_python_ast::Number::Int(i) => i.to_string(),
                        ruff_python_ast::Number::Float(f) => f.to_string(),
                        ruff_python_ast::Number::Complex { .. } => return,
                    };
                    if matches!(value.as_str(), "0" | "1" | "2") {
                        return; // never magic anyway
                    }
                    out.insert(n.range().start().to_usize());
                }
            });
        }
        match s {
            Stmt::FunctionDef(f) => {
                for b in &f.body {
                    sq.push(b);
                }
            }
            _ => push_stmt_children(s, &mut sq),
        }
    }
    out
}

/// G4: per-file exemption-kind labels among the file's magic-number
/// candidates — the named exemptions (data-table, constant-definition,
/// length-guard, unit-named) that cover at least one candidate, plus
/// "position/index" when a candidate is exempted by NO named exemption (the
/// parent-is-op position skip). The stale magic-number message names
/// exactly these — never unit/constant/table for a file whose literals are
/// indices. Empty when the file holds no candidates (nothing is exempted).
pub fn magic_exemption_labels(
    body: &[Stmt],
    source: &str,
    table: &HashSet<usize>,
    const_rhs: &HashSet<usize>,
    len_guard: &HashSet<usize>,
    unit_named: &HashSet<usize>,
) -> Vec<&'static str> {
    let candidates = magic_number_candidates(body, source);
    if candidates.is_empty() {
        return Vec::new();
    }
    let covers = |set: &HashSet<usize>| candidates.iter().any(|o| set.contains(o));
    let mut labels = Vec::new();
    if covers(unit_named) {
        labels.push("unit-named");
    }
    if covers(const_rhs) {
        labels.push("constant-definition");
    }
    if covers(table) {
        labels.push("data-table");
    }
    if covers(len_guard) {
        labels.push("length-guard");
    }
    let any_unnamed = candidates
        .iter()
        .any(|o| !table.contains(o) && !const_rhs.contains(o) && !len_guard.contains(o) && !unit_named.contains(o));
    if any_unnamed {
        labels.push("position/index");
    }
    labels
}

/// A direct element of a collection: nested collections recurse; a number
/// (bare or under a BinOp/UnaryOp/Compare) is a same-kind data candidate;
/// anything else (calls, subscripts, comprehensions) stops the census — not
fn census_value(e: &Expr, groups: &mut std::collections::HashMap<bool, Vec<usize>>) {
    match e {
        Expr::Dict(_) | Expr::List(_) | Expr::Set(_) | Expr::Tuple(_) => census_collection(e, groups),
        Expr::NumberLiteral(n) => push_census_literal(n, groups),
        Expr::BinOp(b) => {
            census_number(b.left.as_ref(), groups);
            census_number(b.right.as_ref(), groups);
        }
        Expr::UnaryOp(u) => census_number(u.operand.as_ref(), groups),
        Expr::Compare(c) => {
            census_number(c.left.as_ref(), groups);
            for cmp in &c.comparators {
                census_number(cmp, groups);
            }
        }
        _ => {}
    }
}

fn push_census_literal(
    n: &ruff_python_ast::ExprNumberLiteral,
    groups: &mut std::collections::HashMap<bool, Vec<usize>>,
) {
    let is_int = matches!(n.value, ruff_python_ast::Number::Int(_));
    let value = match &n.value {
        ruff_python_ast::Number::Int(i) => i.to_string(),
        ruff_python_ast::Number::Float(f) => f.to_string(),
        ruff_python_ast::Number::Complex { .. } => return,
    };
    if matches!(value.as_str(), "0" | "1" | "2") {
        return; // never magic anyway
    }
    groups.entry(is_int).or_default().push(n.range().start().to_usize());
}

// =====================================================================
// advisory refactorings (warn): the detection half of Fowler refactorings
// whose fixes are too involved to auto-apply. Each message names the
// refactoring so the agent can hand-apply it (or a future fix can).

fn census_number(e: &Expr, groups: &mut std::collections::HashMap<bool, Vec<usize>>) {
    if let Expr::NumberLiteral(n) = e {
        push_census_literal(n, groups);
    }
}

/// A radix prefix (`0x`, `0o`, `0b`) — a literal that states its base.
pub fn is_radix_prefix(prefix: &str) -> bool {
    prefix.eq_ignore_ascii_case("0x") || prefix.eq_ignore_ascii_case("0o") || prefix.eq_ignore_ascii_case("0b")
}
