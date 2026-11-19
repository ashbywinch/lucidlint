//! The loop family (Replace Loop with Pipeline): a loop that builds a
//! collection, mutates scattered state, or chains sequential passes has a
//! combinator form — and the finding names which rewrite applies.
// lucidlint: ignore-file complexity the loop-shape walker is ONE exhaustive match over
// statements — the arm count is the AST's surface, not branching
// lucidlint: ignore-file boolean-arg the two flags are the walker's API contract
// (keep / target), not unnamed modes — Rust has no named arguments

use ruff_python_ast::{Expr, Operator, Stmt, StmtFunctionDef};
use ruff_text_size::Ranged;

use crate::{finding::Finding, source::Source, ScanState};

// =====================================================================
// the loop family (Replace Loop with Pipeline) — Python

/// The control-flow children of a statement — nested def/class scopes excluded.
fn loop_stmt_children(s: &Stmt) -> Vec<&Stmt> {
    let mut out = Vec::new();
    match s {
        Stmt::If(i) => {
            for b in &i.body {
                out.push(b);
            }
            for cl in &i.elif_else_clauses {
                for b in &cl.body {
                    out.push(b);
                }
            }
        }
        Stmt::For(f) => {
            for b in &f.body {
                out.push(b);
            }
            for b in &f.orelse {
                out.push(b);
            }
        }
        Stmt::While(w) => {
            for b in &w.body {
                out.push(b);
            }
            for b in &w.orelse {
                out.push(b);
            }
        }
        Stmt::With(w) => {
            for b in &w.body {
                out.push(b);
            }
        }
        Stmt::Try(t) => {
            for b in &t.body {
                out.push(b);
            }
            for h in &t.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                for b in &eh.body {
                    out.push(b);
                }
            }
            for b in &t.orelse {
                out.push(b);
            }
            for b in &t.finalbody {
                out.push(b);
            }
        }
        Stmt::Match(m) => {
            for case in &m.cases {
                for b in &case.body {
                    out.push(b);
                }
            }
        }
        _ => {}
    }
    out
}

/// A mutating receiver call (append/add/update/...): `out.append(x)`.
fn loop_collection_mutation(e: &Expr) -> bool {
    match e {
        Expr::Call(c) => matches!(c.func.as_ref(), Expr::Attribute(a)
            if matches!(a.attr.as_str(), "append" | "add" | "extend" | "update" | "appendleft" | "add_update")),
        _ => false,
    }
}

/// ONE collection-building mutation: a mutating receiver call, `name +=
/// [..]` (list/tuple literal concatenation), or `container[k] = v` — the
/// shapes a comprehension replaces. Anything else in the body makes the
/// loop stateful, not a plain pipeline.
fn loop_single_collection_mutation(s: &Stmt) -> bool {
    match s {
        Stmt::Expr(e) => loop_collection_mutation(&e.value),
        Stmt::AugAssign(a) => {
            matches!(a.op, Operator::Add)
                && matches!(a.target.as_ref(), Expr::Name(_))
                && matches!(a.value.as_ref(), Expr::List(_) | Expr::Tuple(_))
        }
        Stmt::Assign(a) => a.targets.len() == 1 && matches!(&a.targets[0], Expr::Subscript(_)),
        _ => false,
    }
}

/// A loop whose body is exactly ONE collection mutation (with at most one
/// if-filter, no else) — the comprehension shape.
fn loop_body_is_pipeline(stmts: &[Stmt]) -> bool {
    match stmts {
        [Stmt::If(i)] => {
            i.elif_else_clauses.is_empty() && i.body.len() == 1 && loop_single_collection_mutation(&i.body[0])
        }
        [s] => loop_single_collection_mutation(s),
        _ => false,
    }
}

/// (P6) The collection name a pipeline-shaped loop mutates — the receiver
/// of an append/add/update call, the `name += [..]` target, or the
/// subscript base of `container[k] = v`.
fn loop_pipeline_target(stmts: &[Stmt]) -> Option<String> {
    match stmts {
        [Stmt::If(i)] => loop_pipeline_target(&i.body),
        [Stmt::Expr(e)] => match e.value.as_ref() {
            Expr::Call(c) => match c.func.as_ref() {
                Expr::Attribute(a) => match a.value.as_ref() {
                    Expr::Name(n) => Some(n.id.to_string()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        },
        [Stmt::AugAssign(a)] => match a.target.as_ref() {
            Expr::Name(n) => Some(n.id.to_string()),
            _ => None,
        },
        [Stmt::Assign(a)] => match &a.targets[0] {
            Expr::Subscript(s) => match s.value.as_ref() {
                Expr::Name(n) => Some(n.id.to_string()),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// (P6) Is this expression an EMPTY dict/list literal (`= {}`, `= []`)?
fn is_empty_collection(e: &Expr) -> bool {
    matches!(e, Expr::Dict(d) if d.items.is_empty()) || matches!(e, Expr::List(l) if l.elts.is_empty())
}

/// (P6) The names an expression binds (with unpacking) — every assignment
/// target spelled by the expression.
fn collect_expr_target_names(e: &Expr, empty: bool, out: &mut Vec<(String, bool)>) {
    match e {
        Expr::Name(n) => out.push((n.id.to_string(), empty)),
        Expr::Tuple(t) => {
            for elt in &t.elts {
                collect_expr_target_names(elt, empty, out);
            }
        }
        Expr::List(l) => {
            for elt in &l.elts {
                collect_expr_target_names(elt, empty, out);
            }
        }
        Expr::Starred(st) => collect_expr_target_names(&st.value, empty, out),
        _ => {}
    }
}

/// (P6) The names a statement BINDS (assignment targets incl. unpacking,
/// ann/aug assigns, loop targets, def/class/import names, with targets),
/// each with whether the binding is an EMPTY collection creation
/// (`= {}` / `= []`) — an empty creation is not content, so a later loop
/// may still be that collection's own build.
fn collect_stmt_binding_names(s: &Stmt, out: &mut Vec<(String, bool)>) {
    match s {
        Stmt::Assign(a) => {
            let empty = a.targets.len() == 1 && is_empty_collection(&a.value);
            for t in &a.targets {
                collect_expr_target_names(t, empty, out);
            }
        }
        Stmt::AnnAssign(a) => {
            let empty = a.value.as_deref().is_some_and(is_empty_collection);
            collect_expr_target_names(&a.target, empty, out);
        }
        Stmt::AugAssign(a) => collect_expr_target_names(&a.target, false, out),
        Stmt::For(f) => collect_expr_target_names(&f.target, false, out),
        Stmt::FunctionDef(f) => out.push((f.name.to_string(), false)),
        Stmt::ClassDef(c) => out.push((c.name.to_string(), false)),
        Stmt::Import(i) => {
            for a in &i.names {
                let bound = match &a.asname {
                    Some(n) => n.to_string(),
                    None => a.name.split('.').next().unwrap_or(a.name.as_str()).to_string(),
                };
                out.push((bound, false));
            }
        }
        Stmt::ImportFrom(im) => {
            for a in &im.names {
                if a.name.as_str() != "*" {
                    let bound = match &a.asname {
                        Some(n) => n.to_string(),
                        None => a.name.to_string(),
                    };
                    out.push((bound, false));
                }
            }
        }
        Stmt::With(w) => {
            for item in &w.items {
                if let Some(v) = &item.optional_vars {
                    collect_expr_target_names(v, false, out);
                }
            }
        }
        _ => {}
    }
}

/// (P6) Record one (name, empty-creation) pair into the binding map — a
/// name bound with content is never the loop's own build again.
fn record_binding(bindings: &mut std::collections::HashMap<String, bool>, name: &str, empty: bool) {
    if empty {
        bindings.entry(name.to_string()).or_insert(false);
    } else {
        bindings.insert(name.to_string(), true);
    }
}

/// (P6) Accumulate every name a statement binds into the map.
fn collect_stmt_bindings(s: &Stmt, bindings: &mut std::collections::HashMap<String, bool>) {
    let mut names: Vec<(String, bool)> = Vec::new();
    collect_stmt_binding_names(s, &mut names);
    for (n, empty) in names {
        record_binding(bindings, &n, empty);
    }
}

/// An auto-fixable pipeline loop: the single mutation is one the fix
/// engine's comprehension rewrite expresses (append/appendleft/add/extend
/// receivers, `name += [..]`, a subscript store). update/add_update
/// receivers are pipeline-shaped but not rewritable — the finding must not
/// advertise a fix that cannot apply (user ruling: never suggest a fix
/// that cannot fix).
fn loop_pipeline_auto_fixable(stmts: &[Stmt]) -> bool {
    fn supported_call(e: &Expr) -> bool {
        matches!(e, Expr::Call(c)
            if matches!(c.func.as_ref(), Expr::Attribute(a)
                if matches!(a.attr.as_str(), "append" | "appendleft" | "add" | "extend")))
    }
    match stmts {
        [Stmt::Expr(e)] => supported_call(&e.value),
        [Stmt::If(i)] => {
            i.elif_else_clauses.is_empty()
                && i.body.len() == 1
                && matches!(&i.body[0], Stmt::Expr(e) if supported_call(&e.value))
        }
        _ => false, // `+= [..]` and subscript stores: the base check holds
    }
}

/// The per-function loop pass — a flat struct holds the walk's state, so
/// the walker is a method instead of closures. `'a` is the ctx's own
/// borrow lifetime (the function's name), `'b` ScanState's inner source
/// lifetime — independent: the local dies with the function pass, the
/// ScanState outlives it.
struct LoopFunctionCtx<'a, 'b> {
    state: &'a mut ScanState<'b>,
    fn_name: &'a str,
    /// (P6) Module names bound BEFORE the function's line — a function loop
    /// mutating one is never this loop's own build.
    module_bindings: &'a std::collections::HashMap<String, usize>,
    /// The def line — module bindings strictly before it are pre-existing.
    fn_line: usize,
    /// The parameter names — a parameter collection is never a build.
    param_names: Vec<String>,
}

impl<'a, 'b> LoopFunctionCtx<'a, 'b> {
    fn push_finding(&mut self, line: usize, kind: &str, message: String) {
        self.state.findings.push(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            col: 0,
            file: self.state.file.to_string(),
            line,
            function: self.fn_name.to_string(),
            kind: kind.into(),
            severity: "warn".into(),
            message,
        });
    }

    /// The pipeline shape per loop at any depth — and the ONLY loop finding
    /// (P5): a loop whose body is one pure per-item collection mutation
    /// (with at most one if-filter, no else) reduces to a comprehension.
    /// Every other loop emits NOTHING — no mutating-loop, no hoist/fold
    /// advice, no sequence judgement: a PriorityQueue drain, a polling
    /// sleep-schedule, or a bit-decoder fits none of the recipes.
    fn walk_loops(&mut self, stmts: &[Stmt]) {
        let mut bindings: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
        // a parameter arrives with content — never this loop's own build
        for p in &self.param_names {
            bindings.insert(p.clone(), true);
        }
        // a module name bound before the function: pre-existing content
        for (n, l) in self.module_bindings {
            if *l < self.fn_line {
                bindings.insert(n.clone(), true);
            }
        }
        self.walk_loops_from(stmts, &mut bindings);
    }

    fn walk_loops_from(&mut self, stmts: &[Stmt], bindings: &mut std::collections::HashMap<String, bool>) {
        for s in stmts {
            match s {
                Stmt::For(f) => {
                    let line = Source::line_of(self.state.source, f.range().start());
                    // P6: the pipeline gate checks the TARGET's binding — a
                    // loop mutating a pre-existing collection (parameter,
                    // name bound earlier, module name) builds nothing new
                    // and emits nothing. Only a fresh target or one whose
                    // every prior binding is an empty `= {}` / `= []`
                    // creation is this loop's own build.
                    let fresh =
                        loop_pipeline_target(&f.body).is_none_or(|t| !bindings.get(&t).copied().unwrap_or(false));
                    if fresh && loop_body_is_pipeline(&f.body) {
                        let message = if loop_pipeline_auto_fixable(&f.body) {
                            "loop builds a collection — Replace Loop with Pipeline: use a comprehension — fix: loop-pipeline"
                        } else {
                            "loop builds a collection — Replace Loop with Pipeline: use a comprehension"
                        };
                        self.push_finding(line, "loop-pipeline", message.into());
                    }
                    // the loop target itself binds (for later loops) —
                    // nested bodies get a clone so sibling bindings stay out
                    let mut inner = bindings.clone();
                    collect_stmt_bindings(s, &mut inner);
                    self.walk_loops_from(&f.body, &mut inner);
                    self.walk_loops_from(&f.orelse, &mut inner);
                }
                Stmt::While(w) => {
                    collect_stmt_bindings(s, bindings);
                    self.walk_loops_from(&w.body, bindings);
                    self.walk_loops_from(&w.orelse, bindings);
                }
                Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {
                    collect_stmt_bindings(s, bindings);
                }
                _ => {
                    collect_stmt_bindings(s, bindings);
                    for child in loop_stmt_children(s) {
                        self.walk_loops_from(std::slice::from_ref(child), bindings);
                    }
                }
            }
        }
    }
}

/// The per-function pass: the pipeline shape for loops at any depth.
/// `module_bindings` maps every module-level name to its first binding
/// line — the "bound before the function" exclusion (P6). An async
/// function's loops emit nothing: the comprehension's awaits belong inside
/// an async comprehension, which is a different, rarer rewrite the recipe
/// does not prove.
fn loop_function_findings(
    state: &mut ScanState,
    f: &StmtFunctionDef,
    module_bindings: &std::collections::HashMap<String, usize>,
) {
    if f.is_async {
        return;
    }
    let fn_name = f.name.to_string();
    let mut param_names = Vec::new();
    for p in f
        .parameters
        .posonlyargs
        .iter()
        .chain(&f.parameters.args)
        .chain(&f.parameters.kwonlyargs)
    {
        param_names.push(p.parameter.name.to_string());
    }
    if let Some(v) = &f.parameters.vararg {
        param_names.push(v.name.to_string());
    }
    if let Some(k) = &f.parameters.kwarg {
        param_names.push(k.name.to_string());
    }
    let fn_line = Source::line_of(state.source, f.range().start());
    let mut ctx = LoopFunctionCtx {
        state,
        fn_name: &fn_name,
        module_bindings,
        fn_line,
        param_names,
    };
    ctx.walk_loops(&f.body);
}

/// (P6) Record a module-level statement's bindings: first-binding lines for
/// the function exclusion, and the empty/non-empty map for module loops.
fn record_module_bindings(
    s: &Stmt,
    module_lines: &mut std::collections::HashMap<String, usize>,
    module_bindings: &mut std::collections::HashMap<String, bool>,
    source: &str,
) {
    let line = Source::line_of(source, s.range().start());
    let mut names: Vec<(String, bool)> = Vec::new();
    collect_stmt_binding_names(s, &mut names);
    for (n, empty) in names {
        module_lines.entry(n.clone()).or_insert(line);
        record_binding(module_bindings, &n, empty);
    }
}

/// Module-level walk: the plain pipeline shape for module loops, and
/// descent into class bodies (their methods are functions — analyzed by
/// their own `loop_function_findings` run).
fn loop_module_walk(state: &mut ScanState, stmts: &[Stmt], source: &str) {
    fn walk(
        state: &mut ScanState,
        stmts: &[Stmt],
        source: &str,
        module_lines: &mut std::collections::HashMap<String, usize>,
        module_bindings: &mut std::collections::HashMap<String, bool>,
    ) {
        for s in stmts {
            match s {
                Stmt::FunctionDef(f) => {
                    loop_function_findings(state, f, module_lines);
                    record_module_bindings(s, module_lines, module_bindings, source);
                }
                Stmt::For(f) => {
                    // P6: module-level loops — the target must be fresh:
                    // no module binding before the loop except empty
                    // creations, which are this loop's own build.
                    let fresh = loop_pipeline_target(&f.body)
                        .is_none_or(|t| !module_bindings.get(&t).copied().unwrap_or(false));
                    if fresh && loop_body_is_pipeline(&f.body) {
                        let line = Source::line_of(source, f.range().start());
                        state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                        file: state.file.to_string(),
                        line,
                        function: state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default(),
                        kind: "loop-pipeline".into(),
                        severity: "warn".into(),
                        message: if loop_pipeline_auto_fixable(&f.body) {
                            "loop builds a collection — Replace Loop with Pipeline: use a comprehension — fix: loop-pipeline"
                        } else {
                            "loop builds a collection — Replace Loop with Pipeline: use a comprehension"
                        }
                        .into(), });
                    }
                    let mut inner_lines = module_lines.clone();
                    let mut inner = module_bindings.clone();
                    record_module_bindings(s, &mut inner_lines, &mut inner, source);
                    walk(state, &f.body, source, &mut inner_lines, &mut inner);
                    walk(state, &f.orelse, source, &mut inner_lines, &mut inner);
                }
                Stmt::ClassDef(cd) => {
                    record_module_bindings(s, module_lines, module_bindings, source);
                    walk(state, &cd.body, source, module_lines, module_bindings);
                }
                Stmt::While(w) => {
                    record_module_bindings(s, module_lines, module_bindings, source);
                    walk(state, &w.body, source, module_lines, module_bindings);
                    walk(state, &w.orelse, source, module_lines, module_bindings);
                }
                Stmt::If(i) => {
                    record_module_bindings(s, module_lines, module_bindings, source);
                    walk(state, &i.body, source, module_lines, module_bindings);
                    for cl in &i.elif_else_clauses {
                        walk(state, &cl.body, source, module_lines, module_bindings);
                    }
                }
                Stmt::Try(t) => {
                    record_module_bindings(s, module_lines, module_bindings, source);
                    walk(state, &t.body, source, module_lines, module_bindings);
                    for h in &t.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                        walk(state, &eh.body, source, module_lines, module_bindings);
                    }
                    walk(state, &t.orelse, source, module_lines, module_bindings);
                    walk(state, &t.finalbody, source, module_lines, module_bindings);
                }
                Stmt::With(w) => {
                    record_module_bindings(s, module_lines, module_bindings, source);
                    walk(state, &w.body, source, module_lines, module_bindings);
                }
                Stmt::Match(m) => {
                    record_module_bindings(s, module_lines, module_bindings, source);
                    for case in &m.cases {
                        walk(state, &case.body, source, module_lines, module_bindings);
                    }
                }
                _ => {
                    record_module_bindings(s, module_lines, module_bindings, source);
                }
            }
        }
    }
    walk(
        state,
        stmts,
        source,
        &mut std::collections::HashMap::new(),
        &mut std::collections::HashMap::new(),
    );
}

/// Replace Loop with Pipeline family (Python): the ONE loop shape a
/// functional pipeline replaces —
/// - `loop-pipeline`: a loop whose body is ONLY a pure per-item collection
///   mutation (append/add/update, `name += [..]`, or a subscript store)
///   with at most one if-filter, no else — a comprehension in disguise.
///
/// P5 gate: LOOP-pipeline is the ONLY loop finding. A loop that does not
/// reduce to a comprehension emits NOTHING — no mutating-loop verdict, no
/// hoist/fold advice, no sequence judgement: a PriorityQueue drain, a
/// polling sleep-schedule, or a bit-decoder fits none of the recipes.
///
/// P6 gate: the pipeline shape is not enough — the target collection must
/// be the loop's own build (no prior binding except empty `= {}` / `= []`
/// creations). A loop mutating a parameter, an earlier-bound name, or a
/// module name bound before the function emits nothing (settings_payload
/// shape).
///
/// Function-scope analysis; module-level loops get only the single-loop
/// pipeline shape (the global-state family owns module-level containers).
pub fn loop_pipeline_findings(state: &mut ScanState, body: &[Stmt], source: &str) {
    loop_module_walk(state, body, source);
}
