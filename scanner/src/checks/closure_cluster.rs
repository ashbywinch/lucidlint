//! `closure-cluster`: a method whose nested closures partition its locals — a
//! class-in-a-method.

// lucidlint: ignore-file boolean-arg the flags are the walker's API contract (skip
// nested defs / include lambdas), not unnamed modes
// lucidlint: ignore-file complexity the bound-name walkers are dispatch tables over
// the statement kinds — arm count is the AST's surface, not branching

use std::collections::HashSet;

use ruff_python_ast::{Expr, Stmt, StmtFunctionDef};
use ruff_text_size::Ranged;

use crate::finding::Finding;

use super::function_facts::fn_param_names;
use super::{push_stmt_children, stmt_exprs, walk_expr_deep};
use crate::{line_of, ScanState};

/// Names bound (assigned/annassigned/augassigned/looped/with'd) in a
/// statement list, NOT descending into nested defs — a method's OWN locals.
fn collect_bound_names(body: &[Stmt], out: &mut HashSet<String>) {
    let mut stack: Vec<&Stmt> = body.iter().collect();
    while let Some(s) = stack.pop() {
        if matches!(s, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
            continue;
        }
        match s {
            Stmt::Assign(a) => {
                for t in &a.targets {
                    if let Expr::Name(n) = t {
                        out.insert(n.id.to_string());
                    }
                }
            }
            Stmt::AnnAssign(a) => {
                if let Expr::Name(n) = a.target.as_ref() {
                    out.insert(n.id.to_string());
                }
            }
            Stmt::AugAssign(a) => {
                if let Expr::Name(n) = a.target.as_ref() {
                    out.insert(n.id.to_string());
                }
            }
            Stmt::For(f) => {
                if let Expr::Name(n) = f.target.as_ref() {
                    out.insert(n.id.to_string());
                }
            }
            Stmt::With(w) => {
                for item in &w.items {
                    if let Some(v) = &item.optional_vars {
                        if let Expr::Name(n) = v.as_ref() {
                            out.insert(n.id.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
        push_stmt_children(s, &mut stack);
    }
}

/// Every Name referenced anywhere under `body` (nested defs included — an
/// inner closure still references the method's locals).
fn referenced_names(body: &[Stmt], out: &mut HashSet<String>) {
    let mut stack: Vec<&Stmt> = body.iter().collect();
    while let Some(s) = stack.pop() {
        for e in stmt_exprs(s) {
            walk_expr_deep(e, false, &mut |x, _| {
                if let Expr::Name(n) = x {
                    out.insert(n.id.to_string());
                }
            });
        }
        push_stmt_children(s, &mut stack);
    }
}

/// A method whose nested closures partition its locals — a class-in-a-method
/// (#32). >=2 connected components (per the local x nested-fn bipartite
/// graph), each with >=2 nested fns and >=1 distinct local. Degenerate arm:
/// an empty-param method >=150 lines with nested defs. WARN — the shape is
/// often a deliberate cohesive serving method.
pub fn closure_cluster_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    for s in body {
        let Stmt::ClassDef(cls) = s else { continue };
        for m in &cls.body {
            let Stmt::FunctionDef(method) = m else { continue };
            if method.name.as_str().starts_with("__") {
                continue;
            }
            let nested: Vec<&StmtFunctionDef> = method
                .body
                .iter()
                .filter_map(|mm| match mm {
                    Stmt::FunctionDef(f) => Some(f),
                    _ => None,
                })
                .collect();
            if nested.is_empty() {
                continue;
            }
            let method_params = fn_param_names(method);
            let mut locals: HashSet<String> = method_params.iter().cloned().collect();
            collect_bound_names(&method.body, &mut locals);
            // per nested fn: the method locals it references
            let mut fn_refs: Vec<(String, HashSet<String>)> = Vec::new();
            for f in &nested {
                let mut refs = HashSet::new();
                referenced_names(&f.body, &mut refs);
                let own_params: HashSet<String> = fn_param_names(f).into_iter().collect();
                refs = refs.difference(&own_params).cloned().collect();
                refs = refs.intersection(&locals).cloned().collect();
                fn_refs.push((f.name.to_string(), refs));
            }
            // components over the fn graph: fns are linked when they share a
            // referenced local
            let mut comps: Vec<(Vec<String>, HashSet<String>)> = Vec::new();
            let mut seen: HashSet<usize> = HashSet::new();
            for i in 0..fn_refs.len() {
                if seen.contains(&i) {
                    continue;
                }
                let mut stack: Vec<usize> = vec![i];
                let mut fns: Vec<String> = Vec::new();
                let mut comp_locals: HashSet<String> = HashSet::new();
                while let Some(ci) = stack.pop() {
                    if !seen.insert(ci) {
                        continue;
                    }
                    fns.push(fn_refs[ci].0.clone());
                    comp_locals.extend(fn_refs[ci].1.iter().cloned());
                    for j in 0..fn_refs.len() {
                        if seen.contains(&j) {
                            continue;
                        }
                        if !fn_refs[ci].1.is_disjoint(&fn_refs[j].1) {
                            stack.push(j);
                        }
                    }
                }
                comps.push((fns, comp_locals));
            }
            let cluster_comps: Vec<&(Vec<String>, HashSet<String>)> = comps
                .iter()
                .filter(|(fns, locals)| fns.len() >= 2 && !locals.is_empty())
                .collect();
            let empty_params = method_params.iter().all(|p| p == "self" || p == "cls") || method_params.is_empty();
            let span = line_of(source, method.range().end()).saturating_sub(line_of(source, method.range().start()));
            let degenerate = empty_params && span >= 150;
            if cluster_comps.len() < 2 && !degenerate {
                continue;
            }
            let groups_text = cluster_comps
                .iter()
                .map(|(fns, locals)| {
                    let mut ls: Vec<&String> = locals.iter().collect();
                    ls.sort();
                    format!(
                        "{{{}}} over {{{}}}",
                        fns.join(", "),
                        ls.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            let line = line_of(source, method.name.range().start());
            state.findings.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                col: 0,
                file: state.file.to_string(),
                line,
                function: method.name.to_string(),
                kind: "closure-cluster".into(),
                severity: "warn".into(),
                message: format!(
                    "method '{}.{}' has nested closures split into {} groups over disjoint locals: {groups_text} — \
                 a class-in-a-method: each group is a cohesive unit that could be its own class. The split is \
                 structurally visible but may be a deliberate cohesive serving method — extract by hand when it is, \
                 and only when it reads better",
                    cls.name.as_str(),
                    method.name.as_str(),
                    cluster_comps.len()
                ),
            });
        }
    }
}
