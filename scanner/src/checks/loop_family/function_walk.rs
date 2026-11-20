//! The per-function loop pass: the pipeline shape for loops at any depth, and
//! the binding facts that decide whether the loop builds its own collection.

use ruff_python_ast::{Stmt, StmtFunctionDef};
use ruff_text_size::Ranged;

use crate::{finding::Finding, source::Source, ScanState};

use super::LoopFamily;

/// The per-function loop pass — a flat struct holds the walk's state, so
/// the walker is a method instead of closures. `'a` is the ctx's own
/// borrow lifetime (the function's name), `'b` ScanState's inner source
/// lifetime — independent: the local dies with the function pass, the
/// ScanState outlives it.
pub(super) struct FunctionWalk<'a, 'b> {
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

impl<'a, 'b> FunctionWalk<'a, 'b> {
    /// The per-function pass: the pipeline shape for loops at any depth.
    /// `module_bindings` maps every module-level name to its first binding
    /// line — the "bound before the function" exclusion (P6). An async
    /// function's loops emit nothing: the comprehension's awaits belong inside
    /// an async comprehension, which is a different, rarer rewrite the recipe
    /// does not prove.
    pub(super) fn findings(
        state: &'a mut ScanState<'b>,
        f: &StmtFunctionDef,
        module_bindings: &'a std::collections::HashMap<String, usize>,
    ) {
        if f.is_async {
            return;
        }
        let fn_name = f.name.to_string();
        let mut param_names: Vec<String> = f
            .parameters
            .posonlyargs
            .iter()
            .chain(&f.parameters.args)
            .chain(&f.parameters.kwonlyargs)
            .map(|p| p.parameter.name.to_string())
            .collect();
        if let Some(v) = &f.parameters.vararg {
            param_names.push(v.name.to_string());
        }
        if let Some(k) = &f.parameters.kwarg {
            param_names.push(k.name.to_string());
        }
        let fn_line = Source::line_of(state.source, f.range().start());
        let mut walk = FunctionWalk {
            state,
            fn_name: &fn_name,
            module_bindings,
            fn_line,
            param_names,
        };
        walk.walk_loops(&f.body);
    }

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
        for (i, s) in stmts.iter().enumerate() {
            match s {
                Stmt::For(f) => {
                    let line = Source::line_of(self.state.source, f.range().start());
                    // P6: the pipeline gate checks the TARGET's binding — a
                    // loop mutating a pre-existing collection (parameter,
                    // name bound earlier, module name) builds nothing new
                    // and emits nothing. Only a fresh target or one whose
                    // every prior binding is an empty `= {}` / `= []`
                    // creation is this loop's own build.
                    let target = LoopFamily::pipeline_target(&f.body);
                    let fresh = target
                        .as_ref()
                        .is_none_or(|t| !bindings.get(t).copied().unwrap_or(false));
                    if fresh && LoopFamily::body_is_pipeline(&f.body) {
                        // the directive is a promise about the fix engine:
                        // the same shape and the same adjacent empty init
                        // `_LoopsIntoComprehensions` requires, or no command
                        // the directive is a promise about the fix engine
                        let previous = i.checked_sub(1).and_then(|p| stmts.get(p));
                        let fixable = target
                            .as_deref()
                            .is_some_and(|acc| LoopFamily::directive_applies(previous, &f.body, acc));
                        let message = if fixable {
                            "loop builds a collection — Replace Loop with Pipeline: use a comprehension — fix: loop-pipeline"
                        } else {
                            "loop builds a collection — Replace Loop with Pipeline: use a comprehension"
                        };
                        self.push_finding(line, "loop-pipeline", message.into());
                    }
                    // the loop target itself binds (for later loops) —
                    // nested bodies get a clone so sibling bindings stay out
                    let mut inner = bindings.clone();
                    LoopFamily::collect_bindings(s, &mut inner);
                    self.walk_loops_from(&f.body, &mut inner);
                    self.walk_loops_from(&f.orelse, &mut inner);
                }
                Stmt::While(w) => {
                    LoopFamily::collect_bindings(s, bindings);
                    self.walk_loops_from(&w.body, bindings);
                    self.walk_loops_from(&w.orelse, bindings);
                }
                Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {
                    LoopFamily::collect_bindings(s, bindings);
                }
                _ => {
                    LoopFamily::collect_bindings(s, bindings);
                    for body in LoopFamily::stmt_bodies(s) {
                        self.walk_loops_from(body, bindings);
                    }
                }
            }
        }
    }
}
