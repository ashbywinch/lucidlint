// lucidlint: ignore-file complexity the syn loop walkers are single dispatch tables — match-arm count,
// not branching; keep NEW functions under cc 15
//! The loop family for Rust — the syn port of the Python Replace Loop with
//!
//! Shapes, in syn terms:
//!
//! - `loop-pipeline`: the body is exactly one collection build —
//!   `acc.push(item)` (plain or behind ONE if-filter, no else) or an
//!   `acc[idx] = v` store — a combinator in disguise: `.map()`/`.filter()`
//!   for a value-accumulator, `.for_each()` where pushing is the whole body.
//! - `mutating-loop`: the body mutates >= 2 pieces of state that SURVIVE the
//!   loop (rebinds/compound-assigns of fn-scope locals, mutating receiver
//!   calls on them) — the changes are invisible at the call site; compute
//!   each output from the input instead.
//! - `loop-hoist`: exactly one surviving mutation but a body of >= 4
//!   statements — the body computes more than it adds; hoist the per-item
//!   step into a named helper that returns the value, then combine or fold.
//! - `loop-sequence`: >= 2 sequential (non-nested) for/while loops in one
//!   function — a hidden pipeline when they share or feed state (a later
//!   loop reads what an earlier one wrote), else separate named steps.
//!
//! While loops count for the stateful verdicts (mutation/sequence) but never
//! for the pipeline shape: a while condition is not an iterator, so no
//! combinator replaces it. `loop {}` bodies are judged the same way (their
//! only honest shape is stateful).
//! Detection AND fix: the messages carry `fix:` directives the gate wires
//! to `fix.rs` (`fix_loop_pipeline`, `fix_loop_sequence`, `fix_loop_hoist`
//! — offer equals fix: review-bot).

use crate::rustscan;
use syn::spanned::Spanned;

/// The minimum flattened body statement count for a single-mutation loop to
/// carry the hoist advice — the body clearly computes more than it adds.
const HOIST_MIN_STMTS: usize = 4;

/// Mutating receiver calls — `acc.<method>(..)` WRITES `acc`. Reads (`len`,
/// `get`, `iter`, `is_empty`, ...) never count: a body that only inspects
/// the accumulator is not mutating it.
pub(crate) const MUTATING_METHODS: &[&str] = &[
    "push",
    "push_str",
    "extend",
    "insert",
    "remove",
    "pop",
    "clear",
    "sort",
    "sort_unstable",
    "truncate",
    "retain",
    "append",
    "drain",
    "put",
    "add",
    "discard",
    "update",
    "setdefault",
    "dedup",
    "resize",
    "swap_remove",
];

/// The idents a pattern binds.
pub(crate) fn pat_idents(pat: &syn::Pat, out: &mut Vec<String>) {
    match pat {
        syn::Pat::Ident(pi) => {
            let name = pi.ident.to_string();
            if !out.contains(&name) {
                out.push(name);
            }
        }
        syn::Pat::Tuple(t) => {
            for e in &t.elems {
                pat_idents(e, out);
            }
        }
        syn::Pat::TupleStruct(t) => {
            for e in &t.elems {
                pat_idents(e, out);
            }
        }
        syn::Pat::Struct(s) => {
            for f in &s.fields {
                pat_idents(&f.pat, out);
            }
        }
        syn::Pat::Slice(s) => {
            for e in &s.elems {
                pat_idents(e, out);
            }
        }
        syn::Pat::Reference(r) => pat_idents(&r.pat, out),
        syn::Pat::Paren(p) => pat_idents(&p.pat, out),
        syn::Pat::Type(t) => pat_idents(&t.pat, out),
        syn::Pat::Or(o) => {
            for c in &o.cases {
                pat_idents(c, out);
            }
        }
        _ => {}
    }
}

/// The base path an assigned expression writes: `x`, `x[idx]`, `x.field`
/// all write `x`. `self.x` and bare `self` belong to the object, not the
/// loop.
pub(crate) fn expr_base_path(e: &syn::Expr, out: &mut Vec<String>) {
    match e {
        syn::Expr::Path(p) => {
            if p.path.segments.len() == 1 {
                let name = p.path.segments[0].ident.to_string();
                if name != "self" && !out.contains(&name) {
                    out.push(name);
                }
            }
        }
        syn::Expr::Index(i) => expr_base_path(&i.expr, out),
        syn::Expr::Field(f) => expr_base_path(&f.base, out),
        syn::Expr::Reference(r) => expr_base_path(&r.expr, out),
        syn::Expr::Paren(p) => expr_base_path(&p.expr, out),
        syn::Expr::Unary(u) => expr_base_path(&u.expr, out),
        _ => {}
    }
}
pub(crate) fn single_push(stmt: &syn::Stmt) -> Option<String> {
    let syn::Stmt::Expr(e, Some(_)) = stmt else {
        return None;
    };
    let syn::Expr::MethodCall(m) = e else {
        return None;
    };
    if m.method != "push" || m.args.len() != 1 {
        return None;
    }
    match m.receiver.as_ref() {
        syn::Expr::Path(p) if p.path.segments.len() == 1 => Some(p.path.segments[0].ident.to_string()),
        _ => None,
    }
}

/// One `acc[idx] = <expr>` store statement — the indexed-build shape.
pub(crate) fn index_store(stmt: &syn::Stmt) -> Option<String> {
    let syn::Stmt::Expr(e, Some(_)) = stmt else {
        return None;
    };
    let syn::Expr::Assign(a) = e else {
        return None;
    };
    match a.left.as_ref() {
        syn::Expr::Index(i) => match i.expr.as_ref() {
            syn::Expr::Path(p) if p.path.segments.len() == 1 => Some(p.path.segments[0].ident.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// A loop whose body is exactly one collection build — one push or one
pub(crate) fn body_is_pipeline(stmts: &[syn::Stmt]) -> bool {
    match stmts {
        [s] if single_push(s).is_some() || index_store(s).is_some() => true,
        [syn::Stmt::Expr(syn::Expr::If(i), _)]
            if i.else_branch.is_none()
                && i.then_branch.stmts.len() == 1
                && (single_push(&i.then_branch.stmts[0]).is_some()
                    || index_store(&i.then_branch.stmts[0]).is_some()) =>
        {
            true
        }
        _ => false,
    }
}
/// The control-flow children of a statement — nested item scopes excluded.
fn stmt_children(s: &syn::Stmt) -> Vec<&syn::Stmt> {
    match s {
        syn::Stmt::Expr(e, _) => match e {
            syn::Expr::If(i) => {
                let mut out: Vec<&syn::Stmt> = i.then_branch.stmts.iter().collect();
                if let Some((_, els)) = &i.else_branch {
                    if let syn::Expr::Block(b) = els.as_ref() {
                        out.extend(b.block.stmts.iter());
                    }
                }
                out
            }
            syn::Expr::Match(m) => {
                let mut out = Vec::new();
                for arm in &m.arms {
                    arm_body_stmts(&arm.body, &mut out);
                }
                out
            }
            syn::Expr::Block(b) => b.block.stmts.iter().collect(),
            syn::Expr::ForLoop(f) => f.body.stmts.iter().collect(),
            syn::Expr::While(w) => w.body.stmts.iter().collect(),
            syn::Expr::Loop(l) => l.body.stmts.iter().collect(),
            syn::Expr::Unsafe(u) => u.block.stmts.iter().collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The statement lists a match arm's body contributes — a block body walks
/// directly; a nested if/match expression contributes its own children.
fn arm_body_stmts<'s>(body: &'s syn::Expr, out: &mut Vec<&'s syn::Stmt>) {
    match body {
        syn::Expr::Block(b) => out.extend(b.block.stmts.iter()),
        syn::Expr::If(i) => {
            out.extend(i.then_branch.stmts.iter());
            if let Some((_, els)) = &i.else_branch {
                if let syn::Expr::Block(b) = els.as_ref() {
                    out.extend(b.block.stmts.iter());
                }
            }
        }
        syn::Expr::Match(m) => {
            for arm in &m.arms {
                arm_body_stmts(&arm.body, out);
            }
        }
        _ => {}
    }
}

/// The flattened statement count of a loop body — every control-flow child
/// counts, nested items are not the body's business.
fn body_stmt_count(stmts: &[syn::Stmt]) -> usize {
    fn walk(stmts: &[syn::Stmt], n: &mut usize) {
        for s in stmts {
            if matches!(s, syn::Stmt::Item(_)) {
                continue;
            }
            *n += 1;
            for child in stmt_children(s) {
                walk(std::slice::from_ref(child), n);
            }
        }
    }
    let mut n = 0;
    walk(stmts, &mut n);
    n
}

/// The names one statement WRITES — rebinds, compound-assigns, indexed and
/// field stores (via their base path), and mutating receiver calls. `let`
/// bindings are new names, not writes; nested items are new scopes.
fn stmt_writes(stmt: &syn::Stmt, out: &mut Vec<String>) {
    let syn::Stmt::Expr(e, _) = stmt else {
        return;
    };
    match e {
        syn::Expr::Assign(a) => expr_base_path(&a.left, out),
        syn::Expr::Binary(b) if rustscan::is_compound_op(&b.op) => {
            expr_base_path(&b.left, out);
        }
        syn::Expr::MethodCall(m) => {
            if MUTATING_METHODS.contains(&m.method.to_string().as_str()) {
                if let syn::Expr::Path(p) = m.receiver.as_ref() {
                    if p.path.segments.len() == 1 {
                        let name = p.path.segments[0].ident.to_string();
                        if name != "self" && !out.contains(&name) {
                            out.push(name);
                        }
                    }
                }
            }
        }
        syn::Expr::If(i) => {
            for s in &i.then_branch.stmts {
                stmt_writes(s, out);
            }
            if let Some((_, els)) = &i.else_branch {
                if let syn::Expr::Block(b) = els.as_ref() {
                    for s in &b.block.stmts {
                        stmt_writes(s, out);
                    }
                }
            }
        }
        syn::Expr::Match(m) => {
            for arm in &m.arms {
                let mut kids = Vec::new();
                arm_body_stmts(&arm.body, &mut kids);
                for s in kids {
                    stmt_writes(s, out);
                }
            }
        }
        syn::Expr::Block(b) => {
            for s in &b.block.stmts {
                stmt_writes(s, out);
            }
        }
        syn::Expr::Unsafe(u) => {
            for s in &u.block.stmts {
                stmt_writes(s, out);
            }
        }
        syn::Expr::ForLoop(f) => {
            for s in &f.body.stmts {
                stmt_writes(s, out);
            }
        }
        syn::Expr::While(w) => {
            for s in &w.body.stmts {
                stmt_writes(s, out);
            }
        }
        syn::Expr::Loop(l) => {
            for s in &l.body.stmts {
                stmt_writes(s, out);
            }
        }
        _ => {}
    }
}

/// One outermost loop's shape, for the sequence judgement.
struct SeqLoop {
    line: usize,
    mutated: Vec<String>,
    reads: Vec<String>,
}

/// The per-function loop pass — a flat struct holds the walk's state, so
/// the walker is a method instead of closures.
struct LoopFnCtx<'a, 'b> {
    state: &'a mut rustscan::RsState<'b>,
    survivors: &'a std::collections::HashSet<String>,
    fn_name: String,
    fn_line: usize,
    seq: Vec<SeqLoop>,
}

impl<'a, 'b> LoopFnCtx<'a, 'b> {
    fn judge(&mut self, line: usize, body: &[syn::Stmt], is_for: bool, mutated: Vec<String>) {
        if is_for && body_is_pipeline(body) {
            // tier 1 (coding-standards.md: never offer a fix that cannot
            // apply): the directive appears ONLY when the fixer would
            // apply at this line — same contract, same refusal reasons.
            let fixable = crate::fix::loop_pipeline_fixable(self.state.source, self.state.file, line);
            let message = if fixable {
                "loop builds a collection — Replace Loop with Pipeline: use a combinator (.map()/.filter() to build the value, .for_each() where pushing is the whole body) — fix: loop-pipeline"
            } else {
                "loop builds a collection — Replace Loop with Pipeline: use a combinator (.map()/.filter() to build the value, .for_each() where pushing is the whole body)"
            };
            self.state
                .finding("loop-pipeline", "warn", line, &self.fn_name, message.into());
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
            let n = body_stmt_count(body);
            if n >= HOIST_MIN_STMTS {
                let acc = &mutated[0];
                // tier 1 (coding-standards.md: never offer a fix that cannot
                // apply): the directive appears ONLY when the hoister would
                // apply at this line (placeholder name — the name is never
                // the refusal reason).
                let fixable = crate::fix::loop_hoist_fixable_at(self.state.source, self.state.file, line);
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
            stmt_writes(s, &mut writes);
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
                    pat_idents(&f.pat, &mut own);
                    let mutated = self.surviving_mutations(&f.body.stmts, &own);
                    let reads: Vec<String> = rustscan::rust_expr_idents(&f.expr)
                        .into_iter()
                        .filter(|r| self.survivors.contains(r) && !own.contains(r))
                        .collect();
                    self.judge(line, &f.body.stmts, true, mutated.clone());
                    if !in_loop {
                        self.seq.push(SeqLoop { line, reads, mutated });
                    }
                    self.walk_stmts(&f.body.stmts, true);
                }
                syn::Expr::While(w) => {
                    let mutated = self.surviving_mutations(&w.body.stmts, &[]);
                    let reads: Vec<String> = rustscan::rust_expr_idents(&w.cond)
                        .into_iter()
                        .filter(|r| self.survivors.contains(r))
                        .collect();
                    self.judge(line, &w.body.stmts, false, mutated.clone());
                    if !in_loop {
                        self.seq.push(SeqLoop { line, reads, mutated });
                    }
                    self.walk_stmts(&w.body.stmts, true);
                }
                syn::Expr::Loop(l) => {
                    let mutated = self.surviving_mutations(&l.body.stmts, &[]);
                    self.judge(line, &l.body.stmts, false, mutated.clone());
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
                pat_idents(&f.pat, &mut own);
                let mutated = self.surviving_mutations(&f.body.stmts, &own);
                self.judge(line, &f.body.stmts, true, mutated);
                self.walk_stmts(&f.body.stmts, true);
            }
            syn::Expr::While(w) => {
                let line = e.span().start().line;
                let mutated = self.surviving_mutations(&w.body.stmts, &[]);
                self.judge(line, &w.body.stmts, false, mutated);
                self.walk_stmts(&w.body.stmts, true);
            }
            syn::Expr::Loop(l) => {
                let line = e.span().start().line;
                let mutated = self.surviving_mutations(&l.body.stmts, &[]);
                self.judge(line, &l.body.stmts, false, mutated);
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

/// The per-function pass: pipeline/mutating/hoist per loop at any depth,
/// loop-sequence for the function's OUTERMOST loops.
pub(crate) fn rust_loop_findings(
    state: &mut rustscan::RsState,
    sig: &syn::Signature,
    block: &syn::Block,
    fn_name: &str,
    fn_line: usize,
) {
    let mut survivors = rustscan::param_names(sig);
    bind_fn_stmts(&block.stmts, &mut survivors);
    let mut ctx = LoopFnCtx {
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

/// Fn-scope locals bound OUTSIDE every loop body — the state a loop can
/// survive: `let` bindings at the top level plus non-loop control-flow
/// branches. Loop bodies are NOT descended and loop patterns are NOT
/// bound: an iteration variable or a body-local temp is not state that
/// outlives a pass.
fn bind_fn_stmts(stmts: &[syn::Stmt], out: &mut std::collections::HashSet<String>) {
    for s in stmts {
        match s {
            syn::Stmt::Local(l) => {
                let mut v = Vec::new();
                pat_idents(&l.pat, &mut v);
                out.extend(v);
            }
            syn::Stmt::Expr(e, _) => match e {
                syn::Expr::ForLoop(_) | syn::Expr::While(_) | syn::Expr::Loop(_) => {}
                syn::Expr::If(i) => {
                    bind_fn_stmts(&i.then_branch.stmts, out);
                    if let Some((_, els)) = &i.else_branch {
                        bind_fn_expr(els, out);
                    }
                }
                syn::Expr::Match(m) => {
                    for arm in &m.arms {
                        bind_fn_expr(&arm.body, out);
                    }
                }
                syn::Expr::Block(b) => bind_fn_stmts(&b.block.stmts, out),
                syn::Expr::Unsafe(u) => bind_fn_stmts(&u.block.stmts, out),
                _ => {}
            },
            _ => {}
        }
    }
}

fn bind_fn_expr(e: &syn::Expr, out: &mut std::collections::HashSet<String>) {
    match e {
        syn::Expr::If(i) => {
            bind_fn_stmts(&i.then_branch.stmts, out);
            if let Some((_, els)) = &i.else_branch {
                bind_fn_expr(els, out);
            }
        }
        syn::Expr::Match(m) => {
            for arm in &m.arms {
                bind_fn_expr(&arm.body, out);
            }
        }
        syn::Expr::Block(b) => bind_fn_stmts(&b.block.stmts, out),
        syn::Expr::Unsafe(u) => bind_fn_stmts(&u.block.stmts, out),
        _ => {}
    }
}
