//! The per-function loop walk: the facts one function's loops are judged on,
//! and the verdicts those facts support.

use crate::rustscan;
use syn::spanned::Spanned;

use super::branches::Branches;
use super::facts::{LoopFacts, LoopKind, LoopShape, PipelineBuild};
use super::RustLoops;

/// One outermost loop's place in a statement list, for the sequence judgement.
/// `path` is the branch path from the function block: two loops are a sequence
/// only when they share it — they run in order in the same block.
struct SeqLoop {
    path: Vec<usize>,
    facts: LoopFacts,
}

/// One statement list's sequence finding, before it is emitted.
struct SequenceVerdict {
    count: usize,
    lines: Vec<String>,
    names: Vec<String>,
    top: bool,
}

/// The per-function loop pass — a flat struct holds the walk's state, so
/// the walker is a method instead of closures.
pub(super) struct LoopWalk<'a, 'b> {
    state: &'a mut rustscan::RsState<'b>,
    survivors: &'a std::collections::HashSet<String>,
    fn_name: String,
    fn_line: usize,
    seq: Vec<SeqLoop>,
}

/// Removing methods — their receiver LOSES an element. A loop condition that
/// calls one on a name the body writes is draining its own worklist.
const DRAINING_METHODS: &[&str] = &["pop", "pop_front", "pop_back", "remove", "swap_remove", "drain"];

impl<'a, 'b> LoopWalk<'a, 'b> {
    /// The surviving names a loop condition REMOVES from — the worklist a
    /// `while let Some(x) = work.pop()` walk consumes.
    fn condition_drains(&self, cond: &syn::Expr) -> Vec<String> {
        use syn::visit::Visit;

        struct Drains<'s> {
            survivors: &'s std::collections::HashSet<String>,
            names: Vec<String>,
        }
        impl Visit<'_> for Drains<'_> {
            fn visit_expr_method_call(&mut self, node: &syn::ExprMethodCall) {
                if DRAINING_METHODS.contains(&node.method.to_string().as_str()) {
                    if let syn::Expr::Path(p) = node.receiver.as_ref() {
                        if p.path.segments.len() == 1 {
                            self.record(&p.path.segments[0].ident.to_string());
                        }
                    }
                }
                syn::visit::visit_expr_method_call(self, node);
            }

            fn visit_expr_call(&mut self, node: &syn::ExprCall) {
                // the UFCS spelling of the same drain — `Vec::pop(&mut stack)`:
                // the removed-from name is the first argument, not a receiver
                if let syn::Expr::Path(p) = node.func.as_ref() {
                    if let Some(last) = p.path.segments.last() {
                        if DRAINING_METHODS.contains(&last.ident.to_string().as_str()) {
                            if let Some(syn::Expr::Reference(r)) = node.args.first() {
                                // a drain needs `&mut`: `Vec::pop(&stack)` removes
                                // nothing, so a shared borrow is not a drain
                                if r.mutability.is_some() {
                                    if let syn::Expr::Path(rp) = r.expr.as_ref() {
                                        if rp.path.segments.len() == 1 {
                                            self.record(&rp.path.segments[0].ident.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                syn::visit::visit_expr_call(self, node);
            }
        }
        impl Drains<'_> {
            fn record(&mut self, name: &str) {
                if self.survivors.contains(name) && !self.names.iter().any(|n| n == name) {
                    self.names.push(name.to_string());
                }
            }
        }
        let mut drains = Drains {
            survivors: self.survivors,
            names: Vec::new(),
        };
        drains.visit_expr(cond);
        drains.names
    }

    /// The body's writes that are fn-scope survivors, minus the loop's own
    /// pattern bindings (an iteration variable is not loop-mutated state).
    fn surviving_mutations(&self, body: &[syn::Stmt], own: &[String]) -> Vec<String> {
        let mut writes = Vec::new();
        for s in body {
            RustLoops::stmt_writes(s, &mut writes);
        }
        writes
            .into_iter()
            .filter(|w| self.survivors.contains(w) && !own.contains(w))
            .collect()
    }

    /// The facts one loop site is judged on. The body, pattern and
    /// iterable/condition are the site's; the survivor set is the function's.
    fn facts_for(
        &self,
        line: usize,
        kind: LoopKind,
        body: &[syn::Stmt],
        pattern: Option<&syn::Pat>,
        iterable: Option<&syn::Expr>,
    ) -> LoopFacts {
        let mut own = Vec::new();
        if let Some(p) = pattern {
            RustLoops::pat_idents(p, &mut own);
        }
        let mutated = self.surviving_mutations(body, &own);
        let reads: Vec<String> = iterable
            .map(|e| {
                rustscan::RustShape::expr_idents(e)
                    .into_iter()
                    .filter(|r| self.survivors.contains(r) && !own.contains(r))
                    .collect()
            })
            .unwrap_or_default();
        let worklist = match (kind, iterable) {
            (LoopKind::While, Some(cond)) => self.condition_drains(cond),
            _ => Vec::new(),
        };
        let build = if kind == LoopKind::For {
            RustLoops::pipeline_build(body, iterable)
        } else {
            None
        };
        let grows = match (kind, iterable) {
            (LoopKind::While, Some(cond)) => Self::cursor_grows(cond, &mutated),
            _ => false,
        };
        LoopFacts {
            line,
            kind,
            mutated,
            reads,
            worklist,
            build,
            statements: RustLoops::body_stmt_count(body),
            grows,
        }
    }

    /// A `while i < coll.len()` whose body writes `i` and `coll` is a cursor
    /// over a growing worklist — the same traversal as a pop-drain, spelled
    /// with an index. `coll` is in `mutated` because a `&mut coll` argument
    /// or a mutating method call counts as a write.
    fn cursor_grows(cond: &syn::Expr, mutated: &[String]) -> bool {
        let Some((_cursor, collection)) = Self::cursor_over(cond) else {
            return false;
        };
        mutated.contains(&collection)
    }

    /// The `(cursor, collection)` of `i < coll.len()` / `i <= coll.len()`.
    fn cursor_over(cond: &syn::Expr) -> Option<(String, String)> {
        let syn::Expr::Binary(b) = cond else {
            return None;
        };
        if !matches!(b.op, syn::BinOp::Lt(_) | syn::BinOp::Le(_)) {
            return None;
        }
        let cursor = Self::path_name(&b.left)?;
        let syn::Expr::MethodCall(m) = b.right.as_ref() else {
            return None;
        };
        if m.method != "len" {
            return None;
        }
        let collection = Self::path_name(&m.receiver)?;
        Some((cursor, collection))
    }

    fn path_name(e: &syn::Expr) -> Option<String> {
        let syn::Expr::Path(p) = e else {
            return None;
        };
        (p.path.segments.len() == 1).then(|| p.path.segments[0].ident.to_string())
    }

    fn judge(&mut self, facts: &LoopFacts) {
        match facts.shape() {
            LoopShape::Traversal | LoopShape::Quiet => {}
            LoopShape::Pipeline(build) => {
                // tier 1 (coding-standards.md: never offer a fix that cannot
                // apply): the directive appears ONLY when the fixer would
                // apply at this line — same contract, same refusal reasons.
                let fixable = crate::fix::LoopPipeline::fixable(self.state.source, self.state.file, facts.line);
                let directive = if fixable { " — fix: loop-pipeline" } else { "" };
                let message = match build {
                    PipelineBuild::Push(acc) => format!(
                        "the loop's body only pushes to `{acc}` — Replace Loop with Pipeline: `.map(..).collect()` (`.filter(..)` for the guarded push) when the loop creates `{acc}`; `.extend(..)` when `{acc}` already held items{directive}"
                    ),
                    PipelineBuild::Indexed(acc) => format!(
                        "the loop's body only writes one position of `{acc}` per pass — Replace Loop with Pipeline: `.map(..).collect()` over the indices (`.filter(..)` for the guarded store) builds the value{directive}"
                    ),
                };
                self.state
                    .finding("loop-pipeline", "warn", facts.line, &self.fn_name, message);
            }
            LoopShape::Stateful(names) => {
                self.state.finding(
                    "mutating-loop",
                    "warn",
                    facts.line,
                    &self.fn_name,
                    format!(
                        "loop mutates {} — Replace Loop with Pipeline: {} pieces of state survive the loop and the changes are invisible at the call site; compute each output from the input instead",
                        names.join(", "),
                        names.len(),
                    ),
                );
            }
            LoopShape::Hoist { name, statements } => {
                // tier 1 (coding-standards.md: never offer a fix that cannot
                // apply): the directive appears ONLY when the hoister would
                // apply at this line (placeholder name — the name is never
                // the refusal reason).
                let fixable = crate::fix::LoopHoist::fixable_at(self.state.source, self.state.file, facts.line);
                let message = if fixable {
                    format!(
                        "loop body is {statements} statements yet mutates only {name} — hoist the per-item step into a named helper that returns the value (a domain noun for one item's contribution), then combine or fold — fix: loop-hoist --fix-name <Name>",
                    )
                } else {
                    format!(
                        "loop body is {statements} statements yet mutates only {name} — hoist the per-item step into a named helper that returns the value (a domain noun for one item's contribution), then combine or fold",
                    )
                };
                self.state
                    .finding("loop-hoist", "warn", facts.line, &self.fn_name, message);
            }
        }
    }

    fn child_path(path: &[usize], index: usize) -> Vec<usize> {
        let mut child = path.to_vec();
        child.push(index);
        child
    }

    /// Pipeline/mutating per loop at any depth; the OUTERMOST loops are
    /// collected for the sequence judgement. Closures and nested items are
    /// new scopes — their loops belong to them, not to this function.
    fn walk_stmts(&mut self, stmts: &[syn::Stmt], path: &[usize], in_loop: bool) {
        for s in stmts {
            let syn::Stmt::Expr(e, _) = s else {
                continue;
            };
            let line = s.span().start().line;
            match e {
                syn::Expr::ForLoop(f) => {
                    let facts = self.facts_for(line, LoopKind::For, &f.body.stmts, Some(&f.pat), Some(&f.expr));
                    self.judge(&facts);
                    if !in_loop {
                        self.seq.push(SeqLoop {
                            path: path.to_vec(),
                            facts,
                        });
                    }
                    self.walk_stmts(&f.body.stmts, &Self::child_path(path, 0), true);
                }
                syn::Expr::While(w) => {
                    let facts = self.facts_for(line, LoopKind::While, &w.body.stmts, None, Some(&w.cond));
                    self.judge(&facts);
                    if !in_loop {
                        self.seq.push(SeqLoop {
                            path: path.to_vec(),
                            facts,
                        });
                    }
                    self.walk_stmts(&w.body.stmts, &Self::child_path(path, 0), true);
                }
                syn::Expr::Loop(l) => {
                    let facts = self.facts_for(line, LoopKind::Loop, &l.body.stmts, None, None);
                    self.judge(&facts);
                    if !in_loop {
                        self.seq.push(SeqLoop {
                            path: path.to_vec(),
                            facts,
                        });
                    }
                    self.walk_stmts(&l.body.stmts, &Self::child_path(path, 0), true);
                }
                syn::Expr::Match(m) => {
                    // expression arm bodies can BE loops; walk each arm's
                    // expression so those sites are judged where they are.
                    // `Branches` is the statement-list relation INSIDE a body;
                    // it cannot represent a loop expression as the site itself,
                    // so the walk handles the expression and preserves the arm
                    // index in the path.
                    for (i, arm) in m.arms.iter().enumerate() {
                        self.walk_expr(&arm.body, &Self::child_path(path, i), in_loop);
                    }
                }
                _ => {
                    for (i, branch) in Branches::of(s).into_iter().enumerate() {
                        self.walk_stmts(branch.stmts, &Self::child_path(path, i), in_loop);
                    }
                }
            }
        }
    }

    fn walk_expr(&mut self, e: &syn::Expr, path: &[usize], in_loop: bool) {
        let line = e.span().start().line;
        match e {
            syn::Expr::ForLoop(f) => {
                let facts = self.facts_for(line, LoopKind::For, &f.body.stmts, Some(&f.pat), Some(&f.expr));
                self.judge(&facts);
                self.walk_stmts(&f.body.stmts, &Self::child_path(path, 0), true);
            }
            syn::Expr::While(w) => {
                let facts = self.facts_for(line, LoopKind::While, &w.body.stmts, None, Some(&w.cond));
                self.judge(&facts);
                self.walk_stmts(&w.body.stmts, &Self::child_path(path, 0), true);
            }
            syn::Expr::Loop(l) => {
                let facts = self.facts_for(line, LoopKind::Loop, &l.body.stmts, None, None);
                self.judge(&facts);
                self.walk_stmts(&l.body.stmts, &Self::child_path(path, 0), true);
            }
            syn::Expr::If(i) => {
                self.walk_stmts(&i.then_branch.stmts, &Self::child_path(path, 0), in_loop);
                if let Some((_, els)) = &i.else_branch {
                    self.walk_expr(els, &Self::child_path(path, 1), in_loop);
                }
            }
            syn::Expr::Match(m) => {
                for (i, arm) in m.arms.iter().enumerate() {
                    self.walk_expr(&arm.body, &Self::child_path(path, i), in_loop);
                }
            }
            syn::Expr::Block(b) => {
                self.walk_stmts(&b.block.stmts, &Self::child_path(path, 0), in_loop);
            }
            syn::Expr::Unsafe(u) => {
                self.walk_stmts(&u.block.stmts, &Self::child_path(path, 0), in_loop);
            }
            _ => {}
        }
    }

    /// The sequence verdicts, one per statement list that holds two or more
    /// loops: loops SHARE a mutated name, or a later loop READS what an
    /// earlier one wrote -> the sequence IS a pipeline of passes over shared
    /// state; independent passes -> one named step per loop. Loops in sibling
    /// branches never run together and are separate groups.
    fn sequence_verdicts(&self) -> Vec<SequenceVerdict> {
        use std::collections::BTreeMap;

        let mut groups: BTreeMap<Vec<usize>, Vec<&SeqLoop>> = BTreeMap::new();
        for seq in &self.seq {
            groups.entry(seq.path.clone()).or_default().push(seq);
        }
        groups
            .into_iter()
            .filter(|(_, group)| group.len() >= 2)
            .map(|(path, group)| {
                let mut shared: std::collections::HashSet<String> = std::collections::HashSet::new();
                for (i, a) in group.iter().enumerate() {
                    for b in group.iter().skip(i + 1) {
                        for m in &a.facts.mutated {
                            if b.facts.mutated.contains(m) {
                                shared.insert(m.clone());
                            }
                        }
                    }
                }
                let mut fed: Vec<String> = Vec::new();
                for (i, a) in group.iter().enumerate() {
                    let earlier: std::collections::HashSet<&str> = group
                        .iter()
                        .take(i)
                        .flat_map(|s| s.facts.mutated.iter().map(|m| m.as_str()))
                        .collect();
                    for r in &a.facts.reads {
                        if earlier.contains(r.as_str()) && !fed.contains(r) {
                            fed.push(r.clone());
                        }
                    }
                }
                let mut names: Vec<String> = shared.into_iter().chain(fed.iter().cloned()).collect();
                names.sort();
                names.dedup();
                SequenceVerdict {
                    count: group.len(),
                    lines: group.iter().map(|s| s.facts.line.to_string()).collect(),
                    names,
                    top: path.is_empty(),
                }
            })
            .collect()
    }

    /// Emit one finding per sequence group. The finding anchors at the
    /// function line because the fixer rewrites the function's top-level
    /// chain; only that group advertises the directive (tier 1).
    fn emit_sequence(&mut self) {
        for verdict in self.sequence_verdicts() {
            let line = self.fn_line;
            let name = self.fn_name.clone();
            let directive =
                if verdict.top && crate::fix::loop_sequence_fixable(self.state.source, self.state.file, line) {
                    " — fix: loop-sequence"
                } else {
                    ""
                };
            if !verdict.names.is_empty() {
                self.state.finding(
                    "loop-sequence",
                    "warn",
                    line,
                    &name,
                    format!(
                        "{} sequential loops at lines {} share or feed the same state ({}) — the sequence IS a pipeline of passes over shared state: combine the passes or extract each into a named step{}",
                        verdict.count,
                        verdict.lines.join(", "),
                        verdict.names.join(", "),
                        directive,
                    ),
                );
            } else {
                self.state.finding(
                    "loop-sequence",
                    "warn",
                    line,
                    &name,
                    format!(
                        "{} sequential loops at lines {} change the function's state independently — extract each loop into a named step (one named pass per loop), so the function reads as a sequence of steps",
                        verdict.count,
                        verdict.lines.join(", "),
                    ),
                );
            }
        }
    }
}

impl LoopWalk<'_, '_> {
    /// The per-function pass: pipeline/mutating/hoist per loop at any depth,
    /// loop-sequence for the function's OUTERMOST loops.
    pub(crate) fn findings(
        state: &mut rustscan::RsState,
        sig: &syn::Signature,
        block: &syn::Block,
        fn_name: &str,
        fn_line: usize,
    ) {
        let mut survivors = rustscan::RustShape::param_names(sig);
        super::RustLoops::bind_fn_stmts(&block.stmts, &mut survivors);
        let mut ctx = LoopWalk {
            state,
            survivors: &survivors,
            fn_name: fn_name.to_string(),
            fn_line,
            seq: Vec::new(),
        };
        // the borrow of `survivors` ends with the walk — copy the stmts' span
        // view first so the walker holds no alias into the fn body map
        let stmts: Vec<syn::Stmt> = block.stmts.clone();
        ctx.walk_stmts(&stmts, &[], false);
        ctx.emit_sequence();
    }
}
