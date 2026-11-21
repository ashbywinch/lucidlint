//! The per-function loop walk: the state one function's loops are judged on.

use crate::rustscan;
use syn::spanned::Spanned;

use super::{condition_drains, PipelineBuild, RustLoops, HOIST_MIN_STMTS};

/// One outermost loop's shape, for the sequence judgement.
struct SeqLoop {
    line: usize,
    mutated: Vec<String>,
    reads: Vec<String>,
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

impl<'a, 'b> LoopWalk<'a, 'b> {
    fn judge(&mut self, line: usize, body: &[syn::Stmt], is_for: bool, mutated: Vec<String>, drains: &[String]) {
        if drains.iter().any(|d| mutated.contains(d)) {
            // the loop consumes the worklist its body feeds — the writes are
            // the walk's own state, so no output can be computed from the
            // input and no per-item step exists to hoist; silence is the signal
            return;
        }
        let build = if is_for { RustLoops::pipeline_build(body) } else { None };
        if let Some(build) = build {
            // tier 1 (coding-standards.md: never offer a fix that cannot
            // apply): the directive appears ONLY when the fixer would
            // apply at this line — same contract, same refusal reasons.
            let fixable = crate::fix::LoopPipeline::fixable(self.state.source, self.state.file, line);
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
                .finding("loop-pipeline", "warn", line, &self.fn_name, message);
        } else if mutated.len() >= 2 {
            self.state.finding(
                "mutating-loop",
                "warn",
                line,
                &self.fn_name,
                format!(
                    "loop mutates {} — Replace Loop with Pipeline: {} pieces of state survive the loop and the changes are invisible at the call site; compute each output from the input instead",
                    mutated.join(", "),
                    mutated.len(),
                ),
            );
        } else if mutated.len() == 1 {
            let n = RustLoops::body_stmt_count(body);
            if n >= HOIST_MIN_STMTS {
                let acc = &mutated[0];
                // tier 1 (coding-standards.md: never offer a fix that cannot
                // apply): the directive appears ONLY when the hoister would
                // apply at this line (placeholder name — the name is never
                // the refusal reason).
                let fixable = crate::fix::LoopHoist::fixable_at(self.state.source, self.state.file, line);
                let message = if fixable {
                    format!(
                        "loop body is {n} statements yet mutates only {acc} — hoist the per-item step into a named helper that returns the value (a domain noun for one item's contribution), then combine or fold — fix: loop-hoist --fix-name <Name>",
                    )
                } else {
                    format!(
                        "loop body is {n} statements yet mutates only {acc} — hoist the per-item step into a named helper that returns the value (a domain noun for one item's contribution), then combine or fold",
                    )
                };
                self.state.finding("loop-hoist", "warn", line, &self.fn_name, message);
            }
        }
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

    /// Pipeline/mutating per loop at any depth; the OUTERMOST loops are
    /// collected for the sequence judgement. Closures and nested items are
    /// new scopes — their loops belong to them, not to this function.
    fn walk_stmts(&mut self, stmts: &[syn::Stmt], in_loop: bool) {
        for s in stmts {
            let syn::Stmt::Expr(e, _) = s else {
                continue;
            };
            let line = s.span().start().line;
            match e {
                syn::Expr::ForLoop(f) => {
                    let mut own = Vec::new();
                    RustLoops::pat_idents(&f.pat, &mut own);
                    let mutated = self.surviving_mutations(&f.body.stmts, &own);
                    let reads: Vec<String> = rustscan::RustShape::expr_idents(&f.expr)
                        .into_iter()
                        .filter(|r| self.survivors.contains(r) && !own.contains(r))
                        .collect();
                    self.judge(line, &f.body.stmts, true, mutated.clone(), &[]);
                    if !in_loop {
                        self.seq.push(SeqLoop { line, reads, mutated });
                    }
                    self.walk_stmts(&f.body.stmts, true);
                }
                syn::Expr::While(w) => {
                    let mutated = self.surviving_mutations(&w.body.stmts, &[]);
                    let reads: Vec<String> = rustscan::RustShape::expr_idents(&w.cond)
                        .into_iter()
                        .filter(|r| self.survivors.contains(r))
                        .collect();
                    let drains = condition_drains(&w.cond, self.survivors);
                    self.judge(line, &w.body.stmts, false, mutated.clone(), &drains);
                    if !in_loop {
                        self.seq.push(SeqLoop { line, reads, mutated });
                    }
                    self.walk_stmts(&w.body.stmts, true);
                }
                syn::Expr::Loop(l) => {
                    let mutated = self.surviving_mutations(&l.body.stmts, &[]);
                    self.judge(line, &l.body.stmts, false, mutated.clone(), &[]);
                    if !in_loop {
                        self.seq.push(SeqLoop {
                            line,
                            reads: Vec::new(),
                            mutated,
                        });
                    }
                    self.walk_stmts(&l.body.stmts, true);
                }
                syn::Expr::If(i) => {
                    self.walk_stmts(&i.then_branch.stmts, in_loop);
                    if let Some((_, els)) = &i.else_branch {
                        self.walk_expr(els, in_loop);
                    }
                }
                syn::Expr::Match(m) => {
                    for arm in &m.arms {
                        self.walk_expr(&arm.body, in_loop);
                    }
                }
                syn::Expr::Block(b) => {
                    self.walk_stmts(&b.block.stmts, in_loop);
                }
                syn::Expr::Unsafe(u) => {
                    self.walk_stmts(&u.block.stmts, in_loop);
                }
                _ => {}
            }
        }
    }

    fn walk_expr(&mut self, e: &syn::Expr, in_loop: bool) {
        match e {
            syn::Expr::If(i) => {
                self.walk_stmts(&i.then_branch.stmts, in_loop);
                if let Some((_, els)) = &i.else_branch {
                    self.walk_expr(els, in_loop);
                }
            }
            syn::Expr::Match(m) => {
                for arm in &m.arms {
                    self.walk_expr(&arm.body, in_loop);
                }
            }
            syn::Expr::Block(b) => {
                self.walk_stmts(&b.block.stmts, in_loop);
            }
            syn::Expr::Unsafe(u) => {
                self.walk_stmts(&u.block.stmts, in_loop);
            }
            syn::Expr::ForLoop(f) => {
                let line = e.span().start().line;
                let mut own = Vec::new();
                RustLoops::pat_idents(&f.pat, &mut own);
                let mutated = self.surviving_mutations(&f.body.stmts, &own);
                self.judge(line, &f.body.stmts, true, mutated, &[]);
                self.walk_stmts(&f.body.stmts, true);
            }
            syn::Expr::While(w) => {
                let line = e.span().start().line;
                let mutated = self.surviving_mutations(&w.body.stmts, &[]);
                let drains = condition_drains(&w.cond, self.survivors);
                self.judge(line, &w.body.stmts, false, mutated, &drains);
                self.walk_stmts(&w.body.stmts, true);
            }
            syn::Expr::Loop(l) => {
                let line = e.span().start().line;
                let mutated = self.surviving_mutations(&l.body.stmts, &[]);
                self.judge(line, &l.body.stmts, false, mutated, &[]);
                self.walk_stmts(&l.body.stmts, true);
            }
            _ => {}
        }
    }

    /// The sequence verdict: loops SHARE a mutated name, or a later loop
    /// READS what an earlier one wrote -> the sequence IS a pipeline of
    /// passes over shared state; independent passes -> one named step per
    /// loop.
    fn emit_sequence(&mut self) {
        if self.seq.len() < 2 {
            return;
        }
        let mut shared: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (i, a) in self.seq.iter().enumerate() {
            for b in self.seq.iter().skip(i + 1) {
                for m in &a.mutated {
                    if b.mutated.contains(m) {
                        shared.insert(m.clone());
                    }
                }
            }
        }
        let mut fed: Vec<String> = Vec::new();
        for (i, a) in self.seq.iter().enumerate() {
            let earlier: std::collections::HashSet<&str> = self
                .seq
                .iter()
                .take(i)
                .flat_map(|s| s.mutated.iter().map(|m| m.as_str()))
                .collect();
            for r in &a.reads {
                if earlier.contains(r.as_str()) && !fed.contains(r) {
                    fed.push(r.clone());
                }
            }
        }
        let lines: Vec<String> = self.seq.iter().map(|l| l.line.to_string()).collect();
        if !shared.is_empty() || !fed.is_empty() {
            let mut names: Vec<String> = shared.into_iter().collect();
            for f in &fed {
                if !names.contains(f) {
                    names.push(f.clone());
                }
            }
            names.sort();
            let line = self.fn_line;
            let name = self.fn_name.clone();
            // tier 1 (coding-standards.md: never offer a fix that cannot
            // apply): the directive appears ONLY when the sequencer would
            // apply at this fn — same contract.
            let directive = if crate::fix::loop_sequence_fixable(self.state.source, self.state.file, line) {
                " — fix: loop-sequence"
            } else {
                ""
            };
            self.state.finding(
                "loop-sequence",
                "warn",
                line,
                &name,
                format!(
                    "{} sequential loops at lines {} share or feed the same state ({}) — the sequence IS a pipeline of passes over shared state: combine the passes or extract each into a named step{}",
                    self.seq.len(),
                    lines.join(", "),
                    names.join(", "),
                    directive,
                ),
            );
        } else {
            let line = self.fn_line;
            let name = self.fn_name.clone();
            self.state.finding(
                "loop-sequence",
                "warn",
                line,
                &name,
                format!(
                    "{} sequential loops at lines {} change the function's state independently — extract each loop into a named step (one named pass per loop), so the function reads as a sequence of steps",
                    self.seq.len(),
                    lines.join(", "),
                ),
            );
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
        ctx.walk_stmts(&stmts, false);
        ctx.emit_sequence();
    }
}
