//! The loop family (Replace Loop with Pipeline): a loop that builds a
//! collection, mutates scattered state, or chains sequential passes has a
//! combinator form — and the finding names which rewrite applies.

use ruff_python_ast::{Expr, Operator, Stmt};

use crate::ScanState;

mod function_walk;
mod module_walk;

use module_walk::ModuleWalk;

/// Whether a binding creates an empty collection (a later loop may still be
/// that collection's own build) or carries content.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Creation {
    Empty,
    Filled,
}

/// The loop family — Replace Loop with Pipeline: the rule's shape predicates
/// and the binding facts its two walkers read.
pub struct LoopFamily;

impl LoopFamily {
    /// The control-flow children of a statement — nested def/class scopes excluded.
    fn stmt_children(s: &Stmt) -> Vec<&Stmt> {
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
    fn collection_mutation(e: &Expr) -> bool {
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
    fn single_collection_mutation(s: &Stmt) -> bool {
        match s {
            Stmt::Expr(e) => LoopFamily::collection_mutation(&e.value),
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
    fn body_is_pipeline(stmts: &[Stmt]) -> bool {
        match stmts {
            [Stmt::If(i)] => {
                i.elif_else_clauses.is_empty()
                    && i.body.len() == 1
                    && LoopFamily::single_collection_mutation(&i.body[0])
            }
            [s] => LoopFamily::single_collection_mutation(s),
            _ => false,
        }
    }

    /// (P6) The collection name a pipeline-shaped loop mutates — the receiver
    /// of an append/add/update call, the `name += [..]` target, or the
    /// subscript base of `container[k] = v`.
    fn pipeline_target(stmts: &[Stmt]) -> Option<String> {
        match stmts {
            [Stmt::If(i)] => LoopFamily::pipeline_target(&i.body),
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
    fn collect_expr_target_names(e: &Expr, creation: Creation, out: &mut Vec<(String, Creation)>) {
        match e {
            Expr::Name(n) => out.push((n.id.to_string(), creation)),
            Expr::Tuple(t) => {
                for elt in &t.elts {
                    LoopFamily::collect_expr_target_names(elt, creation, out);
                }
            }
            Expr::List(l) => {
                for elt in &l.elts {
                    LoopFamily::collect_expr_target_names(elt, creation, out);
                }
            }
            Expr::Starred(st) => LoopFamily::collect_expr_target_names(&st.value, creation, out),
            _ => {}
        }
    }

    /// (P6) The names a statement BINDS (assignment targets incl. unpacking,
    /// ann/aug assigns, loop targets, def/class/import names, with targets),
    /// each with whether the binding is an EMPTY collection creation
    /// (`= {}` / `= []`) — an empty creation is not content, so a later loop
    /// may still be that collection's own build.
    fn binding_names(s: &Stmt, out: &mut Vec<(String, Creation)>) {
        match s {
            Stmt::Assign(a) => {
                let creation = if a.targets.len() == 1 && LoopFamily::is_empty_collection(&a.value) {
                    Creation::Empty
                } else {
                    Creation::Filled
                };
                for t in &a.targets {
                    LoopFamily::collect_expr_target_names(t, creation, out);
                }
            }
            Stmt::AnnAssign(a) => {
                let creation = if a.value.as_deref().is_some_and(LoopFamily::is_empty_collection) {
                    Creation::Empty
                } else {
                    Creation::Filled
                };
                LoopFamily::collect_expr_target_names(&a.target, creation, out);
            }
            Stmt::AugAssign(a) => LoopFamily::collect_expr_target_names(&a.target, Creation::Filled, out),
            Stmt::For(f) => LoopFamily::collect_expr_target_names(&f.target, Creation::Filled, out),
            Stmt::FunctionDef(f) => out.push((f.name.to_string(), Creation::Filled)),
            Stmt::ClassDef(c) => out.push((c.name.to_string(), Creation::Filled)),
            Stmt::Import(i) => {
                for a in &i.names {
                    let bound = match &a.asname {
                        Some(n) => n.to_string(),
                        None => a.name.split('.').next().unwrap_or(a.name.as_str()).to_string(),
                    };
                    out.push((bound, Creation::Filled));
                }
            }
            Stmt::ImportFrom(im) => {
                for a in &im.names {
                    if a.name.as_str() != "*" {
                        let bound = match &a.asname {
                            Some(n) => n.to_string(),
                            None => a.name.to_string(),
                        };
                        out.push((bound, Creation::Filled));
                    }
                }
            }
            Stmt::With(w) => {
                for item in &w.items {
                    if let Some(v) = &item.optional_vars {
                        LoopFamily::collect_expr_target_names(v, Creation::Filled, out);
                    }
                }
            }
            _ => {}
        }
    }

    /// (P6) Record one (name, empty-creation) pair into the binding map — a
    /// name bound with content is never the loop's own build again.
    fn record_binding(bindings: &mut std::collections::HashMap<String, bool>, name: &str, creation: Creation) {
        if creation == Creation::Empty {
            bindings.entry(name.to_string()).or_insert(false);
        } else {
            bindings.insert(name.to_string(), true);
        }
    }

    /// (P6) Accumulate every name a statement binds into the map.
    fn collect_bindings(s: &Stmt, bindings: &mut std::collections::HashMap<String, bool>) {
        let mut names: Vec<(String, Creation)> = Vec::new();
        LoopFamily::binding_names(s, &mut names);
        for (n, creation) in names {
            LoopFamily::record_binding(bindings, &n, creation);
        }
    }

    /// An auto-fixable pipeline loop: the single mutation is one the fix
    /// engine's comprehension rewrite expresses (append/appendleft/add/extend
    /// receivers, `name += [..]`, a subscript store). update/add_update
    /// receivers are pipeline-shaped but not rewritable — the finding must not
    /// advertise a fix that cannot apply (user ruling: never suggest a fix
    /// that cannot fix).
    fn auto_fixable(stmts: &[Stmt]) -> bool {
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
    pub fn findings(state: &mut ScanState, body: &[Stmt], source: &str) {
        ModuleWalk::walk(state, body, source);
    }
}
