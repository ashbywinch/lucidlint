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
    /// The statement BLOCKS one compound statement owns — one entry per body
    /// (`if`/`else`, `try`/`except`/`finally`, ...). Blocks, not flattened
    /// children: the pipeline directive reads the statement directly before a
    /// loop, which only a sibling list preserves.
    fn stmt_bodies(s: &Stmt) -> Vec<&[Stmt]> {
        let mut out: Vec<&[Stmt]> = Vec::new();
        match s {
            Stmt::If(i) => {
                out.push(&i.body);
                for cl in &i.elif_else_clauses {
                    out.push(&cl.body);
                }
            }
            Stmt::For(f) => {
                out.push(&f.body);
                out.push(&f.orelse);
            }
            Stmt::While(w) => {
                out.push(&w.body);
                out.push(&w.orelse);
            }
            Stmt::With(w) => out.push(&w.body),
            Stmt::Try(t) => {
                out.push(&t.body);
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                    out.push(&eh.body);
                }
                out.push(&t.orelse);
                out.push(&t.finalbody);
            }
            Stmt::Match(m) => {
                for case in &m.cases {
                    out.push(&case.body);
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

    /// Is this loop a comprehension build of its OWN fresh target? The target
    /// must be fresh (not a parameter or an earlier binding), the loop must not
    /// read the collection it grows (a traversal, not a build — its readable
    /// form is a `while` drain, or no loop at all when it starts empty), it must
    /// have no `else` arm (a comprehension has none), and the body must be the
    /// one collection mutation. `fresh` is the caller's binding fact.
    fn is_own_build(f: &ruff_python_ast::StmtFor, target: &str, fresh: bool) -> bool {
        fresh && f.orelse.is_empty() && !Self::expr_mentions(&f.iter, target) && Self::body_is_pipeline(&f.body)
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

    /// Does the expression READ `name` anywhere? A loop that reads the
    /// collection it grows is a traversal, not a build — its readable form is a
    /// `while` drain (or no loop at all), not a comprehension.
    fn expr_mentions(e: &Expr, name: &str) -> bool {
        let mut exprs = Vec::new();
        super::expr_walk::all_exprs(e, &mut exprs);
        exprs
            .iter()
            .any(|e| matches!(e, Expr::Name(n) if n.id.as_str() == name))
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
        /// `acc.append(x)` with a BARE accumulator receiver and one positional
        /// argument — `fix_engine._call_mutation_plan`'s contract. A chained
        /// receiver (`groups.setdefault(k, set()).add(v)`) is not a collection
        /// name, so no comprehension expresses the loop.
        fn supported_call(e: &Expr) -> bool {
            matches!(e, Expr::Call(c)
                if c.arguments.args.len() == 1
                    && c.arguments.keywords.is_empty()
                    && matches!(c.func.as_ref(), Expr::Attribute(a)
                        if matches!(a.value.as_ref(), Expr::Name(_))
                            && matches!(a.attr.as_str(), "append" | "appendleft" | "add" | "extend")))
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

    /// The empty collection the comprehension rewrite needs directly before the
    /// loop: the accumulator's own `acc = []` / `acc = {}` / `acc = set()`, of
    /// the SAME kind the loop mutates. Anything else (prior contents, a
    /// different name, another statement in between) refuses in
    /// `fix_engine._rewrite_plan` — the rewrite cannot preserve prior contents.
    /// The `stmts` list holds statements only: comments and blank lines are
    /// trivia, so the entry before the loop is the previous STATEMENT however
    /// many lines separate them.
    fn init_is_empty(prev: Option<&Stmt>, acc: &str, kind: &str) -> bool {
        fn empty_kind(e: &Expr) -> Option<&'static str> {
            match e {
                Expr::List(l) if l.elts.is_empty() => Some("list"),
                Expr::Dict(d) if d.items.is_empty() => Some("dict"),
                Expr::Call(c)
                    if c.arguments.args.is_empty()
                        && c.arguments.keywords.is_empty()
                        && matches!(c.func.as_ref(), Expr::Name(n) if n.id.as_str() == "set") =>
                {
                    Some("set")
                }
                _ => None,
            }
        }
        let Some(prev) = prev else { return false };
        match prev {
            Stmt::Assign(a) => {
                a.targets.len() == 1
                    && matches!(&a.targets[0], Expr::Name(n) if n.id.as_str() == acc)
                    && empty_kind(&a.value) == Some(kind)
            }
            Stmt::AnnAssign(a) => {
                matches!(a.target.as_ref(), Expr::Name(n) if n.id.as_str() == acc)
                    && a.value.as_deref().and_then(empty_kind) == Some(kind)
            }
            _ => false,
        }
    }

    /// Would the comprehension rewrite apply to this loop as written? The
    /// directive is a promise about the fix engine, so the promise is decided
    /// in one place: the plan's shape (`auto_fixable`), the accumulator kind
    /// (`collection_kind`), and the accumulator's own empty collection of that
    /// kind directly before the loop (`init_is_empty`). `loop_body` is the
    /// for-loop's own body; `previous` is the statement before the loop in the
    /// ENCLOSING list (a `&[Stmt]`, so comments and blank lines are trivia —
    /// the previous entry is the previous statement however many lines
    /// separate them).
    fn directive_applies(previous: Option<&Stmt>, loop_body: &[Stmt], acc: &str) -> bool {
        Self::collection_kind(loop_body)
            .is_some_and(|kind| Self::auto_fixable(loop_body) && Self::init_is_empty(previous, acc, kind))
    }

    /// The collection kind a pipeline-shaped body mutates — the accumulator the
    /// comprehension rewrite initializes (`fix_engine._LoopPipelinePlan`).
    fn collection_kind(stmts: &[Stmt]) -> Option<&'static str> {
        let stmt = match stmts {
            [Stmt::If(i)] => i.body.first()?,
            [s] => s,
            _ => return None,
        };
        match stmt {
            Stmt::Expr(e) => match e.value.as_ref() {
                Expr::Call(c) => match c.func.as_ref() {
                    Expr::Attribute(a) => match a.attr.as_str() {
                        "append" | "appendleft" | "extend" => Some("list"),
                        "add" => Some("set"),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            },
            Stmt::AugAssign(a) => match a.op {
                Operator::Add => Some("list"),
                Operator::BitOr => Some("set"),
                _ => None,
            },
            Stmt::Assign(_) => Some("dict"),
            _ => None,
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
