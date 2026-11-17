// lucidlint: ignore-file complexity the parity-locked AST walkers are single dispatch tables —
// positional in_call_func flag is the API contract, not an unnamed boolean
// lucidlint: ignore-file boolean-arg Rust has no named arguments — the traversal's
// match-arm count is table size, not branching; keep NEW functions under cc 15

//! The remaining standard-family checks, mirroring the Python implementation
//! exactly: suppressions, type-ignore, global-state, builtin-shadow, closures,
//! class-module, vague-name, strewing, except-swallows, broad-except.

// lucidlint: ignore-file closures the unit-name walker and wire-classifier probes are one-purpose local visits
// lucidlint: ignore-file long-param-list the annotation and exemption tables thread the scan shapes and their evidence flags — grouping them adds a hop for the shared tables
// decision table each — splitting them scatters a single mapping, covered by the file why
// lucidlint: ignore-file large-function the walkers and grammar tables are ONE exhaustive dispatch/
use rayon::prelude::*;
use ruff_python_ast::{
    AnyNodeRef, BoolOp, CmpOp, Decorator, Expr, ExprAttribute, ExprCall, ExprContext, Operator, Parameters, Pattern,
    Stmt, StmtClassDef, StmtFunctionDef, UnaryOp,
};
use ruff_text_size::Ranged;
use std::collections::HashSet;

use crate::{source::line_of, Finding, ScanState};

mod advisory;
mod closure_cluster;
mod closures;
mod decisions;
mod docstring;
mod duplicate_block;
mod duplicate_def;
mod duplicate_expression;
mod forwarding;
mod global_state;
mod husks;
mod latent_shapes;
mod latent_state;
mod latent_visitor;
mod loop_family;
mod magic;
mod middle_man;
mod module_names;
mod module_size;
mod process_class;
mod record_shape;
mod scattered;
mod shared_path;
mod sibling;
mod signature;
mod type_ignore;

pub use crate::suppressions::{apply_source_suppressions, logical_line_starts, parse_suppressions};
pub use advisory::{conditional_polymorphism_findings, guard_clause_findings, special_case_findings};
pub use closure_cluster::closure_cluster_findings;
pub use closures::closure_findings;
pub use decisions::{unnamed_classification_findings, unnamed_condition_findings};
pub use docstring::restating_docstring_findings;
pub use duplicate_block::duplicate_block_findings;
pub use duplicate_def::duplicate_def_findings;
pub use duplicate_expression::{collect_duplicate_expressions, duplicate_expression_findings, ExpressionOccurrence};
pub use forwarding::{collect_forwarders, forwarding_chain_findings};
pub use global_state::{global_state_findings, module_constant_names, shadow_findings};
use husks::class_methods;
pub use husks::{delegating_husk_findings, static_husk_findings};
pub use latent_shapes::{
    assembly_class_findings, misplaced_method_findings, tuple_record_findings, wide_tuple_findings,
};
use latent_state::slot_names;
pub use latent_state::{
    collect_self_assigns, data_clump_findings, duplicate_field_findings, feature_envy_findings, undeclared_findings,
};
pub use latent_visitor::latent_visitor_findings;
pub use loop_family::loop_pipeline_findings;
pub use magic::{
    magic_const_rhs_offsets, magic_exemption_labels, magic_len_guard_offsets, magic_table_exempt_offsets,
    name_states_unit, UNIT_TOKENS,
};
pub use middle_man::{collect_middle_man, middle_man_findings, MiddleManCandidate, MiddleManClass};
pub use module_names::{class_module_findings, strewing_findings, vague_name_findings};
pub use module_size::{module_findings, python_module_shape, rust_module_shape, ModuleShape, TypeShape};
pub use process_class::process_class_findings;
pub use record_shape::record_shape_findings;
pub use scattered::{collect_scattered_instances, scattered_instance_findings, ScatteredInstance};
pub use shared_path::{collect_path_expressions, shared_path_findings, PathExpression};
use sibling::collect_field_assignments;
pub use sibling::{collect_sibling_classes, sibling_value_class_findings, SiblingClass};
pub use signature::{
    boolean_arg_findings, detached_method_findings, except_findings, long_param_list_findings,
    positional_literals_findings,
};
pub use type_ignore::{noqa_findings, type_ignore_findings};

pub fn stmt_exprs(s: &Stmt) -> Vec<&Expr> {
    // the expressions directly reachable from a statement (one level)
    let mut out = Vec::new();
    match s {
        Stmt::Assign(a) => {
            push_expr_tree(a.value.as_ref(), &mut out);
            for t in &a.targets {
                push_expr_tree(t, &mut out);
            }
        }
        Stmt::AnnAssign(a) => {
            if let Some(v) = &a.value {
                push_expr_tree(v.as_ref(), &mut out);
            }
        }
        Stmt::AugAssign(a) => {
            push_expr_tree(a.value.as_ref(), &mut out);
            push_expr_tree(a.target.as_ref(), &mut out);
        }
        Stmt::Expr(e) => push_expr_tree(e.value.as_ref(), &mut out),
        Stmt::Return(r) => {
            if let Some(v) = &r.value {
                push_expr_tree(v.as_ref(), &mut out);
            }
        }
        Stmt::Raise(r) => {
            if let Some(e) = &r.exc {
                push_expr_tree(e.as_ref(), &mut out);
            }
        }
        Stmt::Assert(a) => {
            push_expr_tree(a.test.as_ref(), &mut out);
        }
        Stmt::If(i) => {
            push_expr_tree(i.test.as_ref(), &mut out);
        }
        Stmt::While(w) => {
            push_expr_tree(w.test.as_ref(), &mut out);
        }
        Stmt::For(f) => {
            push_expr_tree(f.target.as_ref(), &mut out);
            push_expr_tree(f.iter.as_ref(), &mut out);
        }
        Stmt::Delete(d) => {
            for t in &d.targets {
                push_expr_tree(t, &mut out);
            }
        }
        _ => {}
    }
    out
}

fn push_expr_tree<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
    out.push(e);
    // descend one level for Call/Attribute/Subscript/BinOp shapes
    match e {
        Expr::Call(c) => {
            push_expr_tree(c.func.as_ref(), out);
            for a in &c.arguments.args {
                push_expr_tree(a, out);
            }
            for k in &c.arguments.keywords {
                push_expr_tree(&k.value, out);
            }
        }
        Expr::Attribute(a) => push_expr_tree(&a.value, out),
        Expr::Subscript(s) => {
            push_expr_tree(s.value.as_ref(), out);
            push_expr_tree(s.slice.as_ref(), out);
        }
        Expr::BinOp(b) => {
            push_expr_tree(b.left.as_ref(), out);
            push_expr_tree(b.right.as_ref(), out);
        }
        Expr::UnaryOp(u) => push_expr_tree(&u.operand, out),
        Expr::Compare(c) => {
            push_expr_tree(c.left.as_ref(), out);
            for o in &c.comparators {
                push_expr_tree(o, out);
            }
        }
        Expr::If(e) => {
            push_expr_tree(e.test.as_ref(), out);
            push_expr_tree(e.body.as_ref(), out);
            push_expr_tree(e.orelse.as_ref(), out);
        }
        _ => {}
    }
}

fn mutates_returned_target(s: &Stmt, returned: &HashSet<String>) -> bool {
    let targets: Vec<&Expr> = match s {
        Stmt::Assign(a) => a.targets.iter().collect(),
        Stmt::AugAssign(a) => vec![a.target.as_ref()],
        Stmt::AnnAssign(a) => vec![a.target.as_ref()],
        Stmt::Delete(d) => d.targets.iter().collect(),
        _ => return false,
    };
    for t in targets {
        match t {
            Expr::Name(n) if returned.contains(n.id.as_str()) => return true,
            Expr::Subscript(sub) if returned.contains(sub_name(sub).as_str()) => return true,
            _ => {}
        }
    }
    false
}

pub(super) fn push_stmt_children<'a>(s: &'a Stmt, stack: &mut Vec<&'a Stmt>) {
    match s {
        Stmt::If(i) => {
            for b in &i.body {
                stack.push(b);
            }
            for cl in &i.elif_else_clauses {
                for b in &cl.body {
                    stack.push(b);
                }
            }
        }
        Stmt::For(f) => {
            for b in &f.body {
                stack.push(b);
            }
            for b in &f.orelse {
                stack.push(b);
            }
        }
        Stmt::While(w) => {
            for b in &w.body {
                stack.push(b);
            }
            for b in &w.orelse {
                stack.push(b);
            }
        }
        Stmt::With(w) => {
            for b in &w.body {
                stack.push(b);
            }
        }
        Stmt::Try(t) => {
            for b in &t.body {
                stack.push(b);
            }
            for h in &t.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                for b in &eh.body {
                    stack.push(b);
                }
            }
            for b in &t.orelse {
                stack.push(b);
            }
            for b in &t.finalbody {
                stack.push(b);
            }
        }
        Stmt::Match(m) => {
            for case in &m.cases {
                for b in &case.body {
                    stack.push(b);
                }
            }
        }
        Stmt::ClassDef(c) => {
            for b in &c.body {
                stack.push(b);
            }
        }
        _ => {}
    }
}

/// Base name of an annotation/except-type expression: Name -> its id,
/// Attribute -> the attribute, anything else -> None (e.g. a tuple of types).
pub fn annotation_base_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Attribute(a) => Some(a.attr.to_string()),
        _ => None,
    }
}

/// The `self.<attr> = ...` assignments across a class's methods.
pub(super) fn class_attr_names(class_body: &[Stmt]) -> HashSet<String> {
    let mut attrs = HashSet::new();
    for m in class_body {
        let Stmt::FunctionDef(f) = m else { continue };
        for s in &f.body {
            let Stmt::Assign(a) = s else { continue };
            for t in &a.targets {
                if let Expr::Attribute(at) = t {
                    if let Expr::Name(n) = at.value.as_ref() {
                        if n.id.as_str() == "self" {
                            attrs.insert(at.attr.to_string());
                        }
                    }
                }
            }
        }
    }
    attrs
}

/// Count positional reads of tuple-record dicts: `NAME[k][1]`, `a, b = NAME[k]`,
/// and `for k, (a, b) in NAME.items()` (aliases resolved through module-fn
/// call args).
///
/// Every expression reachable from *e*, INCLUDING comprehension internals
/// (elt/key/value/ifs/iter) — the reads hidden inside comprehensions.
///
/// The ONE expression traversal — every child, including comprehension
/// internals and call-func position (flagged so call-func receivers can be
/// excluded). all_exprs / the receiver-read counter / expr_writes all
/// descend through this; a new Expr variant is handled here once.
pub(super) fn walk_expr_deep<'a>(e: &'a Expr, in_call_func: bool, f: &mut impl FnMut(&'a Expr, bool)) {
    f(e, in_call_func);
    match e {
        Expr::Call(c) => {
            walk_expr_deep(&c.func, true, f);
            for a in &c.arguments.args {
                walk_expr_deep(a, false, f);
            }
            for k in &c.arguments.keywords {
                walk_expr_deep(&k.value, false, f);
            }
        }
        Expr::Attribute(a) => walk_expr_deep(&a.value, false, f),
        Expr::Subscript(s) => {
            walk_expr_deep(&s.value, false, f);
            walk_expr_deep(&s.slice, false, f);
        }
        Expr::BinOp(b) => {
            walk_expr_deep(&b.left, false, f);
            walk_expr_deep(&b.right, false, f);
        }
        Expr::BoolOp(b) => {
            for v in &b.values {
                walk_expr_deep(v, false, f);
            }
        }
        Expr::Compare(c) => {
            walk_expr_deep(&c.left, false, f);
            for o in &c.comparators {
                walk_expr_deep(o, false, f);
            }
        }
        Expr::UnaryOp(u) => walk_expr_deep(&u.operand, false, f),
        Expr::Lambda(l) => walk_expr_deep(&l.body, false, f),
        Expr::If(i) => {
            walk_expr_deep(&i.test, false, f);
            walk_expr_deep(&i.body, false, f);
            walk_expr_deep(&i.orelse, false, f);
        }
        Expr::Named(n) => walk_expr_deep(&n.value, false, f),
        Expr::Await(a) => walk_expr_deep(&a.value, false, f),
        Expr::Starred(st) => walk_expr_deep(&st.value, false, f),
        Expr::Tuple(t) => {
            for el in &t.elts {
                walk_expr_deep(el, false, f);
            }
        }
        Expr::List(l) => {
            for el in &l.elts {
                walk_expr_deep(el, false, f);
            }
        }
        Expr::Set(st) => {
            for el in &st.elts {
                walk_expr_deep(el, false, f);
            }
        }
        Expr::ListComp(l) => {
            walk_expr_deep(&l.elt, false, f);
            for g in &l.generators {
                walk_expr_deep(&g.iter, false, f);
                for c in &g.ifs {
                    walk_expr_deep(c, false, f);
                }
            }
        }
        Expr::SetComp(sc) => {
            walk_expr_deep(&sc.elt, false, f);
            for g in &sc.generators {
                walk_expr_deep(&g.iter, false, f);
                for c in &g.ifs {
                    walk_expr_deep(c, false, f);
                }
            }
        }
        Expr::DictComp(d) => {
            if let Some(k) = &d.key {
                walk_expr_deep(k, false, f);
            }
            walk_expr_deep(&d.value, false, f);
            for g in &d.generators {
                walk_expr_deep(&g.iter, false, f);
                for c in &g.ifs {
                    walk_expr_deep(c, false, f);
                }
            }
        }
        Expr::Generator(g) => {
            walk_expr_deep(&g.elt, false, f);
            for gg in &g.generators {
                walk_expr_deep(&gg.iter, false, f);
                for c in &gg.ifs {
                    walk_expr_deep(c, false, f);
                }
            }
        }
        _ => {}
    }
}

pub(super) fn all_exprs<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
    walk_expr_deep(e, false, &mut |x, _| out.push(x));
}

/// One parameter's grouping identity: its name and annotation key.
pub(super) type ParamKey = (String, String);
/// The unordered parameter pair; each annotation travels with its name.
pub(super) type PairKey = (ParamKey, ParamKey);

/// A class's field names (class-level annotated declarations + self.X
/// assignments in __init__), with the annotation type where present.
pub(super) fn class_field_names(c: &StmtClassDef) -> Vec<(String, Option<String>)> {
    let mut fields: Vec<(String, Option<String>)> = Vec::new();
    for m in &c.body {
        match m {
            Stmt::AnnAssign(a) => {
                if let Expr::Name(n) = a.target.as_ref() {
                    let ty = annotation_base_name(a.annotation.as_ref()).map(|s| s.to_string());
                    fields.push((n.id.to_string(), ty));
                }
            }
            Stmt::FunctionDef(f) => {
                if f.name.as_str() != "__init__" {
                    continue;
                }
                for st in &f.body {
                    if let Stmt::AnnAssign(a) = st {
                        if let Expr::Attribute(at) = a.target.as_ref() {
                            if matches!(at.value.as_ref(), Expr::Name(n) if n.id.as_str() == "self") {
                                let ty = annotation_base_name(a.annotation.as_ref()).map(|s| s.to_string());
                                fields.push((at.attr.to_string(), ty));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    fields
}

pub fn inner_function_count(fn_stmt: &Stmt) -> u32 {
    let mut count = 0;
    let mut stack: Vec<&Stmt> = Vec::new();
    if let Stmt::FunctionDef(f) = fn_stmt {
        for s in &f.body {
            stack.push(s);
        }
    }
    while let Some(s) = stack.pop() {
        if let Stmt::FunctionDef(f) = s {
            count += 1;
            // descend into nested function bodies too — Python's ast.walk
            // counts functions at any depth
            for b in &f.body {
                stack.push(b);
            }
        }
        push_stmt_children(s, &mut stack);
        for e in stmt_exprs(s) {
            count_expr_lambdas(e, &mut count);
        }
    }
    count
}

fn count_expr_lambdas(e: &Expr, count: &mut u32) {
    // stmt_exprs already flattens the full expression tree (push_expr_tree
    // descends calls, bins, comps) — the lambda is present exactly once.
    // Any descent here would double-count.
    if matches!(e, Expr::Lambda(_)) {
        *count += 1;
    }
}

// ------------------------------------------------------------- #27 husks

/// Names that surface through the function's returns: every name that
/// appears ANYWHERE in a return expression tree (nested functions
/// excluded) — a bare `return name`, the values of a returned dict literal,
/// tuple elements, f-string fields, call arguments. A mutated name that
/// surfaces in any part of the returned value surfaces the error (H4: the
/// server.py:704 shape — the except mutates db and the returned dict
/// carries it, so the value surfaces although `db` is not itself returned).
pub fn returned_names(body: &[Stmt]) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut stack: Vec<&Stmt> = body.iter().collect();
    while let Some(s) = stack.pop() {
        if let Stmt::FunctionDef(_) = s {
            continue; // nested function bodies excluded
        }
        if let Stmt::Return(r) = s {
            if let Some(v) = &r.value {
                collect_expr_names(v.as_ref(), &mut out);
            }
        }
        push_stmt_children(s, &mut stack);
    }
    out
}

/// Every `Name` in a return expression tree — the returned value's full
/// shape (dict literal values and keys, tuple elements, f-string fields,
/// call arguments), not just a bare name. Uses ruff's source_order walker,
/// which descends all expression children.
fn collect_expr_names(e: &Expr, out: &mut HashSet<String>) {
    use ruff_python_ast::visitor::source_order::{walk_expr, SourceOrderVisitor};
    struct NameCollector<'a>(&'a mut HashSet<String>);
    impl<'a> SourceOrderVisitor<'a> for NameCollector<'a> {
        fn visit_expr(&mut self, e: &'a Expr) {
            if let Expr::Name(n) = e {
                self.0.insert(n.id.to_string());
            }
            walk_expr(self, e);
        }
    }
    let mut c = NameCollector(out);
    c.visit_expr(e); // visit the ROOT too — walk_expr only descends children
}

fn sub_name(s: &ruff_python_ast::ExprSubscript) -> String {
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

/// Collect `set_*` methods (and property setters) for the repo-wide
/// unused-setter pass — runs per file; reference counting needs the whole
/// repo (prod vs test split) to distinguish dead from test-only.
pub fn collect_setters(state: &mut ScanState, body: &[Stmt]) {
    fn walk(state: &mut ScanState, stmts: &[Stmt], source: &str) {
        for s in stmts {
            match s {
                Stmt::FunctionDef(f) => {
                    let is_setter = f.name.as_str().starts_with("set_")
                        || f.decorator_list
                            .iter()
                            .any(|d| matches!(&d.expression, Expr::Attribute(a) if a.attr.as_str() == "setter"));
                    if is_setter {
                        state
                            .setters
                            .push((f.name.to_string(), line_of(source, f.name.range().start())));
                    }
                }
                Stmt::ClassDef(cd) => walk(state, &cd.body, source),
                _ => {}
            }
        }
    }
    walk(state, body, state.source);
}

/// Remove Setting Method: a `set_*` method (or a property setter) referenced
/// nowhere in production is dead — deletable, not just unused. Runs
/// repo-wide so prod refs (same-file AND cross-file) are complete.
///
/// Test-only references do NOT make it live: a test seam for code nothing
/// ships calls is not a seam, it is dead code wearing a harness. The message
/// names the imminent-caller escape so an agent mid-refactor is not wrongly
/// told to delete.
pub fn unused_setter_findings(
    setters: &[(String, String, usize)],
    prod_refs: &HashSet<String>,
    test_refs: &HashSet<String>,
) -> Vec<Finding> {
    let mut out = Vec::new();
    for (rel, name, line) in setters {
        if prod_refs.contains(name) {
            continue; // referenced by production — live
        }
        let test_only = test_refs.contains(name);
        let message = if test_only {
            format!(
                "setter '{name}' ({rel}:{line}) is referenced only from tests, never from production — a test seam for dead code is not a seam: nothing shipped calls it, so it is dead. If the production caller is imminent, write the calling code — this finding then clears. Public API entry points are exempt"
            )
        } else {
            format!("setter '{name}' ({rel}:{line}) is never referenced — Remove Setting Method: delete it")
        };
        out.push(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            file: rel.to_string(),
            line: *line,
            col: 0,
            function: name.to_string(),
            kind: "unused-setter".into(),
            severity: "warn".into(),
            message,
        });
    }
    out
}

// =====================================================================
// repo-wide families: duplicate (Dice on structural skeletons) + unused
// (defined-but-never-referenced). These are computed in the Rust runner
// across ALL files of one invocation — mirroring _duplicate_actions and
// _unused_actions in lucidlint.py.
// =====================================================================

pub use crate::forwarding::ForwarderFn;
pub use crate::forwarding::ForwarderMethod;
pub use crate::forwarding::RepoMethod;
pub use crate::skeleton::SkeletonFn;
pub use crate::skeleton::SkeletonModule;

/// enumeration replicates `ast.iter_child_nodes` field order — the
/// Parameters node emits its defaults hoisted (kw_defaults then defaults),
/// dict literals emit all keys before all values, and if/elif chains
/// re-expand into nested "If" nodes, exactly like CPython's orelse nesting.
/// A BFS queue item — either an AST node or a bare operator/context token
/// (CPython visits `ast.operator` / `ast.expr_context` nodes in `ast.walk`).
pub enum Q<'a> {
    N(AnyNodeRef<'a>),
    T(&'a str),
}

/// The structured fix kind of a RAW finding message: the first token after
/// the `— fix: ` directive tail. None when the message carries no directive
/// (fix-less findings never construct a Fix — Sixth-pass BQ1).
pub fn fix_kind_of_message(message: &str) -> Option<&str> {
    let pos = message.rfind("— fix: ")?;
    let tail = &message[pos + "— fix: ".len()..];
    let kind = tail.split_whitespace().next()?;
    if kind == "lucidlint" {
        return None; // the rendered full-command template — not a directive
    }
    Some(kind)
}

/// The rule's default fix kind (the catalog's Rule.fix via KIND_FIX) — the
/// structured fix_kind for findings whose message carries no shape-routed
/// directive (large-function -> extract-method, partition etc.).
pub fn kind_fix_default(kind: &str) -> &'static str {
    for (k, fix) in crate::rules_gen::KIND_FIX {
        if *k == kind {
            return fix;
        }
    }
    ""
}

pub fn fn_skeleton(f: &StmtFunctionDef) -> Vec<String> {
    use ruff_python_ast::AnyNodeRef;
    let mut toks: Vec<String> = Vec::new();
    let mut queue: Vec<Q> = Vec::new();
    toks.push(if f.is_async { "AsyncFunctionDef" } else { "FunctionDef" }.to_string());
    // CPython FunctionDef children: arguments, body, decorator_list, returns
    queue.push(Q::N(AnyNodeRef::Parameters(&f.parameters)));
    for s in &f.body {
        queue.push(Q::N(AnyNodeRef::from(s)));
    }
    for d in &f.decorator_list {
        queue.push(Q::N(AnyNodeRef::from(&d.expression)));
    }
    if let Some(r) = &f.returns {
        queue.push(Q::N(AnyNodeRef::from(r.as_ref())));
    }
    let mut i = 0usize;
    while i < queue.len() {
        let node = match &queue[i] {
            Q::N(n) => *n,
            Q::T(t) => {
                toks.push((*t).to_string());
                i += 1;
                continue;
            }
        };
        i += 1;
        match node {
            AnyNodeRef::ExprName(_) => toks.push("N".to_string()),
            AnyNodeRef::ExprStringLiteral(_)
            | AnyNodeRef::ExprBytesLiteral(_)
            | AnyNodeRef::ExprNumberLiteral(_)
            | AnyNodeRef::ExprBooleanLiteral(_)
            | AnyNodeRef::ExprNoneLiteral(_)
            | AnyNodeRef::ExprEllipsisLiteral(_)
            | AnyNodeRef::InterpolatedStringLiteralElement(_) => toks.push("C".to_string()),
            AnyNodeRef::Parameter(_) | AnyNodeRef::ParameterWithDefault(_) => toks.push("A".to_string()),
            AnyNodeRef::Parameters(_) => toks.push("arguments".to_string()),
            AnyNodeRef::ElifElseClause(_) => toks.push("If".to_string()),
            AnyNodeRef::InterpolatedElement(_) => toks.push("FormattedValue".to_string()),
            _ => toks.push(format!("{:?}", node.kind())),
        }
        skel_children(node, &mut queue);
    }
    toks
}

fn op_token(op: &Operator) -> &'static str {
    use ruff_python_ast::Operator as O;
    match op {
        O::Add => "Add",
        O::Sub => "Sub",
        O::Mult => "Mult",
        O::MatMult => "MatMult",
        O::Div => "Div",
        O::Mod => "Mod",
        O::Pow => "Pow",
        O::LShift => "LShift",
        O::RShift => "RShift",
        O::BitOr => "BitOr",
        O::BitXor => "BitXor",
        O::BitAnd => "BitAnd",
        O::FloorDiv => "FloorDiv",
    }
}

fn unary_op_token(op: &UnaryOp) -> &'static str {
    use ruff_python_ast::UnaryOp as U;
    match op {
        U::Invert => "Invert",
        U::Not => "Not",
        U::UAdd => "UAdd",
        U::USub => "USub",
    }
}

fn bool_op_token(op: &BoolOp) -> &'static str {
    use ruff_python_ast::BoolOp as B;
    match op {
        B::And => "And",
        B::Or => "Or",
    }
}

fn cmp_op_token(op: &CmpOp) -> &'static str {
    use ruff_python_ast::CmpOp as C;
    match op {
        C::Eq => "Eq",
        C::NotEq => "NotEq",
        C::Lt => "Lt",
        C::LtE => "LtE",
        C::Gt => "Gt",
        C::GtE => "GtE",
        C::Is => "Is",
        C::IsNot => "IsNot",
        C::In => "In",
        C::NotIn => "NotIn",
    }
}

fn ctx_token(ctx: &ExprContext) -> &'static str {
    use ruff_python_ast::ExprContext as X;
    match ctx {
        X::Load => "Load",
        X::Store => "Store",
        X::Del => "Del",
        X::Invalid => "Invalid",
    }
}

/// Push a pattern node onto the skeleton queue.
fn push_pattern<'a>(queue: &mut Vec<Q<'a>>, p: &'a Pattern) {
    let n = match p {
        Pattern::MatchValue(x) => AnyNodeRef::PatternMatchValue(x),
        Pattern::MatchSingleton(x) => AnyNodeRef::PatternMatchSingleton(x),
        Pattern::MatchSequence(x) => AnyNodeRef::PatternMatchSequence(x),
        Pattern::MatchMapping(x) => AnyNodeRef::PatternMatchMapping(x),
        Pattern::MatchClass(x) => AnyNodeRef::PatternMatchClass(x),
        Pattern::MatchStar(x) => AnyNodeRef::PatternMatchStar(x),
        Pattern::MatchAs(x) => AnyNodeRef::PatternMatchAs(x),
        Pattern::MatchOr(x) => AnyNodeRef::PatternMatchOr(x),
    };
    queue.push(Q::N(n));
}

/// One BFS queue item — the walker's two push helpers (module-level: a
/// nested pair would hide structure the latent-class rule exists to surface).
fn skel_push<'a>(q: &mut Vec<Q<'a>>, n: AnyNodeRef<'a>) {
    q.push(Q::N(n));
}

fn skel_tok<'a>(q: &mut Vec<Q<'a>>, t: &'static str) {
    q.push(Q::T(t));
}

pub fn skel_children<'a>(node: AnyNodeRef<'a>, queue: &mut Vec<Q<'a>>) {
    use ruff_python_ast::Pattern;
    match node {
        AnyNodeRef::StmtClassDef(c) => {
            // CPython: bases, keywords, body, decorator_list
            if let Some(arguments) = &c.arguments {
                for b in &arguments.args {
                    skel_push(queue, AnyNodeRef::from(b));
                }
                for k in &arguments.keywords {
                    skel_push(queue, AnyNodeRef::Keyword(k));
                }
            }
            for s in &c.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
            for d in &c.decorator_list {
                skel_push(queue, AnyNodeRef::from(&d.expression));
            }
        }
        AnyNodeRef::StmtReturn(r) => {
            if let Some(v) = &r.value {
                skel_push(queue, AnyNodeRef::from(v.as_ref()));
            }
        }
        AnyNodeRef::StmtDelete(d) => {
            for t in &d.targets {
                skel_push(queue, AnyNodeRef::from(t));
            }
        }
        AnyNodeRef::StmtTypeAlias(t) => {
            skel_push(queue, AnyNodeRef::from(t.name.as_ref()));
            if let Some(tp) = &t.type_params {
                for p in &tp.type_params {
                    match p {
                        ruff_python_ast::TypeParam::TypeVar(t) => skel_push(queue, AnyNodeRef::TypeParamTypeVar(t)),
                        ruff_python_ast::TypeParam::TypeVarTuple(t) => skel_push(queue, AnyNodeRef::TypeParamTypeVarTuple(t)),
                        ruff_python_ast::TypeParam::ParamSpec(t) => skel_push(queue, AnyNodeRef::TypeParamParamSpec(t)),
                    }
                }
            }
            skel_push(queue, AnyNodeRef::from(t.value.as_ref()));
        }
        AnyNodeRef::StmtAssign(a) => {
            for t in &a.targets {
                skel_push(queue, AnyNodeRef::from(t));
            }
            skel_push(queue, AnyNodeRef::from(a.value.as_ref()));
        }
        AnyNodeRef::StmtAugAssign(a) => {
            skel_push(queue, AnyNodeRef::from(a.target.as_ref()));
            skel_tok(queue, op_token(&a.op));
            skel_push(queue, AnyNodeRef::from(a.value.as_ref()));
        }
        AnyNodeRef::StmtAnnAssign(a) => {
            skel_push(queue, AnyNodeRef::from(a.target.as_ref()));
            skel_push(queue, AnyNodeRef::from(a.annotation.as_ref()));
            if let Some(v) = &a.value {
                skel_push(queue, AnyNodeRef::from(v.as_ref()));
            }
        }
        AnyNodeRef::StmtFor(f) => {
            skel_push(queue, AnyNodeRef::from(f.target.as_ref()));
            skel_push(queue, AnyNodeRef::from(f.iter.as_ref()));
            for s in &f.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
            for s in &f.orelse {
                skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::StmtWhile(w) => {
            skel_push(queue, AnyNodeRef::from(w.test.as_ref()));
            for s in &w.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
            for s in &w.orelse {
                skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::StmtIf(i) => {
            skel_push(queue, AnyNodeRef::from(i.test.as_ref()));
            for s in &i.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
            // CPython re-nests elifs as inner If nodes in orelse
            for clause in &i.elif_else_clauses {
                skel_push(queue, AnyNodeRef::ElifElseClause(clause));
            }
        }
        AnyNodeRef::ElifElseClause(clause) => {
            if let Some(t) = &clause.test {
                skel_push(queue, AnyNodeRef::from(t));
            }
            for s in &clause.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::StmtWith(w) => {
            for item in &w.items {
                skel_push(queue, AnyNodeRef::WithItem(item));
            }
            for s in &w.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::WithItem(item) => {
            skel_push(queue, AnyNodeRef::from(&item.context_expr));
            if let Some(v) = &item.optional_vars {
                skel_push(queue, AnyNodeRef::from(v.as_ref()));
            }
        }
        AnyNodeRef::StmtMatch(m) => {
            skel_push(queue, AnyNodeRef::from(m.subject.as_ref()));
            for case in &m.cases {
                skel_push(queue, AnyNodeRef::MatchCase(case));
            }
        }
        AnyNodeRef::MatchCase(case) => {
            match &case.pattern {
                Pattern::MatchValue(p) => skel_push(queue, AnyNodeRef::PatternMatchValue(p)),
                Pattern::MatchSingleton(p) => skel_push(queue, AnyNodeRef::PatternMatchSingleton(p)),
                Pattern::MatchSequence(p) => skel_push(queue, AnyNodeRef::PatternMatchSequence(p)),
                Pattern::MatchMapping(p) => skel_push(queue, AnyNodeRef::PatternMatchMapping(p)),
                Pattern::MatchClass(p) => skel_push(queue, AnyNodeRef::PatternMatchClass(p)),
                Pattern::MatchStar(p) => skel_push(queue, AnyNodeRef::PatternMatchStar(p)),
                Pattern::MatchAs(p) => skel_push(queue, AnyNodeRef::PatternMatchAs(p)),
                Pattern::MatchOr(p) => skel_push(queue, AnyNodeRef::PatternMatchOr(p)),
            }
            if let Some(g) = &case.guard {
                skel_push(queue, AnyNodeRef::from(g.as_ref()));
            }
            for s in &case.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::StmtRaise(r) => {
            if let Some(e) = &r.exc {
                skel_push(queue, AnyNodeRef::from(e.as_ref()));
            }
            if let Some(c) = &r.cause {
                skel_push(queue, AnyNodeRef::from(c.as_ref()));
            }
        }
        AnyNodeRef::StmtTry(t) => {
            for s in &t.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
            for h in &t.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                skel_push(queue, AnyNodeRef::ExceptHandlerExceptHandler(eh));
            }
            for s in &t.orelse {
                skel_push(queue, AnyNodeRef::from(s));
            }
            for s in &t.finalbody {
                skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::ExceptHandlerExceptHandler(eh) => {
            if let Some(t) = &eh.type_ {
                skel_push(queue, AnyNodeRef::from(t.as_ref()));
            }
            for s in &eh.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
        }
        AnyNodeRef::StmtAssert(a) => {
            skel_push(queue, AnyNodeRef::from(a.test.as_ref()));
            if let Some(m) = &a.msg {
                skel_push(queue, AnyNodeRef::from(m.as_ref()));
            }
        }
        AnyNodeRef::StmtImport(imp) => {
            for a in &imp.names {
                skel_push(queue, AnyNodeRef::Alias(a));
            }
        }
        AnyNodeRef::StmtImportFrom(imp) => {
            for a in &imp.names {
                skel_push(queue, AnyNodeRef::Alias(a));
            }
        }
        AnyNodeRef::StmtExpr(e) => skel_push(queue, AnyNodeRef::from(e.value.as_ref())),
        // a nested function's subtree IS part of ast.walk — descend like the
        // module-level root: arguments, body, decorator_list, returns
        AnyNodeRef::StmtFunctionDef(f) => {
            skel_push(queue, AnyNodeRef::Parameters(&f.parameters));
            for s in &f.body {
                skel_push(queue, AnyNodeRef::from(s));
            }
            for d in &f.decorator_list {
                skel_push(queue, AnyNodeRef::from(&d.expression));
            }
            if let Some(r) = &f.returns {
                skel_push(queue, AnyNodeRef::from(r.as_ref()));
            }
        }
        // leaf / identifier-only statements
        AnyNodeRef::StmtGlobal(_)
        | AnyNodeRef::StmtNonlocal(_)
        | AnyNodeRef::StmtPass(_)
        | AnyNodeRef::StmtBreak(_)
        | AnyNodeRef::StmtContinue(_)
        | AnyNodeRef::StmtIpyEscapeCommand(_)
        | AnyNodeRef::Alias(_) => {}
        AnyNodeRef::Parameters(p) => {
            // CPython arguments children: posonlyargs, args, vararg,
            // kwonlyargs, kw_defaults, kwarg, defaults (hoisted)
            for pwd in &p.posonlyargs {
                skel_push(queue, AnyNodeRef::ParameterWithDefault(pwd));
            }
            for pwd in &p.args {
                skel_push(queue, AnyNodeRef::ParameterWithDefault(pwd));
            }
            if let Some(v) = &p.vararg {
                skel_push(queue, AnyNodeRef::Parameter(v));
            }
            for pwd in &p.kwonlyargs {
                skel_push(queue, AnyNodeRef::ParameterWithDefault(pwd));
            }
            for pwd in p.kwonlyargs.iter().filter_map(|p| p.default.as_deref()) {
                skel_push(queue, AnyNodeRef::from(pwd));
            }
            if let Some(k) = &p.kwarg {
                skel_push(queue, AnyNodeRef::Parameter(k));
            }
            for pwd in p
                .posonlyargs
                .iter()
                .chain(&p.args)
                .filter_map(|p| p.default.as_deref())
            {
                skel_push(queue, AnyNodeRef::from(pwd));
            }
        }
        AnyNodeRef::Parameter(param) => {
            if let Some(a) = &param.annotation {
                skel_push(queue, AnyNodeRef::from(a.as_ref()));
            }
        }
        AnyNodeRef::ParameterWithDefault(pwd) => {
            // the default is hoisted by Parameters (CPython defaults list)
            if let Some(a) = &pwd.parameter.annotation {
                skel_push(queue, AnyNodeRef::from(a.as_ref()));
            }
        }
        AnyNodeRef::Keyword(k) => skel_push(queue, AnyNodeRef::from(&k.value)),
        AnyNodeRef::Comprehension(c) => {
            skel_push(queue, AnyNodeRef::from(&c.target));
            skel_push(queue, AnyNodeRef::from(&c.iter));
            for f in &c.ifs {
                skel_push(queue, AnyNodeRef::from(f));
            }
        }
        AnyNodeRef::ExprBoolOp(b) => {
            skel_tok(queue, bool_op_token(&b.op));
            for v in &b.values {
                skel_push(queue, AnyNodeRef::from(v));
            }
        }
        AnyNodeRef::ExprNamed(n) => {
            skel_push(queue, AnyNodeRef::from(n.target.as_ref()));
            skel_push(queue, AnyNodeRef::from(n.value.as_ref()));
        }
        AnyNodeRef::ExprBinOp(b) => {
            skel_push(queue, AnyNodeRef::from(b.left.as_ref()));
            skel_tok(queue, op_token(&b.op));
            skel_push(queue, AnyNodeRef::from(b.right.as_ref()));
        }
        AnyNodeRef::ExprUnaryOp(u) => {
            skel_tok(queue, unary_op_token(&u.op));
            skel_push(queue, AnyNodeRef::from(u.operand.as_ref()));
        }
        AnyNodeRef::ExprLambda(l) => {
            if let Some(p) = &l.parameters {
                skel_push(queue, AnyNodeRef::Parameters(p));
            }
            skel_push(queue, AnyNodeRef::from(l.body.as_ref()));
        }
        AnyNodeRef::ExprIf(e) => {
            skel_push(queue, AnyNodeRef::from(e.test.as_ref()));
            skel_push(queue, AnyNodeRef::from(e.body.as_ref()));
            skel_push(queue, AnyNodeRef::from(e.orelse.as_ref()));
        }
        AnyNodeRef::ExprDict(d) => {
            // CPython: all keys first, then all values
            for item in &d.items {
                if let Some(k) = &item.key {
                    skel_push(queue, AnyNodeRef::from(k));
                }
            }
            for item in &d.items {
                skel_push(queue, AnyNodeRef::from(&item.value));
            }
        }
        AnyNodeRef::ExprSet(s) => {
            for e in &s.elts {
                skel_push(queue, AnyNodeRef::from(e));
            }
        }
        AnyNodeRef::ExprListComp(c) => {
            skel_push(queue, AnyNodeRef::from(c.elt.as_ref()));
            for g in &c.generators {
                skel_push(queue, AnyNodeRef::Comprehension(g));
            }
        }
        AnyNodeRef::ExprSetComp(c) => {
            skel_push(queue, AnyNodeRef::from(c.elt.as_ref()));
            for g in &c.generators {
                skel_push(queue, AnyNodeRef::Comprehension(g));
            }
        }
        AnyNodeRef::ExprGenerator(g) => {
            skel_push(queue, AnyNodeRef::from(g.elt.as_ref()));
            for gen in &g.generators {
                skel_push(queue, AnyNodeRef::Comprehension(gen));
            }
        }
        AnyNodeRef::ExprDictComp(c) => {
            if let Some(k) = &c.key {
                skel_push(queue, AnyNodeRef::from(k.as_ref()));
            }
            skel_push(queue, AnyNodeRef::from(c.value.as_ref()));
            for g in &c.generators {
                skel_push(queue, AnyNodeRef::Comprehension(g));
            }
        }
        AnyNodeRef::ExprAwait(a) => skel_push(queue, AnyNodeRef::from(a.value.as_ref())),
        AnyNodeRef::ExprYield(y) => {
            if let Some(v) = &y.value {
                skel_push(queue, AnyNodeRef::from(v.as_ref()));
            }
        }
        AnyNodeRef::ExprYieldFrom(y) => skel_push(queue, AnyNodeRef::from(y.value.as_ref())),
        AnyNodeRef::ExprCompare(c) => {
            skel_push(queue, AnyNodeRef::from(c.left.as_ref()));
            for o in &c.ops {
                skel_tok(queue, cmp_op_token(o));
            }
            for o in &c.comparators {
                skel_push(queue, AnyNodeRef::from(o));
            }
        }
        AnyNodeRef::ExprCall(c) => {
            skel_push(queue, AnyNodeRef::from(c.func.as_ref()));
            for a in &c.arguments.args {
                skel_push(queue, AnyNodeRef::from(a));
            }
            for k in &c.arguments.keywords {
                skel_push(queue, AnyNodeRef::Keyword(k));
            }
        }
        AnyNodeRef::ExprFString(f) => {
            for element in f.value.elements() {
                skel_push(queue, AnyNodeRef::from(element));
            }
        }
        AnyNodeRef::ExprTString(t) => {
            // t-strings (3.14) — structurally a JoinedStr; walk parts' elements
            for element in t.value.elements() {
                skel_push(queue, AnyNodeRef::from(element));
            }
        }
        AnyNodeRef::InterpolatedElement(e) => {
            skel_push(queue, AnyNodeRef::from(e.expression.as_ref()));
            if let Some(spec) = &e.format_spec {
                for element in &spec.elements {
                    skel_push(queue, AnyNodeRef::from(element));
                }
            }
        }
        AnyNodeRef::InterpolatedStringFormatSpec(_) | AnyNodeRef::InterpolatedStringLiteralElement(_) => {}
        AnyNodeRef::ExprAttribute(a) => {
            skel_push(queue, AnyNodeRef::from(a.value.as_ref()));
            skel_tok(queue, ctx_token(&a.ctx));
        }
        AnyNodeRef::ExprSubscript(s) => {
            skel_push(queue, AnyNodeRef::from(s.value.as_ref()));
            skel_push(queue, AnyNodeRef::from(s.slice.as_ref()));
            skel_tok(queue, ctx_token(&s.ctx));
        }
        AnyNodeRef::ExprStarred(s) => {
            skel_push(queue, AnyNodeRef::from(s.value.as_ref()));
            skel_tok(queue, ctx_token(&s.ctx));
        }
        AnyNodeRef::ExprList(l) => {
            for e in &l.elts {
                skel_push(queue, AnyNodeRef::from(e));
            }
            skel_tok(queue, ctx_token(&l.ctx));
        }
        AnyNodeRef::ExprTuple(t) => {
            for e in &t.elts {
                skel_push(queue, AnyNodeRef::from(e));
            }
            skel_tok(queue, ctx_token(&t.ctx));
        }
        AnyNodeRef::ExprSlice(s) => {
            if let Some(l) = &s.lower {
                skel_push(queue, AnyNodeRef::from(l.as_ref()));
            }
            if let Some(u) = &s.upper {
                skel_push(queue, AnyNodeRef::from(u.as_ref()));
            }
            if let Some(st) = &s.step {
                skel_push(queue, AnyNodeRef::from(st.as_ref()));
            }
        }
        AnyNodeRef::PatternMatchValue(p) => skel_push(queue, AnyNodeRef::from(p.value.as_ref())),
        AnyNodeRef::PatternMatchSequence(p) => {
            for pat in &p.patterns {
                push_pattern(queue, pat);
            }
        }
        AnyNodeRef::PatternMatchMapping(p) => {
            for k in &p.keys {
                skel_push(queue, AnyNodeRef::from(k));
            }
            for pat in &p.patterns {
                push_pattern(queue, pat);
            }
        }
        AnyNodeRef::PatternMatchClass(p) => {
            skel_push(queue, AnyNodeRef::from(p.cls.as_ref()));
            for pat in &p.arguments.patterns {
                push_pattern(queue, pat);
            }
            for kw in &p.arguments.keywords {
                push_pattern(queue, &kw.pattern);
            }
        }
        AnyNodeRef::PatternMatchAs(p) => {
            if let Some(pat) = &p.pattern {
                push_pattern(queue, pat);
            }
        }
        AnyNodeRef::PatternMatchOr(p) => {
            for pat in &p.patterns {
                push_pattern(queue, pat);
            }
        }
        AnyNodeRef::PatternMatchSingleton(_) | AnyNodeRef::PatternMatchStar(_) => {}
        AnyNodeRef::TypeParamTypeVar(t) => {
            if let Some(b) = &t.bound {
                skel_push(queue, AnyNodeRef::from(b.as_ref()));
            }
            if let Some(d) = &t.default {
                skel_push(queue, AnyNodeRef::from(d.as_ref()));
            }
        }
        AnyNodeRef::TypeParamTypeVarTuple(t) => {
            if let Some(d) = &t.default {
                skel_push(queue, AnyNodeRef::from(d.as_ref()));
            }
        }
        AnyNodeRef::TypeParamParamSpec(t) => {
            if let Some(d) = &t.default {
                skel_push(queue, AnyNodeRef::from(d.as_ref()));
            }
        }
        AnyNodeRef::PatternArguments(_) | AnyNodeRef::PatternKeyword(_) => {}
        AnyNodeRef::Arguments(a) => {
            for e in &a.args {
                skel_push(queue, AnyNodeRef::from(e));
            }
            for k in &a.keywords {
                skel_push(queue, AnyNodeRef::Keyword(k));
            }
        }
        // literals, names, module — leaves
        AnyNodeRef::ExprName(n) => skel_tok(queue, ctx_token(&n.ctx)),
        AnyNodeRef::ExprStringLiteral(_)
        | AnyNodeRef::ExprBytesLiteral(_)
        | AnyNodeRef::ExprNumberLiteral(_)
        | AnyNodeRef::ExprBooleanLiteral(_)
        | AnyNodeRef::ExprNoneLiteral(_)
        | AnyNodeRef::ExprEllipsisLiteral(_)
        | AnyNodeRef::ExprIpyEscapeCommand(_)
        | AnyNodeRef::ModModule(_)
        | AnyNodeRef::ModExpression(_)
        // never pushed by skel_children, but the match must be total
        | AnyNodeRef::Decorator(_)
        | AnyNodeRef::TypeParams(_)
        | AnyNodeRef::FString(_)
        | AnyNodeRef::TString(_)
        | AnyNodeRef::StringLiteral(_)
        | AnyNodeRef::BytesLiteral(_)
        | AnyNodeRef::Identifier(_) => {}
    }
}

/// `_is_duplicate_candidate`: at least two real statements (one-line
/// accessors are not copy-paste) and a 12+ token skeleton; `__init__`
/// boilerplate is conventional.
pub fn is_duplicate_candidate(f: &StmtFunctionDef, skeleton_len: usize) -> bool {
    if f.name.as_str() == "__init__" {
        return false;
    }
    let stmts: Vec<&Stmt> = f
        .body
        .iter()
        .filter(|s| !matches!(s, Stmt::Expr(e) if matches!(e.value.as_ref(), Expr::StringLiteral(_))))
        .collect();
    stmts.len() >= 2 && skeleton_len >= 12
}

// ------------------------------------------------------------- #26 duplicate-module

/// Vendored path components — never scanned for module forks (a fork inside
/// vendored code is upstream's problem). Independent of SCAN_SKIP_DIRS,
/// which has no vendor entries.
pub use crate::paths::is_vendored;

/// The normalized constant token: `NAME:norm(type)` — numeric literals
/// normalised, strings stripped, bools canonical (R4). None for non-literal
/// values (calls, names, collections).
fn constant_token(target: &str, value: &Expr) -> Option<String> {
    let ty = match value {
        Expr::NumberLiteral(_) => "num",
        Expr::StringLiteral(_) | Expr::BytesLiteral(_) => "str",
        Expr::BooleanLiteral(_) => "bool",
        Expr::NoneLiteral(_) => "none",
        Expr::UnaryOp(u) => return constant_token(target, u.operand.as_ref()),
        _ => return None,
    };
    Some(format!("{target}:{ty}"))
}

/// The `NAME = literal` constant tokens in a statement list, sorted+deduped.
fn constant_tokens(body: &[Stmt]) -> Vec<String> {
    let mut out = Vec::new();
    for s in body {
        let (targets, value): (Vec<&Expr>, Option<&Expr>) = match s {
            Stmt::Assign(a) => (a.targets.iter().collect(), Some(a.value.as_ref())),
            Stmt::AnnAssign(a) => (vec![a.target.as_ref()], a.value.as_deref()),
            _ => (Vec::new(), None),
        };
        let Some(v) = value else { continue };
        for t in targets {
            if let Expr::Name(n) = t {
                if n.id.as_str().starts_with("__") {
                    continue;
                }
                if let Some(tok) = constant_token(n.id.as_str(), v) {
                    out.push(tok);
                }
            }
        }
    }
    let mut uniq: Vec<String> = out.into_iter().collect();
    uniq.sort();
    uniq.dedup();
    uniq
}

/// The fields a module's own classes declare: class-body annotations
/// (`name: str`) plus the `self.<name> = ...` assignments in their methods —
/// the field set the sibling pass compares. A record's fields ARE its facts,
/// so two records whose fields differ are siblings by shape, not copies
/// (#73: two one-field records were reported as a fork because a field NAME
/// is an "N" token in the skeleton).
fn declared_record_fields(classes: &[&StmtClassDef]) -> Vec<String> {
    let mut fields: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for c in classes {
        for m in &c.body {
            if let Stmt::AnnAssign(a) = m {
                if let Expr::Name(n) = a.target.as_ref() {
                    fields.insert(n.id.to_string());
                }
            }
            if let Stmt::FunctionDef(f) = m {
                let mut assigned: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
                let mut types: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
                collect_field_assignments(&f.body, &std::collections::HashMap::new(), &mut assigned, &mut types);
                fields.extend(assigned);
            }
        }
    }
    fields.into_iter().collect()
}

/// One statement's structural tokens, headed by a member-boundary sentinel
/// so bigrams never cross statement boundaries.
fn stmt_skeleton<'a>(stmt: &'a Stmt, sentinel: &str, toks: &mut Vec<String>, queue: &mut Vec<Q<'a>>) {
    toks.push(sentinel.to_string());
    queue.push(Q::N(AnyNodeRef::from(stmt)));
    let mut i = 0usize;
    while i < queue.len() {
        let node = match &queue[i] {
            Q::N(n) => *n,
            Q::T(t) => {
                toks.push((*t).to_string());
                i += 1;
                continue;
            }
        };
        i += 1;
        match node {
            AnyNodeRef::ExprName(_) => toks.push("N".to_string()),
            AnyNodeRef::ExprStringLiteral(_)
            | AnyNodeRef::ExprBytesLiteral(_)
            | AnyNodeRef::ExprNumberLiteral(_)
            | AnyNodeRef::ExprBooleanLiteral(_)
            | AnyNodeRef::ExprNoneLiteral(_)
            | AnyNodeRef::ExprEllipsisLiteral(_)
            | AnyNodeRef::InterpolatedStringLiteralElement(_) => toks.push("C".to_string()),
            AnyNodeRef::Parameter(_) | AnyNodeRef::ParameterWithDefault(_) => toks.push("A".to_string()),
            AnyNodeRef::Parameters(_) => toks.push("arguments".to_string()),
            AnyNodeRef::ElifElseClause(_) => toks.push("If".to_string()),
            AnyNodeRef::InterpolatedElement(_) => toks.push("FormattedValue".to_string()),
            _ => toks.push(format!("{:?}", node.kind())),
        }
        skel_children(node, queue);
    }
}

/// The structural tokens of a statement list with member boundaries (sentinels).
fn body_skeleton(body: &[Stmt], toks: &mut Vec<String>) {
    for s in body {
        if matches!(s, Stmt::Import(_) | Stmt::ImportFrom(_)) {
            continue;
        }
        // FRESH queue per statement: a shared queue would restart each statement's
        // walk at index 0 and re-walk every earlier statement's subtree — O(n^2)
        // on big modules, OOM on a 2k-line file
        let mut queue: Vec<Q> = Vec::new();
        stmt_skeleton(s, ";", toks, &mut queue);
    }
}

/// Collect a file's duplicate-module identities (#26): the module itself,
/// each top-level class, and the combined module-class identity when a
/// module's non-import top-levels are exactly one class def.
/// A class identity's direct base names ("Name:Base" / "Attr:pkg:Base"),
/// sorted+deduped — the C2 shared-base pairing signal.
fn class_bases(c: &StmtClassDef) -> Vec<String> {
    let mut bases: Vec<String> = Vec::new();
    if let Some(arguments) = &c.arguments {
        for b in &arguments.args {
            match b {
                Expr::Name(n) => bases.push(format!("Name:{}", n.id.as_str())),
                Expr::Attribute(a) => {
                    if let Expr::Name(v) = a.value.as_ref() {
                        bases.push(format!("Attr:{}:{}", v.id.as_str(), a.attr.as_str()));
                    }
                }
                _ => {}
            }
        }
    }
    bases.sort();
    bases.dedup();
    bases
}

pub fn collect_skeleton_modules(state: &mut ScanState, body: &[Stmt], rel: &str) {
    if is_skipped_module(state, rel) {
        return;
    }
    if body.len() < 3 {
        return; // the file >=3-statement floor
    }
    let classes: Vec<&StmtClassDef> = body
        .iter()
        .filter_map(|s| match s {
            Stmt::ClassDef(c) => Some(c),
            _ => None,
        })
        .collect();
    let module_consts = constant_tokens(body);
    let mut module_skel = Vec::new();
    body_skeleton(body, &mut module_skel);
    let mut module_members: Vec<String> = body
        .iter()
        .filter_map(|s| match s {
            Stmt::FunctionDef(f) => Some(f.name.to_string()),
            Stmt::ClassDef(c) => Some(c.name.to_string()),
            _ => None,
        })
        .collect();
    module_members.sort();
    let non_import = body
        .iter()
        .filter(|s| !matches!(s, Stmt::Import(_) | Stmt::ImportFrom(_)))
        .count();
    let module_qualifies = non_import >= 2 && module_skel.len() >= 12;
    if module_qualifies {
        state.skeleton_modules.push(SkeletonModule {
            rel: rel.to_string(),
            name: rel
                .rsplit('/')
                .next()
                .unwrap_or(rel)
                .trim_end_matches(".py")
                .to_string(),
            line: 1,
            skeleton: module_skel.clone(),
            consts: module_consts.clone(),
            members: module_members.clone(),
            fields: declared_record_fields(&classes),
            entity: "module",
            bases: Vec::new(),
        });
    }
    for c in &classes {
        let mut skel = Vec::new();
        body_skeleton(&c.body, &mut skel);
        let consts = constant_tokens(&c.body);
        let mut members: Vec<String> = class_methods(c).iter().map(|f| f.name.to_string()).collect();
        members.sort();
        if skel.len() < 12 {
            continue;
        }
        let line = line_of(state.source, c.name.range().start());
        state.skeleton_modules.push(SkeletonModule {
            rel: rel.to_string(),
            name: c.name.to_string(),
            line,
            skeleton: skel.clone(),
            consts: consts.clone(),
            members: members.clone(),
            fields: declared_record_fields(&[c]),
            entity: "class",
            bases: class_bases(c),
        });
        // module-class identity: exactly one class; everything else at the
        // top level is imports or constants
        let other_statements = body
            .iter()
            .filter(|s| {
                !matches!(s, Stmt::Import(_) | Stmt::ImportFrom(_) | Stmt::ClassDef(_))
                    && !matches!(s, Stmt::Assign(_) | Stmt::AnnAssign(_))
            })
            .count()
            == 0;
        if is_module_class(classes.len(), other_statements, module_qualifies) {
            let mut combined = skel.clone();
            combined.extend(module_consts.iter().cloned());
            let mut all = module_members.clone();
            all.extend(members.iter().cloned());
            all.sort();
            all.dedup();
            state.skeleton_modules.push(SkeletonModule {
                rel: rel.to_string(),
                name: c.name.to_string(),
                line,
                skeleton: combined,
                consts: module_consts.clone(),
                members: all,
                fields: declared_record_fields(&[c]),
                entity: "module-class",
                bases: Vec::new(),
            });
        }
    }
}

/// Cross-file module/class forks (#26): two identities with the same
/// structural skeleton (>= 0.9 Dice) AND identical constants — a fix lands
/// in one copy while the other silently keeps the old behavior. Pairing
/// identities: module-module, class-class, and module-class (a module whose
/// non-import top-levels are exactly one class def). The message names the
/// INTERSECTING members (matched members:) — the bigram Dice cannot produce
/// the list, so the member sets are tracked alongside.
pub fn duplicate_module_findings(entities: &[SkeletonModule]) -> Vec<Finding> {
    use std::collections::HashMap;
    let mut out = Vec::new();
    if entities.len() < 2 {
        return out;
    }
    let interner = BigramInterner::new(&entities.iter().map(|e| e.skeleton.as_slice()).collect::<Vec<_>>());
    let mut buckets: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, e) in entities.iter().enumerate() {
        buckets.entry(e.skeleton.len()).or_default().push(i);
    }
    let mut len_keys: Vec<usize> = buckets.keys().copied().collect();
    len_keys.sort_unstable();
    let compatible = |a: &SkeletonModule, b: &SkeletonModule| {
        a.entity == b.entity || a.entity == "module-class" || b.entity == "module-class"
    };
    let results: Vec<Option<(Finding, String, String, bool)>> = (0..entities.len())
        .into_par_iter()
        .map(|i| {
            let e = &entities[i];
            let l = e.skeleton.len();
            let tol = (l / 5).max(2);
            let lo = l.saturating_sub(tol);
            let hi = l + tol;
            let mut best: Option<usize> = None;
            for &len in len_keys.iter().filter(|&&k| k >= lo && k <= hi) {
                let bucket = &buckets[&len];
                let start = bucket.partition_point(|&j| j <= i);
                for &j in &bucket[start..] {
                    let earlier = &entities[j];
                    if !compatible(e, earlier) {
                        continue;
                    }
                    // #73: a record's declared fields are its facts. Two
                    // records whose fields differ hold different data — there
                    // is no shared behaviour for a fix to land in one copy and
                    // miss in the other, so they are siblings, not a fork.
                    if fields_differ(e, earlier) {
                        continue;
                    }
                    // two classes pair on matching constants as well as on
                    // shape: small classes are alike by nature, so the config
                    // evidence carries the fork claim there. Modules pair on
                    // the structural measure alone — a whole file's shape is
                    // evidence enough, and constant-free module forks exist.
                    if consts_differ(e, earlier)
                    {
                        continue;
                    }
                    let sim = dice_from_bigrams(interner.bigrams_of(i), interner.bigrams_of(j));
                    if sim >= 0.9 {
                        best = Some(j);
                        break;
                    }
                }
            }
            let j = best?;
            // the finding lands on the LATER entity (list order), naming the
            // earlier one — mirrors duplicate_findings' _first_duplicate
            // semantics, so the earlier copy's readers see the fork at the
            // later copy
            let (earlier, later) = (e, &entities[j]);
            let eset: std::collections::HashSet<&str> = earlier.members.iter().map(|m| m.as_str()).collect();
            let mut matched: Vec<String> = later
                .members
                .iter()
                .filter(|m| eset.contains(m.as_str()))
                .cloned()
                .collect();
            matched.sort();
            let sim = dice_from_bigrams(interner.bigrams_of(i), interner.bigrams_of(j));
            let what = if later.entity == "class" { "classes" } else { "modules" };
            let members_text = if matched.is_empty() {
                String::new()
            } else {
                format!(" — matched members: {}", matched.join(", "))
            };
            // C2: two classes inheriting the SAME base have a shared owner —
            // the fork prose is false there (a fix in one subclass does not
            // silently diverge; the base is the shared home)
            let shared_base = earlier.entity == "class"
                && later.entity == "class"
                && !earlier.bases.is_empty()
                && earlier.bases == later.bases;
            let message = if shared_base {
                format!(
                    "{what}: {a} and {b} are two subclasses of one base with identical shape; only their constants differ. If they are config-variants, move the constants into the base and delete the duplicate subclasses. If they were meant to differ, one is missing its override.",
                    what = what,
                    a = earlier.name,
                    b = later.name,
                )
            } else {
                let consts_clause = if !earlier.consts.is_empty() && earlier.consts == later.consts {
                    format!(" with identical constants {{{}}}", later.consts.join(", "))
                } else {
                    String::new()
                };
                format!(
                    "{what} '{}' ({}:{}) and '{}' ({}:{}) are {:.0}% similar{}{} — a structural match. A fix can land in one copy and silently miss the other. If they are copies, reconcile them into one. If one is generated from the other and a check that fails when they diverge pins them, this is not a fork and the finding does not apply.",
                    earlier.name,
                    earlier.rel,
                    earlier.line,
                    later.name,
                    later.rel,
                    later.line,
                    sim * 100.0,
                    consts_clause,
                    members_text,
                )
            };
            Some((
                Finding { logical_start: None, seam_members: Vec::new(),
                col: 0,
                file: later.rel.clone(),
                line: later.line,
                function: later.name.clone(),
                kind: "duplicate-module".into(),
                severity: "fail".into(),
                message, },
                earlier.rel.clone(),
                later.rel.clone(),
                later.entity == "class",
            ))
        })
        .collect();
    // one finding per file pair, at the most specific level: when a class
    // pair and its enclosing module pair report the same two files, the
    // class finding wins — it names the classes and their relationship.
    let class_pairs: std::collections::HashSet<(String, String)> = results
        .iter()
        .flatten()
        .filter(|(_, _, _, is_class)| *is_class)
        .map(|(_, a, b, _)| (a.clone(), b.clone()))
        .collect();
    for (f, a, b, is_class) in results.into_iter().flatten() {
        if !is_class && class_pairs.contains(&(a, b)) {
            continue;
        }
        out.push(f);
    }
    out
}

/// A directory whose browsable files (modules + docs) cross the threshold —
/// the material of a structure with no structure. Refs are repo-relative;
/// the caller filters `__init__.py`, vendored and ignored dirs, so this is a
/// pure count over what the reader actually browses.
pub const FLAT_FOLDER_THRESHOLD: usize = 20;

pub fn flat_folder_findings(modules: &[String], docs: &[String]) -> Vec<Finding> {
    use std::collections::BTreeMap;
    // per dir: (module rels, doc rels) — the names are the evidence for which
    // domain subsets the folder can be split into
    let mut per_dir: BTreeMap<String, (Vec<&String>, Vec<&String>)> = BTreeMap::new();
    for rel in modules {
        if rel.rsplit('/').next() == Some("__init__.py") {
            continue; // a package marker, not a module the reader opens
        }
        let dir = rel.rsplit_once('/').map_or("", |(d, _)| d);
        per_dir.entry(dir.to_string()).or_default().0.push(rel);
    }
    for rel in docs {
        let dir = rel.rsplit_once('/').map_or("", |(d, _)| d);
        per_dir.entry(dir.to_string()).or_default().1.push(rel);
    }
    let mut out = Vec::new();
    for (dir, (mods, txt)) in per_dir {
        if mods.len() + txt.len() < FLAT_FOLDER_THRESHOLD {
            continue;
        }
        let label = if dir.is_empty() { "the repo root" } else { dir.as_str() };
        // the action names what the folder holds: a docs pile has no modules
        // to group (round: "'docs' holds 31 docs … group the modules")
        let action = match (mods.len(), txt.len()) {
            (0, _) => "Split it into coherent topic subsets: one subfolder per topic these pages are about, named with the topic its pages share.",
            (_, 0) => "Split it into coherent domain subsets: one subfolder per thing these modules are about, named with the noun its members share.",
            _ => "Split it into coherent domain subsets: one subfolder per thing these files are about, named with the noun its members share.",
        };
        // the candidates the names already offer — the action's evidence, and
        // the antidote to `misc/` or `1/`: a subset is a thing, not a bucket
        let names: Vec<&String> = mods.iter().chain(txt.iter()).copied().collect();
        let candidates = subset_candidates(&names, &dir);
        let advice = if candidates.is_empty() {
            String::new()
        } else {
            format!(
                " Its names already repeat: {candidates} — check that each one names a thing before moving the files."
            )
        };
        out.push(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            col: 0,
            file: if dir.is_empty() { ".".to_string() } else { dir.clone() },
            line: 0,
            function: String::new(),
            kind: "flat-folder".into(),
            severity: "warn".into(),
            message: format!(
                "'{label}' holds {} files ({m} modules, {t} docs) — at {FLAT_FOLDER_THRESHOLD}+ files one folder stops telling a reader which belong together. {action}{advice} A split by file kind, size, or number is not a subset: it names the files, not the thing they belong to.",
                mods.len() + txt.len(),
                m = mods.len(),
                t = txt.len()
            ),
        });
    }
    out
}

/// The lower-case tokens a filename carries: split on separators
/// (`_`, `-`, `.`) AND CamelCase boundaries, so `CommuteNode`,
/// `commute.node` and `commute_node` all offer `commute` and `node`.
fn name_tokens(stem: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for piece in stem.split(|c: char| !c.is_alphanumeric()) {
        let mut start = 0usize;
        let chars: Vec<char> = piece.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            let boundary = i > start && c.is_uppercase() && chars[i - 1].is_lowercase();
            if boundary || i + 1 == chars.len() {
                let end = if i + 1 == chars.len() { i + 1 } else { i };
                let token: String = chars[start..end].iter().collect::<String>().to_lowercase();
                if token.len() > 2 {
                    out.push(token);
                }
                start = i;
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The nouns the folder's filenames already repeat: tokens shared by >= 3
/// files and by less than 60% of them (a token in most files is the folder's
/// own family — `node` in `nodes/` — not a subset). Up to three, most common
/// first. These are candidates for a reader to check, never orders.
fn subset_candidates(names: &[&String], dir: &str) -> String {
    use std::collections::BTreeMap;
    // The folder's own name is the family's name — `nodes/`, `apis/`, `bus/` —
    // never one of its subsets. Plain and plural forms of the same word are
    // matched by suffix, so a singular folder (`bus`) is not singularised into
    // nonsense (`bu`).
    let family = dir.rsplit('/').next().unwrap_or(dir).to_lowercase();
    let is_family = |token: &str| token == family || format!("{token}s") == family || format!("{token}es") == family;
    let mut per_token: BTreeMap<String, usize> = BTreeMap::new();
    for rel in names {
        let stem = rel.rsplit('/').next().unwrap_or(rel);
        let stem = stem.rsplit_once('.').map_or(stem, |(s, _)| s);
        for token in name_tokens(stem) {
            *per_token.entry(token).or_default() += 1;
        }
    }
    let limit = (names.len() as f64 * 0.6).max(3.0) as usize;
    let mut candidates: Vec<(String, usize)> = per_token
        .into_iter()
        .filter(|(token, n)| *n >= 3 && *n < limit && !is_family(token))
        .collect();
    candidates.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    candidates
        .iter()
        .take(3)
        .map(|(token, n)| format!("{token} ({n})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Cross-file copy-paste findings (`_duplicate_actions`).
///
/// Not O(n²): the tolerance guard (`|len_a - len_b| <= max(2, len/5)`) means
/// only skeletons of similar length can ever match, so candidates are
/// bucketed by skeleton length and each one probes only the lengths within
/// its window — near-linear for real repos, and the all-same-length
/// degenerate case is genuinely pairwise (every candidate is a valid peer).
/// `_first_duplicate` semantics are preserved exactly: the earliest LATER
/// candidate (list order) at >= 90% Dice.
///
/// Dice is computed for every pair: a content hash of the unique bigram SET
/// cannot shortcut the decision — equal unique sets with different
/// multiplicities (a repeated bigram) score Dice < 0.9 while hashing equal,
/// and differing sets can still score >= 0.9. The set hash is neither
/// necessary nor sufficient, so it is not a valid collision test
/// (2026-08-17 review-log: set-vs-multiset 100%-similar false positive).
pub fn duplicate_findings(fns: &[SkeletonFn]) -> Vec<Finding> {
    use std::collections::HashMap;
    let mut out = Vec::new();
    let interner = BigramInterner::new(&fns.iter().map(|f| f.skeleton.as_slice()).collect::<Vec<_>>());
    let mut buckets: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, fr) in fns.iter().enumerate() {
        buckets.entry(fr.skeleton.len()).or_default().push(i);
    }
    let mut len_keys: Vec<usize> = buckets.keys().copied().collect();
    len_keys.sort_unstable();
    let results: Vec<Option<Finding>> = (0..fns.len())
        .into_par_iter()
        .map(|i| {
            let fr = &fns[i];
            let l = fr.skeleton.len();
            let tol = (l / 5).max(2);
            let lo = l.saturating_sub(tol);
            let hi = l + tol;
            let mut best: Option<(usize, f64)> = None;
            for &len in len_keys.iter().filter(|&&k| k >= lo && k <= hi) {
                let bucket = &buckets[&len];
                let start = bucket.partition_point(|&j| j <= i);
                for &j in &bucket[start..] {
                    let sim = dice_from_bigrams(interner.bigrams_of(i), interner.bigrams_of(j));
                    if sim >= 0.9 {
                        // a function nested inside another always resembles it:
                        // the span contains the other's — containment, not
                        // copy-paste (houses: `_fetch` inside `api_fetch`)
                        // line numbers are per file: only a SAME-FILE pair can
                        // be nested (two files' copies share the same numbers)
                        let one = (fr.line, fr.end_line);
                        let two = (fns[j].line, fns[j].end_line);
                        let nested = fr.rel == fns[j].rel
                            && ((one.0 <= two.0 && two.1 <= one.1) || (two.0 <= one.0 && one.1 <= two.1));
                        if nested {
                            continue;
                        }
                        if best.is_none_or(|(b, _)| j < b) {
                            best = Some((j, sim));
                        }
                        break; // later members of this bucket are later indices
                    }
                }
            }
            if let Some((j, sim)) = best {
                let dup = &fns[j];
                return Some(Finding { logical_start: None, seam_members: Vec::new(),
                col: 0,
                file: dup.rel.clone(),
                line: dup.line,
                function: dup.name.clone(),
                kind: "duplicate".into(),
                severity: "warn".into(),
                message: format!(
                    "function '{}' ({}:{}) is {:.0}% similar to '{}' ({}:{}) — copy-paste; extract the shared logic into one function",
                    dup.name, dup.rel, dup.line, sim * 100.0, fr.name, fr.rel, fr.line
                ), });
            }
            None
        })
        .collect();
    for f in results.into_iter().flatten() {
        out.push(f);
    }
    out
}
/// Interns skeleton tokens to small ids and precomputes each function's
/// sorted bigram index list ONCE — `duplicate_findings` then scores pairs
/// with an allocation-free two-pointer merge instead of rebuilding bigram
/// maps per pair (the save-time repo-wide merge's dominant cost: 59ms on a
/// 57-file repo for 224 skeletons).
pub struct BigramInterner {
    bigrams: Vec<(u32, u32)>,
    offsets: Vec<(usize, usize)>, // (start, end) into bigrams per input
}

impl BigramInterner {
    pub fn new(skeletons: &[&[String]]) -> Self {
        let mut intern: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
        let mut bigrams: Vec<(u32, u32)> = Vec::new();
        let mut offsets: Vec<(usize, usize)> = Vec::with_capacity(skeletons.len());
        for skel in skeletons {
            let mut ids: Vec<u32> = Vec::with_capacity(skel.len());
            for tok in *skel {
                let next = intern.len() as u32;
                let id = *intern.entry(tok.as_str()).or_insert(next);
                ids.push(id);
            }
            let start = bigrams.len();
            bigrams.extend(ids.windows(2).map(|w| (w[0], w[1])));
            bigrams[start..].sort_unstable();
            offsets.push((start, bigrams.len()));
        }
        Self { bigrams, offsets }
    }

    /// The function's sorted bigram list.
    pub fn bigrams_of(&self, i: usize) -> &[(u32, u32)] {
        let (s, e) = self.offsets[i];
        &self.bigrams[s..e]
    }
}

/// Dice over SORTED bigram lists — multiset intersection via a two-pointer
/// merge (counts equal runs). Matches `dice_similarity`'s multiset contract
/// exactly; the pairing test pins them equal on the review-log cases.
pub fn dice_from_bigrams(a: &[(u32, u32)], b: &[(u32, u32)]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut common = 0usize;
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            let x = a[i];
            let (ci, cj) = (i, j);
            while i < a.len() && a[i] == x {
                i += 1;
            }
            while j < b.len() && b[j] == x {
                j += 1;
            }
            common += (i - ci).min(j - cj);
        } else if a[i] < b[j] {
            i += 1;
        } else {
            j += 1;
        }
    }
    (2.0 * common as f64) / (a.len() + b.len()) as f64
}

/// Dead-code findings (`_unused_actions`). Definitions carry an `is_class`
/// flag — the #28 dead-class arm: a never-referenced class is as dead as a
/// never-referenced function.
pub fn unused_findings(
    definitions: &[(String, String, usize, bool)], // (rel, name, line, is_class)
    prod_refs: &HashSet<String>,
    test_refs: &HashSet<String>,
    strings: &[String],
) -> Vec<Finding> {
    let mut out = Vec::new();
    for (rel, name, line, is_class) in definitions {
        let what = if *is_class { "class" } else { "function" };
        if is_live_name(name, prod_refs, strings) {
            continue;
        }
        let (message, kind) = if test_refs.contains(name) {
            (
                format!(
                    "{what} '{name}' ({rel}:{line}) is referenced only from tests — if it is a deliberate test seam (isolation hook, fixture helper), document it; otherwise production code that nothing ships calls is dead — delete it"
                ),
                "unused",
            )
        } else {
            (
                format!(
                    "{what} '{name}' ({rel}:{line}) is defined but never referenced — dead code is deleted, not kept (unless it is a CLI command or public API entry point)"
                ),
                "unused",
            )
        };
        out.push(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            col: 0,
            file: rel.clone(),
            line: *line,
            function: name.clone(),
            kind: kind.into(),
            severity: "warn".into(),
            message,
        });
    }
    out
}
// =====================================================================
// latent-class partition + the test-only rule families (monkeypatch,
// skipif, fakefs) — the last per-file Python families
// =====================================================================

/// `_partition_for_class`: a class whose methods split into >= 2
/// field-disjoint groups after removing at most 2 connectors.
fn partition_for_class(source: &str, cls: &StmtClassDef) -> Option<(Vec<String>, Vec<Vec<String>>)> {
    let methods: Vec<&Stmt> = cls.body.iter().filter(|s| matches!(s, Stmt::FunctionDef(_))).collect();
    if methods.len() < 6 {
        return None;
    }
    let span = line_of(source, cls.range().end()) - line_of(source, cls.name.range().start());
    if span < 150 {
        return None;
    }
    // method name -> set of self.<attr> fields accessed anywhere in the body
    let mut mf: Vec<(String, HashSet<String>)> = Vec::new();
    for m in &methods {
        let Stmt::FunctionDef(f) = m else { continue };
        let mut fields = HashSet::new();
        let mut queue: Vec<Q> = f.body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
        for d in &f.decorator_list {
            queue.push(Q::N(AnyNodeRef::from(&d.expression)));
        }
        let mut qi = 0usize;
        while qi < queue.len() {
            if let Q::N(n) = queue[qi] {
                if let AnyNodeRef::ExprAttribute(a) = n {
                    if let Expr::Name(name) = a.value.as_ref() {
                        if name.id == "self" {
                            fields.insert(a.attr.to_string());
                        }
                    }
                }
                skel_children(n, &mut queue);
            }
            qi += 1;
        }
        mf.push((f.name.to_string(), fields));
    }
    let names: Vec<String> = mf.iter().map(|(n, _)| n.clone()).collect();
    // smallest connector removal exposing >= 2 disjoint groups
    for removal in 0..3usize {
        for removed in combinations(&names, removal) {
            let kept: Vec<String> = names.iter().filter(|n| !removed.contains(n)).cloned().collect();
            let groups = connected_groups(&kept, &mf);
            let big: Vec<Vec<String>> = groups
                .into_iter()
                .filter(|g| {
                    let fields: HashSet<&String> = g
                        .iter()
                        .flat_map(|m| {
                            mf.iter()
                                .find(|(n, _)| n == m)
                                .map(|(_, f)| f.iter())
                                .unwrap_or_default()
                        })
                        .collect();
                    g.len() >= 2 && fields.len() >= 2
                })
                .collect();
            if big.len() >= 2 {
                return Some((removed, big));
            }
        }
    }
    None
}

/// combinations of `names` taken `k` at a time (k <= 2, so O(n^2) max).
fn combinations(names: &[String], k: usize) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    if k == 0 {
        out.push(Vec::new());
        return out;
    }
    if k == 1 {
        for n in names {
            out.push(vec![n.clone()]);
        }
        return out;
    }
    for i in 0..names.len() {
        for j in (i + 1)..names.len() {
            out.push(vec![names[i].clone(), names[j].clone()]);
        }
    }
    out
}

/// Methods connected by sharing at least one field — `_connected_groups`.
fn connected_groups(kept: &[String], mf: &[(String, HashSet<String>)]) -> Vec<Vec<String>> {
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for start in kept {
        if seen.contains(start.as_str()) {
            continue;
        }
        let mut group: Vec<String> = Vec::new();
        let mut stack: Vec<&str> = vec![start];
        while let Some(m) = stack.pop() {
            if !seen.insert(m) {
                continue;
            }
            group.push(m.to_string());
            let fields = mf
                .iter()
                .find(|(n, _)| n == m)
                .map(|(_, f)| f)
                .cloned()
                .unwrap_or_default();
            for other in kept {
                if seen.contains(other.as_str()) {
                    continue;
                }
                let other_fields = mf
                    .iter()
                    .find(|(n, _)| n == other)
                    .map(|(_, f)| f)
                    .cloned()
                    .unwrap_or_default();
                if !fields.is_disjoint(&other_fields) {
                    stack.push(other);
                }
            }
        }
        groups.push(group);
    }
    groups
}

/// `_partition_findings`: the latent-class field-partition family.
pub fn partition_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    for s in body {
        let Stmt::ClassDef(cls) = s else { continue };
        if let Some((connectors, groups)) = partition_for_class(source, cls) {
            let groups_text = groups
                .iter()
                .map(|g| format!("{{{}}}", g.join(",")))
                .collect::<Vec<_>>()
                .join("/");
            let conn_text = if connectors.is_empty() {
                "none".to_string()
            } else {
                format!("{{{}}}", connectors.join(","))
            };
            let metric: usize = groups.iter().map(|g| g.len()).sum();
            let _ = metric;
            state.findings.push(Finding { logical_start: None, seam_members: groups.iter().flatten().cloned().collect(),
            col: 0,
            file: state.file.to_string(),
            line: line_of(source, cls.name.range().start()),
            function: cls.name.to_string(),
            kind: "partition".into(),
            severity: "fail".into(),
            message: format!(
                "methods split into {count} field-disjoint groups ({groups_text}), connectors removed: {conn_text} — each group touches only its own fields, so the class is really {count} independent classes. Split it into {count} classes. — fix: extract-class",
                count = groups.len()
            ), });
        }
    }
}

const MONKEYPATCH_METHODS: [&str; 5] = ["setattr", "setitem", "delattr", "setenv", "delenv"];

fn monkeypatch_decorator(state: &mut ScanState, source: &str, d: &Decorator, mock_imports: &HashSet<String>) {
    let expr = &d.expression;
    let func = if let Expr::Call(c) = expr {
        c.func.as_ref()
    } else {
        expr
    };
    let desc: Option<String> = match func {
        Expr::Name(name) if mock_imports.contains(name.id.as_str()) => Some(format!("@{}", name.id.as_str())),
        Expr::Attribute(a) if a.attr.as_str() == "patch" => Some("@patch".to_string()),
        _ => None,
    };
    if let Some(desc) = desc {
        mp_finding(state, &desc, line_of(source, d.range().start()));
    }
}

/// Check whether an expression chain (Attribute or Name) resolves to an
/// imported mock symbol — guards against false positives on e.g. self.client.patch.
fn attr_resolves_to_mock(val: &Expr, imps: &HashSet<String>) -> bool {
    match val {
        Expr::Name(n) => imps.contains(n.id.as_str()),
        Expr::Attribute(inner) => {
            let seg = inner.attr.to_string();
            imps.contains(&seg) || attr_resolves_to_mock(&inner.value, imps)
        }
        _ => false,
    }
}

/// `_monkeypatch_findings` — forbidden patch usage in tests.
pub fn monkeypatch_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    let mut mock_imports: HashSet<String> = HashSet::new();
    for s in body {
        match s {
            Stmt::ImportFrom(imp) => {
                if let Some(module) = &imp.module {
                    if module.to_string().contains("mock") {
                        for a in &imp.names {
                            mock_imports.insert(a.name.to_string());
                        }
                    }
                }
            }
            Stmt::Import(imp) => {
                for a in &imp.names {
                    if a.name.to_string().contains("mock") {
                        mock_imports.insert(a.name.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    // calls + decorators in one full walk
    let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    let mut qi = 0usize;
    while qi < queue.len() {
        if let Q::N(n) = queue[qi] {
            if let AnyNodeRef::ExprCall(call) = n {
                let line = line_of(source, call.range().start());
                let desc: Option<String> = match call.func.as_ref() {
                    Expr::Attribute(a) => match a.value.as_ref() {
                        Expr::Name(name)
                            if name.id == "monkeypatch" && MONKEYPATCH_METHODS.contains(&a.attr.as_str()) =>
                        {
                            Some(format!("monkeypatch.{}", a.attr.as_str()))
                        }
                        _ if a.attr.as_str() == "patch" && attr_resolves_to_mock(a.value.as_ref(), &mock_imports) => {
                            // Only flag .patch when the receiver chain resolves
                            // to an imported mock symbol — avoids false positives
                            // on e.g. self.client.patch(url)
                            Some(format!("{}.patch", &source[a.value.range()]))
                        }
                        _ => None,
                    },
                    Expr::Name(name) if name.id == "patch" && mock_imports.contains("patch") => {
                        Some("patch".to_string())
                    }
                    _ => None,
                };
                if let Some(desc) = desc {
                    mp_finding(state, &desc, line);
                }
            }
            // decorators: skel_children descends into the expression, never
            // the Decorator node — examine them at the function/class pop
            if let AnyNodeRef::StmtFunctionDef(f) = n {
                for d in &f.decorator_list {
                    monkeypatch_decorator(state, source, d, &mock_imports);
                }
            }
            if let AnyNodeRef::StmtClassDef(cls) = n {
                for d in &cls.decorator_list {
                    monkeypatch_decorator(state, source, d, &mock_imports);
                }
            }
            skel_children(n, &mut queue);
        }
        qi += 1;
    }
}

fn mp_finding(state: &mut ScanState, desc: &str, line: usize) {
    state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
            file: state.file.to_string(),
            line,
            function: String::new(),
            kind: "monkeypatch".into(),
            severity: "fail".into(),
            message: format!(
                "{desc} at line {line} — never monkeypatch global state; inject an object fake (a class implementing the real protocol) via parameter injection or a dependency-injection container — fakes are objects, not functions"
            ), });
}

const SKIPIF_NEEDLES: [&str; 5] = ["os.environ", "environ", "getenv", "os.path.exists", "sys.platform"];

/// `pytest.mark.skip` — the parked-test decorator (attribute chain only, so
/// unrelated `.skip` attributes are not flagged).
fn is_pytest_mark_skip(a: &ExprAttribute) -> bool {
    if a.attr.as_str() != "skip" {
        return false;
    }
    match a.value.as_ref() {
        Expr::Attribute(inner) => {
            inner.attr.as_str() == "mark" && matches!(inner.value.as_ref(), Expr::Name(n) if n.id.as_str() == "pytest")
        }
        _ => false,
    }
}

/// A parked-test finding — same family (kind `skipif`) and same message as
/// the env-needle case.
fn parked_skip_finding(state: &mut ScanState, source: &str, offset: ruff_text_size::TextSize) {
    let line = line_of(source, offset);
    state.findings.push(Finding {
        logical_start: None,
        seam_members: Vec::new(),
        col: 0,
        file: state.file.to_string(),
        line,
        function: String::new(),
        kind: "skipif".into(),
        severity: "fail".into(),
        message: format!("@pytest.mark.skip at line {line} — a permanently skipped test rots; fix it or delete it"),
    });
}

/// `_skipif_findings` — never skip a test: not for a missing environment
/// (`skipif` on an env needle) and not permanently (`@pytest.mark.skip` —
/// a parked test rots).
pub fn skipif_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    let mut qi = 0usize;
    // `@pytest.mark.skip()` is both an ExprCall and (its func) an
    // ExprAttribute node — dedupe by decorator start so one decorator
    // reports at most once.
    let mut reported: HashSet<usize> = HashSet::new();
    while qi < queue.len() {
        if let Q::N(n) = queue[qi] {
            match n {
                AnyNodeRef::ExprCall(call) => {
                    if let Expr::Attribute(a) = call.func.as_ref() {
                        if a.attr.as_str() == "skipif" {
                            let cond_parts: Vec<String> = call
                                .arguments
                                .args
                                .iter()
                                .map(|e| source[e.range()].to_string())
                                .chain(
                                    call.arguments
                                        .keywords
                                        .iter()
                                        .map(|k| source[k.value.range()].to_string()),
                                )
                                .collect();
                            let cond = cond_parts.join(" ");
                            if SKIPIF_NEEDLES.iter().any(|needle| cond.contains(needle)) {
                                state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                                                                    file: state.file.to_string(),
                                                                    line: line_of(source, call.range().start()),
                                                                    function: String::new(),
                                                                    kind: "skipif".into(),
                                                                    severity: "fail".into(),
                                                                    message: format!(
                                                                        "@pytest.mark.skipif on environment presence at line {} — never skip a test for a missing dependency: fake it (a fixture builds a stand-in) so it runs identically everywhere; only the E2E suite may skip",
                                                                        line_of(source, call.range().start())
                                                                    ), });
                            }
                        } else if is_skip_mark(a, call, &mut reported) {
                            parked_skip_finding(state, source, call.range().start());
                        }
                    }
                }
                AnyNodeRef::ExprAttribute(a)
                    if is_pytest_mark_skip(a) && reported.insert(a.range().start().to_usize()) =>
                {
                    // bare `@pytest.mark.skip` (no call parens) — the
                    // decorator expression is the attribute itself
                    parked_skip_finding(state, source, a.range().start());
                }
                _ => {}
            }
            skel_children(n, &mut queue);
        }
        qi += 1;
    }
}

const FS_PATH_METHODS: [&str; 17] = [
    "read_text",
    "write_text",
    "read_bytes",
    "write_bytes",
    "mkdir",
    "unlink",
    "rename",
    "replace",
    "touch",
    "rmdir",
    "iterdir",
    "glob",
    "rglob",
    "exists",
    "resolve",
    "symlink_to",
    "copy",
];
const FS_OS_OPS: [&str; 9] = [
    "remove", "rename", "mkdir", "makedirs", "rmdir", "unlink", "symlink", "link", "replace",
];
const FS_SHUTIL_OPS: [&str; 5] = ["copy", "copy2", "move", "rmtree", "copytree"];
const FS_TEMPFILE: [&str; 6] = [
    "TemporaryDirectory",
    "NamedTemporaryFile",
    "TemporaryFile",
    "mkdtemp",
    "mkstemp",
    "mktemp",
];

/// `_fakefs_findings` — real FS access in tests without pyfakefs.
pub fn fakefs_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    let uses_fakefs_base = {
        let mut base = false;
        for s in body {
            match s {
                Stmt::ImportFrom(imp) => {
                    if imp.module.as_ref().is_some_and(|m| m.to_string().contains("pyfakefs")) {
                        base = true;
                    }
                }
                Stmt::Import(imp) => {
                    if imp.names.iter().any(|a| a.name.to_string().contains("pyfakefs")) {
                        base = true;
                    }
                }
                Stmt::ClassDef(cls) => {
                    if let Some(arguments) = &cls.arguments {
                        for b in &arguments.args {
                            if source[b.range()].to_lowercase().contains("fakefs")
                                || source[b.range()].to_lowercase().contains("fake_filesystem")
                            {
                                base = true;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        base
    };
    // all functions, with their enclosing test-class status
    let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    let mut qi = 0usize;
    while qi < queue.len() {
        if let Q::N(n) = queue[qi] {
            if let AnyNodeRef::StmtFunctionDef(f) = n {
                let line = line_of(source, f.name.range().start());
                let is_test = f.name.as_str().starts_with("test_")
                    || body.iter().any(|s| {
                        matches!(s, Stmt::ClassDef(c) if c.name.to_lowercase().starts_with("test")
                            && c.body.iter().any(|m| m.range().start() == n.range().start()))
                    });
                if !is_test {
                    skel_children(n, &mut queue);
                    qi += 1;
                    continue;
                }
                let uses_fakefs = f
                    .parameters
                    .posonlyargs
                    .iter()
                    .chain(&f.parameters.args)
                    .any(|p| p.parameter.name.as_str() == "fs");
                if uses_fakefs || uses_fakefs_base {
                    skel_children(n, &mut queue);
                    qi += 1;
                    continue;
                }
                // real-FS usage vs sanctioned real-FS need
                let mut real_fs = false;
                let mut needs_real = false;
                // ast.walk includes decorators + parameter annotations —
                // a skipif guard on a real file (.exists()) is FS usage
                let mut fq: Vec<Q> = f.body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
                for d in &f.decorator_list {
                    fq.push(Q::N(AnyNodeRef::from(&d.expression)));
                }
                let mut fi = 0usize;
                while fi < fq.len() {
                    if let Q::N(fn_) = fq[fi] {
                        match fn_ {
                            AnyNodeRef::ExprName(n) if n.id == "tmp_path" => real_fs = true,
                            AnyNodeRef::ExprName(n) if matches!(n.id.as_str(), "sqlite3" | "subprocess") => {
                                needs_real = true
                            }
                            AnyNodeRef::ExprAttribute(a)
                                if matches!(a.attr.as_str(), "symlink" | "symlink_to" | "link") =>
                            {
                                needs_real = true
                            }
                            AnyNodeRef::ExprCall(c) => {
                                match c.func.as_ref() {
                                    // bare open(...)/tempfile(...) count; a Path(...).open(...)
                                    // attribute call does NOT (Python's _real_fs_usage: only
                                    // Name funcs match open/tempfile; attribute calls must be
                                    // in the path/os/shutil/tempfile method lists)
                                    Expr::Name(n) if matches!(n.id.as_str(), "open" | "tempfile") => {
                                        real_fs = true;
                                    }
                                    Expr::Attribute(a) => {
                                        let attr = a.attr.as_str();
                                        if FS_PATH_METHODS.contains(&attr)
                                            || FS_OS_OPS.contains(&attr)
                                            || FS_SHUTIL_OPS.contains(&attr)
                                            || FS_TEMPFILE.contains(&attr)
                                        {
                                            real_fs = true;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                        skel_children(fn_, &mut fq);
                    }
                    fi += 1;
                }
                if real_fs && !needs_real {
                    state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                                            file: state.file.to_string(),
                                            line,
                                            function: f.name.to_string(),
                                            kind: "fakefs".into(),
                                            severity: "fail".into(),
                                            message: format!(
                                                "test '{}' at line {line} touches the real filesystem (tmp_path/open/Path) without pyfakefs — tests fake the filesystem (the `fs` fixture or fake_filesystem_unittest). Reach the real filesystem only when the code under test needs real FS semantics (subprocess interop, symlinks, C-level I/O like sqlite3), or when the test's subject is a checked-in file the repo ships (a workflow, a build script); comment why",
                                                f.name.as_str()
                                            ), });
                }
            }
            skel_children(n, &mut queue);
        }
        qi += 1;
    }
}

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
                    line: line_of(source, f.name.range().start()),
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

fn param_is_mutated(body: &[Stmt], name: &str) -> bool {
    /// `sources[…]`, `del sources[…]`, and the augmented forms. `sources["a"]
    /// ["b"] = …` stores into `sources["a"]`, so the chain walks to its root.
    fn subscript_of(e: &Expr, name: &str) -> bool {
        match e {
            Expr::Subscript(s) => rooted_at(&s.value, name),
            _ => false,
        }
    }
    fn rooted_at(e: &Expr, name: &str) -> bool {
        match e {
            Expr::Name(n) => n.id.as_str() == name,
            Expr::Subscript(s) => rooted_at(&s.value, name),
            Expr::Attribute(a) => rooted_at(&a.value, name),
            _ => false,
        }
    }
    const MUTATORS: [&str; 7] = [
        "pop",
        "popitem",
        "update",
        "clear",
        "setdefault",
        "__setitem__",
        "__delitem__",
    ];
    let mut sq: Vec<&Stmt> = body.iter().collect();
    while let Some(s) = sq.pop() {
        let stored = match s {
            Stmt::Assign(a) => a.targets.iter().any(|t| subscript_of(t, name)),
            Stmt::AnnAssign(a) => subscript_of(&a.target, name),
            Stmt::AugAssign(a) => subscript_of(&a.target, name),
            Stmt::Delete(d) => d.targets.iter().any(|t| subscript_of(t, name)),
            _ => false,
        };
        let mutated = stmt_exprs(s).iter().any(|e| match e {
            Expr::Call(c) => match c.func.as_ref() {
                Expr::Attribute(at) => MUTATORS.contains(&at.attr.as_str()) && rooted_at(&at.value, name),
                _ => false,
            },
            _ => false,
        });
        if stored || mutated {
            return true;
        }
        push_stmt_children(s, &mut sq);
    }
    false
}

fn is_empty_literal(e: &Expr) -> bool {
    matches!(e, Expr::StringLiteral(s) if s.value.is_empty())
        || matches!(e, Expr::List(l) if l.elts.is_empty())
        || matches!(e, Expr::Dict(d) if d.items.is_empty())
        || matches!(e, Expr::Tuple(t) if t.elts.is_empty())
}

// =====================================================================
// over-abstraction: an ABC with exactly one concrete subclass is ceremony
// (the last Python finding family — cross-file class + import resolution)
// =====================================================================

const ABSTRACT_DECORATORS: [&str; 4] = [
    "abstractmethod",
    "abstractproperty",
    "abstractclassmethod",
    "abstractstaticmethod",
];

/// Top-level classes with abstractness and base references.
pub fn collect_classes(body: &[Stmt], source: &str) -> Vec<crate::ClassInfo> {
    let mut out = Vec::new();
    for s in body {
        let Stmt::ClassDef(cls) = s else { continue };
        let mut abstract_ = false;
        for d in &cls.decorator_list {
            let expr = if let Expr::Call(c) = &d.expression {
                c.func.as_ref()
            } else {
                &d.expression
            };
            match expr {
                Expr::Name(n) if ABSTRACT_DECORATORS.contains(&n.id.as_str()) => abstract_ = true,
                Expr::Attribute(a) if ABSTRACT_DECORATORS.contains(&a.attr.as_str()) => abstract_ = true,
                _ => {}
            }
        }
        let mut bases: Vec<String> = Vec::new();
        let mut declared: Vec<String> = Vec::new();
        for m in &cls.body {
            match m {
                Stmt::AnnAssign(a) => {
                    if let Expr::Name(n) = a.target.as_ref() {
                        declared.push(n.id.to_string());
                    }
                }
                Stmt::Assign(a) => {
                    for t in &a.targets {
                        if let Expr::Name(n) = t {
                            if n.id.as_str() == "__slots__" {
                                if let Some(tuple) = slot_names(&a.value) {
                                    declared.extend(tuple);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        for m in &cls.body {
            let Stmt::FunctionDef(f) = m else { continue };
            if f.name.as_str() == "__init__" {
                for st in &f.body {
                    if let Stmt::AnnAssign(a) = st {
                        if let Expr::Attribute(at) = a.target.as_ref() {
                            if matches!(at.value.as_ref(), Expr::Name(n) if n.id.as_str() == "self") {
                                declared.push(at.attr.to_string());
                            }
                        }
                    }
                }
            }
        }
        declared.sort();
        declared.dedup();
        if let Some(arguments) = &cls.arguments {
            for b in &arguments.args {
                match b {
                    Expr::Name(n) => {
                        if matches!(n.id.to_lowercase().as_str(), "abc" | "abcmeta") {
                            abstract_ = true;
                        }
                        bases.push(format!("Name:{}", n.id.as_str()));
                    }
                    Expr::Attribute(a) => {
                        if matches!(a.attr.to_lowercase().as_str(), "abc" | "abcmeta") {
                            abstract_ = true;
                        }
                        if let Expr::Name(v) = a.value.as_ref() {
                            bases.push(format!("Attr:{}:{}", v.id.as_str(), a.attr.as_str()));
                        }
                    }
                    _ => {}
                }
            }
        }
        out.push(crate::ClassInfo {
            name: cls.name.to_string(),
            line: line_of(source, cls.name.range().start()),
            abstract_,
            bases,
            declared,
        });
    }
    out
}

/// Import aliases from top-level imports — `_import_map`.
pub fn collect_imports(body: &[Stmt], _source: &str) -> Vec<crate::ImportInfo> {
    let mut out = Vec::new();
    let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
    let mut qi = 0usize;
    while qi < queue.len() {
        if let Q::N(n) = queue[qi] {
            match n {
                AnyNodeRef::StmtImportFrom(imp) => {
                    if let Some(module) = &imp.module {
                        for a in &imp.names {
                            let alias = a
                                .asname
                                .as_ref()
                                .map(|x| x.to_string())
                                .unwrap_or_else(|| a.name.to_string());
                            out.push(crate::ImportInfo {
                                alias,
                                module: module.to_string(),
                                imported: a.name.to_string(),
                            });
                        }
                    }
                }
                AnyNodeRef::StmtImport(imp) => {
                    for a in &imp.names {
                        let alias = a
                            .asname
                            .as_ref()
                            .map(|x| x.to_string())
                            .unwrap_or_else(|| a.name.to_string());
                        out.push(crate::ImportInfo {
                            alias,
                            module: a.name.to_string(),
                            imported: a.name.to_string(),
                        });
                    }
                }
                _ => {}
            }
            skel_children(n, &mut queue);
        }
        qi += 1;
    }
    out
}

/// `_class_key`: a module reference to a file rel.
fn class_key(
    classes: &std::collections::HashMap<(String, String), &crate::ClassInfo>,
    mrel: &str,
    mname: &str,
) -> Option<(String, String)> {
    if mrel.ends_with(".py") {
        let key = (mrel.to_string(), mname.to_string());
        return if classes.contains_key(&key) { Some(key) } else { None };
    }
    let base = mrel.replace('.', "/");
    for candidate in [format!("{base}.py"), format!("{base}/__init__.py")] {
        let key = (candidate, mname.to_string());
        if classes.contains_key(&key) {
            return Some(key);
        }
    }
    None
}

/// Abstract classes with exactly one concrete subclass — `_abstraction_actions`.
pub fn abstraction_findings(scans: &[(String, Vec<crate::ClassInfo>, Vec<crate::ImportInfo>)]) -> Vec<Finding> {
    use std::collections::HashMap;
    let mut classes: HashMap<(String, String), &crate::ClassInfo> = HashMap::new();
    for (rel, cls_list, _) in scans {
        for c in cls_list {
            classes.insert((rel.clone(), c.name.clone()), c);
        }
    }
    let mut concrete: HashMap<(String, String), Vec<String>> = HashMap::new();
    for (rel, cls_list, imports) in scans {
        let import_map: HashMap<&str, (&str, &str)> = imports
            .iter()
            .map(|i| (i.alias.as_str(), (i.module.as_str(), i.imported.as_str())))
            .collect();
        for c in cls_list {
            for base in &c.bases {
                let candidates: Vec<(String, String)> = if let Some(rest) = base.strip_prefix("Name:") {
                    let mut cands = vec![(rel.clone(), rest.to_string())];
                    if let Some((module, imported)) = import_map.get(rest) {
                        cands.push(((*module).to_string(), (*imported).to_string()));
                    }
                    cands
                } else if let Some(rest) = base.strip_prefix("Attr:") {
                    let mut parts = rest.splitn(2, ':');
                    let alias = parts.next().unwrap_or("");
                    let attr = parts.next().unwrap_or("");
                    let mut cands = Vec::new();
                    if let Some((module, _name)) = import_map.get(alias) {
                        cands.push(((*module).to_string(), attr.to_string()));
                    }
                    cands
                } else {
                    Vec::new()
                };
                for (mrel, mname) in candidates {
                    if let Some(key) = class_key(&classes, &mrel, &mname) {
                        if key != (rel.clone(), c.name.clone())
                            && classes.get(&key).map(|k| k.abstract_).unwrap_or(false)
                        {
                            concrete.entry(key).or_default().push(c.name.clone());
                        }
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut entries: Vec<((String, String), Vec<String>)> = concrete.into_iter().collect();
    entries.sort();
    for ((rel, name), subs) in entries {
        if subs.len() == 1 {
            let line = classes.get(&(rel.clone(), name.clone())).map(|c| c.line).unwrap_or(1);
            out.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                            file: rel.clone(),
                            line,
                            function: name.clone(),
                            kind: "over-abstraction".into(),
                            severity: "fail".into(),
                            message: format!(
                                "abstract class '{name}' in {rel} has exactly one concrete subclass ('{}') — an ABC with a single implementation is ceremony: fold the subclass into the base or drop the ABC; an abstraction earns its keep at two real, differing implementations",
                                subs[0]
                            ), });
        }
    }
    out
}

// ------------------------------------------------- unnamed-condition: named decisions
// Each names one decision the checks make inline, so the call site reads as the
// question it asks rather than a list of operators.

/// Big enough, or mutating shared state, to be a closures candidate.
fn is_closures_candidate(cc: u32, span: u32, mutated: &[String]) -> bool {
    cc >= 15 || span >= 60 || !mutated.is_empty()
}

/// The read reaches into a collaborator: not the receiver, not a parameter or
/// loop target, and often enough to be a habit.
fn is_collaborator_envy(
    receiver: &str,
    reads: usize,
    params: &HashSet<String>,
    loop_targets: &HashSet<String>,
    collaborators: &HashSet<String>,
    self_reads: usize,
) -> bool {
    receiver != "self"
        && !params.contains(receiver)
        && !loop_targets.contains(receiver)
        && collaborators.contains(receiver)
        && reads >= 4
        && reads >= self_reads + 3
}

/// A static husk: every method static, nothing property-like, no dataclass
/// fields, no state written.
fn is_static_husk(all_static: bool, has_property: bool, has_dataclass_fields: bool, writes_state: bool) -> bool {
    all_static && !has_property && !has_dataclass_fields && !writes_state
}

/// A radix prefix (`0x`, `0o`, `0b`) — a literal that states its base.
pub fn is_radix_prefix(prefix: &str) -> bool {
    prefix.eq_ignore_ascii_case("0x") || prefix.eq_ignore_ascii_case("0o") || prefix.eq_ignore_ascii_case("0b")
}

/// The name component is a unit token, or its plural.
fn is_unit_token(component: &str, token: &str) -> bool {
    component == token
        || (component.len() == token.len() + 1 && component.starts_with(token) && component.ends_with('s'))
}

/// The if body is a single nested if — one link of a guard chain.
fn is_nested_if_body(if_stmt: &ruff_python_ast::StmtIf) -> bool {
    if_stmt.body.len() == 1 && if_stmt.elif_else_clauses.is_empty() && matches!(if_stmt.body[0], Stmt::If(_))
}

/// The module is outside the identity scan: test code, a package marker, or
/// vendored.
fn is_skipped_module(state: &ScanState, rel: &str) -> bool {
    state.is_test || rel.ends_with("__init__.py") || is_vendored(rel)
}

/// Two class skeletons carry different field sets — siblings, not a fork.
fn fields_differ(e: &crate::skeleton::SkeletonModule, earlier: &crate::skeleton::SkeletonModule) -> bool {
    !e.fields.is_empty() && !earlier.fields.is_empty() && e.fields != earlier.fields
}

/// Two class skeletons pair on constants only when both carry them.
fn consts_differ(e: &crate::skeleton::SkeletonModule, earlier: &crate::skeleton::SkeletonModule) -> bool {
    e.entity == "class" && earlier.entity == "class" && (e.consts.is_empty() || e.consts != earlier.consts)
}

/// The file's shape is one class plus module-level imports and constants.
fn is_module_class(classes: usize, other_statements: bool, module_qualifies: bool) -> bool {
    classes == 1 && other_statements && module_qualifies
}

/// The name is still used: `main`, a production reference, or a string
/// reference (a framework's dispatch by name).
fn is_live_name(name: &str, prod_refs: &HashSet<String>, strings: &[String]) -> bool {
    name == "main" || prod_refs.contains(name) || strings.iter().any(|s| s.contains(name))
}

/// A bare `@pytest.mark.skip` with no reason — the skip hides the test.
fn is_skip_mark(attr: &ExprAttribute, call: &ExprCall, reported: &mut HashSet<usize>) -> bool {
    attr.attr.as_str() == "skip"
        && is_pytest_mark_skip(attr)
        && call.arguments.args.is_empty()
        && call.arguments.keywords.is_empty()
        && reported.insert(call.range().start().to_usize())
}

/// A pytest assertion attribute (`assert*`, `raises`, `fail`).
fn is_assert_attr(attr: &ExprAttribute) -> bool {
    attr.attr.as_str().starts_with("assert") || attr.attr.as_str() == "raises" || attr.attr.as_str() == "fail"
}

/// Does any expression in the body reference the given name? A source-order
/// walk — nested functions included (a closure capturing `self` still uses
/// instance state).
fn body_refs_name(body: &[Stmt], name: &str) -> bool {
    use ruff_python_ast::visitor::source_order::{walk_stmt, SourceOrderVisitor};
    struct Probe<'a> {
        name: &'a str,
        found: bool,
    }
    impl<'a> SourceOrderVisitor<'a> for Probe<'a> {
        fn visit_expr(&mut self, e: &'a Expr) {
            if let Expr::Name(n) = e {
                if n.id.as_str() == self.name {
                    self.found = true;
                }
            }
            walk_stmt_probe(self, e);
        }
    }
    // walk_expr is the trait default; we must recurse manually
    fn walk_stmt_probe<'a, V: SourceOrderVisitor<'a>>(v: &mut V, e: &'a Expr) {
        ruff_python_ast::visitor::source_order::walk_expr(v, e);
    }
    let mut probe = Probe { name, found: false };
    for s in body {
        walk_stmt(&mut probe, s);
        if probe.found {
            break;
        }
    }
    probe.found
}

/// The parameter names of a function (posonly, args, kwonly, vararg, kwarg).
fn function_param_names(p: &Parameters, out: &mut HashSet<String>) {
    for a in &p.posonlyargs {
        out.insert(a.parameter.name.to_string());
    }
    for a in &p.args {
        out.insert(a.parameter.name.to_string());
    }
    for a in &p.kwonlyargs {
        out.insert(a.parameter.name.to_string());
    }
    if let Some(v) = &p.vararg {
        out.insert(v.name.to_string());
    }
    if let Some(k) = &p.kwarg {
        out.insert(k.name.to_string());
    }
}

// ------------------------------------------------------------- latent-class
// cross-function shapes (module scope): a method in exile, an anonymous
// tuple record, and a state-threading assembly site.
