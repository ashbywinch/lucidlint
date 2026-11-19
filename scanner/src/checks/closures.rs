//! `closures`: a function whose nested functions carry the work and its state —
//! the nested structure is a class waiting to be extracted.
// lucidlint: ignore-file boolean-arg the two flags are the walker's API contract
// (through-attributes / keeps-count), not unnamed modes

use std::collections::HashSet;

use ruff_python_ast::{Expr, Stmt, StmtFunctionDef};

use crate::finding::Finding;

use super::expr_walk::{push_stmt_children, walk_expr_deep};
use super::function_facts::FunctionFacts;
use crate::{stmt_line, ScanState};

/// `closures`: the rule object. Its methods read one function's nested definitions and captured state.
pub struct Closures;

/// Method calls that mutate the receiver in place — the writes that turn a
/// captured enclosing-scope local into shared instance state.
const MUTATING_METHODS: &[&str] = &[
    "append",
    "extend",
    "insert",
    "pop",
    "remove",
    "clear",
    "sort",
    "reverse",
    "add",
    "discard",
    "update",
    "setdefault",
    "appendleft",
    "appendright",
    "put",
    "push",
    "enqueue",
    "__setitem__",
    "__iadd__",
];

impl Closures {
    /// Closures: >= 2 inner functions/lambdas with cc >= 15, span >= 60, OR
    /// shared-state mutation (closures that WRITE to the enclosing function's
    /// locals are a class in disguise even when small). The mutation gate counts
    /// a write reached through a captured object's attribute (`writer.lines.append`);
    /// the message's accumulator claim renders only when a closure writes the
    /// captured local itself (`L.append`), so the message never calls a
    /// collaborator an accumulator.
    pub fn closure_findings(state: &mut ScanState, stmt: &Stmt, cc: u32, span: u32) {
        let Stmt::FunctionDef(f) = stmt else { return };
        let inner = FunctionFacts::inner_function_count(stmt);
        if inner < 2 {
            return;
        }
        let mutated = Self::closures_mutate_shared_state(f);
        if !Self::is_closures_candidate(cc, span, &mutated) {
            return;
        }
        let line = stmt_line(state.source, stmt);
        // the claim is about the captured LOCAL, not an object it holds: a
        // method call on `prop.attr` is a collaborator being used, not an
        // accumulator being filled.
        let assigned = Self::captured_state_writes(f, false);
        let why = if assigned.is_empty() {
            String::new()
        } else {
            format!(
                " — its closures write to {} captured local{} (the accumulator pattern)",
                assigned.len(),
                if assigned.len() == 1 { "" } else { "s" }
            )
        };
        state.findings.push(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            col: 0,
            file: state.file.to_string(),
            line,
            function: f.name.to_string(),
            kind: "closures".into(),
            severity: "fail".into(),
            message: format!(
                "'{}' defines {inner} inner functions closing over its state{why} — a class in disguise",
                f.name.as_str()
            ),
        });
    }

    /// The distinct enclosing-scope locals that the direct inner functions WRITE
    /// to (mutating method calls, subscript/attribute stores, nonlocal rebinds).
    /// Empty = the closures only read the enclosing state (a factory of handlers
    /// is the legit idiom; a class in disguise is not). Only DIRECT inner
    /// functions are scanned — a deeper nesting is judged by its own
    /// `Self::closure_findings` run when its enclosing function is scanned.
    pub fn closures_mutate_shared_state(f: &StmtFunctionDef) -> Vec<String> {
        Self::captured_state_writes(f, true)
    }

    /// The distinct enclosing-scope locals the direct inner functions write.
    /// `through_attributes` counts a mutating call on an attribute reached from
    /// the local (`writer.lines.append`) as a write to `writer` — the
    /// collaborator shape, used for the mutation GATE. Without it only the local
    /// itself counts (`L.append`), which is the message's accumulator claim.
    fn captured_state_writes(f: &StmtFunctionDef, through_attributes: bool) -> Vec<String> {
        let (inners, outer_bound) = Self::scope_fns_and_bindings(&f.body);
        let mut params = HashSet::new();
        FunctionFacts::function_param_names(&f.parameters, &mut params);
        let mut mutated: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for inner in inners {
            let mut inner_bound = HashSet::new();
            FunctionFacts::function_param_names(&inner.parameters, &mut inner_bound);
            let (_, deeper) = Self::scope_fns_and_bindings(&inner.body);
            inner_bound.extend(deeper);
            let candidates: HashSet<&str> = outer_bound
                .union(&params)
                .filter(|n| !inner_bound.contains(*n))
                .map(|s| s.as_str())
                .collect();
            for name in &candidates {
                if !seen.contains(*name)
                    && Self::scope_mutates(&inner.body, &HashSet::from([*name]), through_attributes)
                {
                    seen.insert(name.to_string());
                    mutated.push(name.to_string());
                }
            }
        }
        mutated
    }

    /// Does any statement in *body* (control-flow descend, not nested def/class
    /// scopes) WRITE to one of *candidates*?
    fn scope_mutates(body: &[Stmt], candidates: &HashSet<&str>, through_attributes: bool) -> bool {
        let mut stack: Vec<&Stmt> = Vec::new();
        for s in body {
            stack.push(s);
        }
        while let Some(s) = stack.pop() {
            if Self::stmt_writes(s, candidates, through_attributes) {
                return true;
            }
            if matches!(s, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
                continue; // a new scope — its bodies belong to a deeper run
            }
            push_stmt_children(s, &mut stack);
        }
        false
    }

    fn stmt_writes(s: &Stmt, candidates: &HashSet<&str>, through_attributes: bool) -> bool {
        match s {
            Stmt::Assign(a) => a.targets.iter().any(|t| Self::store_target_writes(t, candidates)),
            Stmt::AnnAssign(a) => Self::store_target_writes(&a.target, candidates),
            Stmt::AugAssign(a) => Self::store_target_writes(&a.target, candidates),
            Stmt::Delete(d) => d.targets.iter().any(|t| Self::store_target_writes(t, candidates)),
            Stmt::Nonlocal(n) => n.names.iter().any(|id| candidates.contains(id.as_str())),
            Stmt::Expr(e) => Self::expr_writes(&e.value, candidates, through_attributes),
            Stmt::Return(r) => r
                .value
                .as_ref()
                .is_some_and(|v| Self::expr_writes(v, candidates, through_attributes)),
            Stmt::If(i) => Self::expr_writes(&i.test, candidates, through_attributes),
            Stmt::While(w) => Self::expr_writes(&w.test, candidates, through_attributes),
            Stmt::For(f) => {
                Self::expr_writes(&f.iter, candidates, through_attributes)
                    || Self::expr_writes(&f.target, candidates, through_attributes)
            }
            Stmt::With(w) => w.items.iter().any(|item| {
                Self::expr_writes(&item.context_expr, candidates, through_attributes)
                    || item
                        .optional_vars
                        .as_ref()
                        .is_some_and(|v| Self::expr_writes(v, candidates, through_attributes))
            }),
            Stmt::Try(t) => t.handlers.iter().any(|h| match h {
                ruff_python_ast::ExceptHandler::ExceptHandler(eh) => eh
                    .type_
                    .as_ref()
                    .is_some_and(|t| Self::expr_writes(t, candidates, through_attributes)),
            }),
            Stmt::Assert(a) => Self::expr_writes(&a.test, candidates, through_attributes),
            _ => false,
        }
    }

    /// An assignment/delete TARGET that writes INTO a candidate container
    /// (L[0] = x, L.attr = x, del L[i]) — plain name rebinds are not writes to
    /// the object (they need `nonlocal`, handled separately).
    fn store_target_writes(t: &Expr, candidates: &HashSet<&str>) -> bool {
        match t {
            Expr::Name(_) => false,
            Expr::Subscript(s) => {
                let mut v = s.value.as_ref();
                loop {
                    match v {
                        Expr::Subscript(inner) => v = inner.value.as_ref(),
                        Expr::Name(n) => return candidates.contains(n.id.as_str()),
                        _ => return false,
                    }
                }
            }
            Expr::Attribute(a) => Self::store_target_writes(&a.value, candidates),
            Expr::Tuple(t) => t.elts.iter().any(|e| Self::store_target_writes(e, candidates)),
            Expr::List(l) => l.elts.iter().any(|e| Self::store_target_writes(e, candidates)),
            Expr::Starred(st) => Self::store_target_writes(&st.value, candidates),
            _ => false,
        }
    }

    /// Does *e* WRITE to a candidate: a mutating method call on it, or a write
    /// nested anywhere inside (call args, subscripts, comprehensions, lambdas).
    /// `through_attributes` peels a receiver chain to its base name
    /// (`writer.lines.append` writes `writer`); without it only a receiver that
    /// IS the candidate name counts.
    fn expr_writes(e: &Expr, candidates: &HashSet<&str>, through_attributes: bool) -> bool {
        // the unified traversal descends every child; this closure only decides
        // whether a CALL mutates a captured candidate (the receiver check)
        let mut found = false;
        walk_expr_deep(e, false, &mut |x, _| {
            if found {
                return;
            }
            if let Expr::Call(c) = x {
                if let Expr::Attribute(a) = c.func.as_ref() {
                    if MUTATING_METHODS.contains(&a.attr.as_str()) {
                        let mut receiver = a.value.as_ref();
                        loop {
                            match receiver {
                                Expr::Attribute(inner) if through_attributes => receiver = inner.value.as_ref(),
                                Expr::Name(n) => {
                                    if candidates.contains(n.id.as_str()) {
                                        found = true;
                                    }
                                    break;
                                }
                                _ => break,
                            }
                        }
                    }
                }
            }
        });
        found
    }

    /// The direct inner functions of *body*'s scope (control-flow nesting only —
    /// a def inside a nested def/class belongs to that nested scope) and the
    /// names the scope binds at any control-flow depth.
    fn scope_fns_and_bindings(body: &[Stmt]) -> (Vec<&StmtFunctionDef>, HashSet<String>) {
        let mut fns = Vec::new();
        let mut bound: HashSet<String> = HashSet::new();
        let mut stack: Vec<&Stmt> = Vec::new();
        for s in body {
            stack.push(s);
        }
        while let Some(s) = stack.pop() {
            match s {
                Stmt::FunctionDef(f) => {
                    fns.push(f);
                    bound.insert(f.name.to_string());
                }
                Stmt::ClassDef(c) => {
                    bound.insert(c.name.to_string());
                }
                Stmt::Assign(a) => {
                    for t in &a.targets {
                        Self::expr_bindings(t, &mut bound);
                    }
                }
                Stmt::AnnAssign(a) => Self::expr_bindings(&a.target, &mut bound),
                Stmt::AugAssign(a) => Self::expr_bindings(&a.target, &mut bound),
                Stmt::For(f) => {
                    Self::expr_bindings(&f.target, &mut bound);
                }
                Stmt::With(w) => {
                    for item in &w.items {
                        if let Some(v) = &item.optional_vars {
                            Self::expr_bindings(v, &mut bound);
                        }
                    }
                }
                Stmt::Import(i) => {
                    for a in &i.names {
                        bound.insert(a.name.as_str().split('.').next().unwrap_or("").to_string());
                    }
                }
                Stmt::ImportFrom(i) => {
                    for a in &i.names {
                        if a.name.as_str() != "*" {
                            bound.insert(a.name.to_string());
                        }
                    }
                }
                _ => {}
            }
            if matches!(s, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
                continue; // nested scopes are not this scope's bindings
            }
            push_stmt_children(s, &mut stack);
        }
        (fns, bound)
    }

    /// Names an expression binds as an assignment/unpacking target.
    fn expr_bindings(e: &Expr, out: &mut HashSet<String>) {
        match e {
            Expr::Name(n) => {
                out.insert(n.id.to_string());
            }
            Expr::Tuple(t) => {
                for el in &t.elts {
                    Self::expr_bindings(el, out);
                }
            }
            Expr::List(l) => {
                for el in &l.elts {
                    Self::expr_bindings(el, out);
                }
            }
            Expr::Starred(st) => Self::expr_bindings(&st.value, out),
            _ => {}
        }
    }

    /// Big enough, or mutating shared state, to be a closures candidate.
    fn is_closures_candidate(cc: u32, span: u32, mutated: &[String]) -> bool {
        cc >= 15 || span >= 60 || !mutated.is_empty()
    }
}
