//! Python function shape: what kind of body a function has, so the complexity
//! message names the shape (a dispatch chain, a rule battery, or plain code)
//! instead of prescribing extract-method blindly.
use std::collections::HashSet;

use ruff_python_ast::{Expr, Stmt, StmtIf};

/// The names BOUND (assigned) in a statement run — arm-local bindings.
/// Mirrors fix_engine's _BoundNames (Assign/AnnAssign/For/With targets and
/// walrus/import bindings): a name bound in one dispatch arm and READ in a
/// sibling cannot be a uniform handler parameter.
/// The nested-item count that makes a helper body a pipeline.
const NESTED_ITEMS_MIN: usize = 3;

struct DispatchArmNames<'a> {
    bound: HashSet<&'a str>,
    read: HashSet<&'a str>,
}

/// Python function shape — what kind of body a function has: a dispatch
/// chain, a rule battery, or plain code.
pub(crate) struct FnShape;

impl FnShape {
    /// The `acc = []` opener (plain or annotated) — (acc name, index).
    fn empty_list_init(body: &[Stmt]) -> Option<(String, usize)> {
        for (i, stmt) in body.iter().enumerate() {
            match stmt {
                Stmt::Assign(a) => {
                    if Self::is_empty_list_assignment(a) {
                        let Expr::Name(n) = &a.targets[0] else { unreachable!() };
                        return Some((n.id.to_string(), i));
                    }
                }
                Stmt::AnnAssign(a) => match (a.value.as_deref(), a.target.as_ref()) {
                    (Some(Expr::List(l)), Expr::Name(n)) if l.elts.is_empty() => {
                        return Some((n.id.to_string(), i));
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        None
    }

    /// Is the statement exactly one `acc.append(<value>)` call?
    fn is_single_append(stmts: &[Stmt], acc: &str) -> bool {
        if stmts.len() != 1 {
            return false;
        }
        let Stmt::Expr(e) = &stmts[0] else { return false };
        let Expr::Call(call) = e.value.as_ref() else {
            return false;
        };
        let Expr::Attribute(a) = call.func.as_ref() else {
            return false;
        };
        a.attr.as_str() == "append"
            && matches!(a.value.as_ref(), Expr::Name(n) if n.id.as_str() == acc)
            && call.arguments.args.len() == 1
    }

    /// The FIXABLE rule-battery shape: an `acc = []` opener, then >= 3
    /// CONTIGUOUS `if <cond>: acc.append(<v>)` checks (no else, one append
    /// each). Mirrors fix_engine's _rule_battery_shape — the offer equals the
    /// fix.
    fn rule_battery_shape(body: &[Stmt]) -> Option<String> {
        let (acc, init_idx) = Self::empty_list_init(body)?;
        let mut n = 0usize;
        for stmt in &body[init_idx + 1..] {
            let Stmt::If(i) = stmt else { break };
            if !i.elif_else_clauses.is_empty() || !Self::is_single_append(&i.body, &acc) {
                return None;
            }
            n += 1;
        }
        if n < NESTED_ITEMS_MIN {
            return None;
        }
        Some(acc)
    }

    /// The comprehension generators of a statement's expressions — their targets
    /// bind arm-locally (mirrors fix_engine's visit_CompFor).
    fn stmt_comprehension_generators(s: &Stmt) -> Vec<&ruff_python_ast::Comprehension> {
        fn collect_expr<'a>(e: &'a Expr, out: &mut Vec<&'a ruff_python_ast::Comprehension>) {
            match e {
                Expr::ListComp(c) => {
                    out.extend(c.generators.iter());
                    collect_expr(&c.elt, out);
                }
                Expr::SetComp(c) => {
                    out.extend(c.generators.iter());
                    collect_expr(&c.elt, out);
                }
                Expr::DictComp(c) => {
                    out.extend(c.generators.iter());
                    if let Some(k) = &c.key {
                        collect_expr(k, out);
                    }
                    collect_expr(&c.value, out);
                }
                Expr::Generator(g) => {
                    out.extend(g.generators.iter());
                    collect_expr(&g.elt, out);
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        match s {
            Stmt::Assign(a) => collect_expr(a.value.as_ref(), &mut out),
            Stmt::AnnAssign(a) => {
                if let Some(v) = &a.value {
                    collect_expr(v.as_ref(), &mut out);
                }
            }
            Stmt::Expr(e) => collect_expr(e.value.as_ref(), &mut out),
            Stmt::Return(r) => {
                if let Some(v) = &r.value {
                    collect_expr(v.as_ref(), &mut out);
                }
            }
            Stmt::If(i) => {
                for b in &i.body {
                    out.extend(Self::stmt_comprehension_generators(b));
                }
            }
            _ => {}
        }
        out
    }

    /// One dispatch arm's selector when the test is `sel == "lit"`.
    fn dispatch_arm_selector(i: &StmtIf) -> Option<(&str, &str)> {
        let Expr::Compare(c) = i.test.as_ref() else { return None };
        if c.ops.len() != 1 || !matches!(c.ops[0], ruff_python_ast::CmpOp::Eq) {
            return None;
        }
        let Expr::Name(n) = c.left.as_ref() else { return None };
        let Expr::StringLiteral(_) = &c.comparators[0] else {
            return None;
        };
        Some((n.id.as_str(), "lit"))
    }

    /// The FIXABLE dispatch shape: a preamble, >= 3 contiguous `if <sel> ==
    /// "lit": <body>` arms over ONE selector, then exactly one trailing
    /// `return <default>`. This mirrors fix_engine's _dispatch_chain_shape — the
    /// offer is only made when the fix applies. Arms whose body reads a name
    /// BOUND IN A SIBLING ARM are refused (the named-handler rewrite cannot
    /// preserve them — see _dispatch_named_mode).
    fn dispatch_chain_shape(body: &[Stmt]) -> Option<String> {
        let first_if = body.iter().position(|s| matches!(s, Stmt::If(_)))?;
        let mut selector: Option<&str> = None;
        let mut n = 0usize;
        for stmt in &body[first_if..] {
            let Stmt::If(i) = stmt else { break };
            let (sel, _lit) = Self::dispatch_arm_selector(i)?;
            if !i.elif_else_clauses.is_empty() {
                return None; // an arm with elif/else is not the fixable shape
            }
            match selector {
                None => selector = Some(sel),
                Some(s) if s == sel => {}
                _ => return None,
            }
            n += 1;
        }
        if n < NESTED_ITEMS_MIN {
            return None;
        }
        // the no-match path: a single trailing `return <default>`
        let tail = &body[first_if + n..];
        if tail.len() != 1 || !matches!(tail[0], Stmt::Return(_)) {
            return None;
        }
        // sibling-bound reads: an arm reading a name another arm binds cannot be
        // rewritten to uniform module-level handlers (the value does not exist
        // at the call site) — parity with the fix's refusal
        let arms: Vec<&[Stmt]> = body[first_if..first_if + n]
            .iter()
            .filter_map(|s| match s {
                Stmt::If(i) => Some(i.body.as_slice()),
                _ => None,
            })
            .collect();
        let names: Vec<DispatchArmNames> = arms.iter().map(|b| FnShape::collect_dispatch_arm_names(b)).collect();
        for (i, arm) in names.iter().enumerate() {
            for (j, other) in names.iter().enumerate() {
                if i != j && arm.read.intersection(&other.bound).next().is_some() {
                    return None;
                }
            }
        }
        selector.map(str::to_string)
    }

    fn collect_dispatch_arm_names(body: &[Stmt]) -> DispatchArmNames<'_> {
        use ruff_python_ast::visitor::source_order::{walk_stmt, SourceOrderVisitor};
        struct Collect<'a> {
            bound: HashSet<&'a str>,
            read: HashSet<&'a str>,
        }
        impl<'a> SourceOrderVisitor<'a> for Collect<'a> {
            fn visit_expr(&mut self, e: &'a Expr) {
                match e {
                    Expr::Name(n) => {
                        self.read.insert(n.id.as_str());
                    }
                    Expr::Named(n) => {
                        if let Expr::Name(t) = n.target.as_ref() {
                            self.bound.insert(t.id.as_str());
                        }
                    }
                    _ => {}
                }
                ruff_python_ast::visitor::source_order::walk_expr(self, e);
            }
            fn visit_stmt(&mut self, s: &'a Stmt) {
                match s {
                    Stmt::Assign(a) => {
                        for t in &a.targets {
                            if let Expr::Name(t) = t {
                                self.bound.insert(t.id.as_str());
                            }
                        }
                    }
                    Stmt::AnnAssign(a) => {
                        if let Expr::Name(n) = a.target.as_ref() {
                            self.bound.insert(n.id.as_str());
                        }
                    }
                    Stmt::AugAssign(a) => {
                        if let Expr::Name(n) = a.target.as_ref() {
                            self.bound.insert(n.id.as_str());
                        }
                    }
                    Stmt::For(f) => {
                        if let Expr::Name(n) = f.target.as_ref() {
                            self.bound.insert(n.id.as_str());
                        }
                    }
                    Stmt::With(w) => {
                        for item in &w.items {
                            if let Some(v) = &item.optional_vars {
                                if let Expr::Name(n) = v.as_ref() {
                                    self.bound.insert(n.id.as_str());
                                }
                            }
                        }
                    }
                    Stmt::Import(imp) => {
                        for a in &imp.names {
                            self.bound.insert(a.name.as_str());
                        }
                    }
                    Stmt::ImportFrom(imp) => {
                        for a in &imp.names {
                            self.bound.insert(a.name.as_str());
                        }
                    }
                    _ => {}
                }
                // comprehension targets bind arm-locally (mirrors fix_engine's
                // visit_CompFor)
                for gen in FnShape::stmt_comprehension_generators(s) {
                    if let Expr::Name(n) = &gen.target {
                        self.bound.insert(n.id.as_str());
                    }
                }
                walk_stmt(self, s);
            }
        }
        let mut c = Collect {
            bound: HashSet::new(),
            read: HashSet::new(),
        };
        for s in body {
            walk_stmt(&mut c, s);
        }
        DispatchArmNames {
            bound: c.bound,
            read: c.read,
        }
    }

    /// All statement lists inside a function, excluding nested function bodies —
    /// the Python `_statement_lists` equivalent.
    fn is_empty_list_assignment(assign: &ruff_python_ast::StmtAssign) -> bool {
        matches!(assign.value.as_ref(), Expr::List(l) if l.elts.is_empty())
            && assign.targets.len() == 1
            && matches!(assign.targets[0], Expr::Name(_))
    }

    pub(crate) fn collect_stmt_lists<'a>(stmt: &'a Stmt, out: &mut Vec<&'a [Stmt]>) {
        match stmt {
            Stmt::FunctionDef(_) => {}
            Stmt::ClassDef(c) => {
                out.push(&c.body);
                for s in &c.body {
                    Self::collect_stmt_lists(s, out);
                }
            }
            Stmt::If(i) => {
                out.push(&i.body);
                for s in &i.body {
                    Self::collect_stmt_lists(s, out);
                }
                for cl in &i.elif_else_clauses {
                    out.push(&cl.body);
                    for s in &cl.body {
                        Self::collect_stmt_lists(s, out);
                    }
                }
            }
            Stmt::For(f) => {
                out.push(&f.body);
                for s in &f.body {
                    Self::collect_stmt_lists(s, out);
                }
                out.push(&f.orelse);
                for s in &f.orelse {
                    Self::collect_stmt_lists(s, out);
                }
            }
            Stmt::While(w) => {
                out.push(&w.body);
                for s in &w.body {
                    Self::collect_stmt_lists(s, out);
                }
                out.push(&w.orelse);
                for s in &w.orelse {
                    Self::collect_stmt_lists(s, out);
                }
            }
            Stmt::With(w) => {
                out.push(&w.body);
                for s in &w.body {
                    Self::collect_stmt_lists(s, out);
                }
            }
            Stmt::Try(t) => {
                out.push(&t.body);
                for s in &t.body {
                    Self::collect_stmt_lists(s, out);
                }
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                    out.push(&eh.body);
                    for s in &eh.body {
                        Self::collect_stmt_lists(s, out);
                    }
                }
                out.push(&t.orelse);
                for s in &t.orelse {
                    Self::collect_stmt_lists(s, out);
                }
                out.push(&t.finalbody);
                for s in &t.finalbody {
                    Self::collect_stmt_lists(s, out);
                }
            }
            Stmt::Match(m) => {
                for case in &m.cases {
                    out.push(&case.body);
                    for s in &case.body {
                        Self::collect_stmt_lists(s, out);
                    }
                }
            }
            _ => {}
        }
    }

    /// Classify a Python function body's shape for the complexity message: a
    /// dispatch chain (>= 3 top-level ifs comparing the SAME selector to string
    /// literals) or a rule battery (>= 3 top-level ifs appending to the SAME
    /// list) get shape-specific lucid guidance; anything else is "plain". The
    /// scan sees the body once — the message must not re-parse (review: run_tool
    /// is a dispatch chain, _deterministic_violations a rule battery).
    pub(crate) fn python_fn_shape(body: &[Stmt]) -> (&'static str, String) {
        // the OFFER must equal the FIX: a shape is only "dispatch"/"rules" when
        // the fix's own shape checks pass — otherwise the directive would offer
        // something that refuses (the fix-engine v1 boundaries are strict)
        if let Some(sel) = Self::dispatch_chain_shape(body) {
            return ("dispatch", sel);
        }
        if let Some(acc) = Self::rule_battery_shape(body) {
            return ("rules", acc);
        }
        ("plain", String::new())
    }
}
