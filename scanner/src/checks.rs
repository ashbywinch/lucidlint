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
use ruff_python_ast::{AnyNodeRef, Decorator, Expr, ExprAttribute, ExprCall, Parameters, Stmt, StmtClassDef};
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
mod duplicate_module;
mod flat_folder;
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

pub use crate::fix_render::{fix_kind_of_message, kind_fix_default};
pub use crate::skeleton::{dice_from_bigrams, fn_skeleton, is_duplicate_candidate, skel_children, BigramInterner, Q};
pub use crate::suppressions::{apply_source_suppressions, logical_line_starts, parse_suppressions};
pub use advisory::{conditional_polymorphism_findings, guard_clause_findings, special_case_findings};
pub use closure_cluster::closure_cluster_findings;
pub use closures::closure_findings;
pub use decisions::{unnamed_classification_findings, unnamed_condition_findings};
pub use docstring::restating_docstring_findings;
pub use duplicate_block::duplicate_block_findings;
pub use duplicate_def::duplicate_def_findings;
pub use duplicate_expression::{collect_duplicate_expressions, duplicate_expression_findings, ExpressionOccurrence};
pub use duplicate_module::{collect_skeleton_modules, duplicate_module_findings};
pub use flat_folder::flat_folder_findings;
pub use forwarding::{collect_forwarders, forwarding_chain_findings};
pub use global_state::{global_state_findings, module_constant_names, shadow_findings};
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
