//! The module-level walk: the plain pipeline shape for module loops, and
//! descent into class bodies (their methods are functions, analysed by their
//! own per-function pass).

use ruff_python_ast::Stmt;
use ruff_text_size::Ranged;

use crate::{finding::Finding, source::Source, ScanState};

use super::function_walk::FunctionWalk;
use super::{Creation, LoopFamily};

pub(super) struct ModuleWalk;

impl ModuleWalk {
    /// Module-level walk: the plain pipeline shape for module loops, and
    /// descent into class bodies (their methods are functions — analyzed by
    /// their own `loop_function_findings` run).
    pub(super) fn walk(state: &mut ScanState, stmts: &[Stmt], source: &str) {
        Self::walk_into(
            state,
            stmts,
            source,
            &mut std::collections::HashMap::new(),
            &mut std::collections::HashMap::new(),
        );
    }

    /// Walk one statement list, descending into every nested body with the
    /// bindings the enclosing scope has recorded so far.
    fn walk_into(
        state: &mut ScanState,
        stmts: &[Stmt],
        source: &str,
        module_lines: &mut std::collections::HashMap<String, usize>,
        module_bindings: &mut std::collections::HashMap<String, bool>,
    ) {
        for (i, s) in stmts.iter().enumerate() {
            match s {
                Stmt::FunctionDef(f) => {
                    FunctionWalk::findings(state, f, module_lines);
                    Self::record_bindings(s, module_lines, module_bindings, source);
                }
                Stmt::For(f) => {
                    // P6: module-level loops — the target must be fresh:
                    // no module binding before the loop except empty
                    // creations, which are this loop's own build.
                    let target = LoopFamily::pipeline_target(&f.body);
                    let fresh = target
                        .as_ref()
                        .is_none_or(|t| !module_bindings.get(t).copied().unwrap_or(false));
                    if fresh && LoopFamily::body_is_pipeline(&f.body) {
                        let fixable = target.as_deref().is_some_and(|acc| {
                            LoopFamily::collection_kind(&f.body).is_some_and(|kind| {
                                LoopFamily::auto_fixable(&f.body)
                                    && LoopFamily::init_is_empty(i.checked_sub(1).and_then(|p| stmts.get(p)), acc, kind)
                            })
                        });
                        let line = Source::line_of(source, f.range().start());
                        state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                    file: state.file.to_string(),
                    line,
                    function: state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default(),
                    kind: "loop-pipeline".into(),
                    severity: "warn".into(),
                    message: if fixable {
                        "loop builds a collection — Replace Loop with Pipeline: use a comprehension — fix: loop-pipeline"
                    } else {
                        "loop builds a collection — Replace Loop with Pipeline: use a comprehension"
                    }
                    .into(), });
                    }
                    let mut inner_lines = module_lines.clone();
                    let mut inner = module_bindings.clone();
                    Self::record_bindings(s, &mut inner_lines, &mut inner, source);
                    Self::walk_into(state, &f.body, source, &mut inner_lines, &mut inner);
                    Self::walk_into(state, &f.orelse, source, &mut inner_lines, &mut inner);
                }
                Stmt::ClassDef(cd) => {
                    Self::record_bindings(s, module_lines, module_bindings, source);
                    Self::walk_into(state, &cd.body, source, module_lines, module_bindings);
                }
                Stmt::While(w) => {
                    Self::record_bindings(s, module_lines, module_bindings, source);
                    Self::walk_into(state, &w.body, source, module_lines, module_bindings);
                    Self::walk_into(state, &w.orelse, source, module_lines, module_bindings);
                }
                Stmt::If(i) => {
                    Self::record_bindings(s, module_lines, module_bindings, source);
                    Self::walk_into(state, &i.body, source, module_lines, module_bindings);
                    for cl in &i.elif_else_clauses {
                        Self::walk_into(state, &cl.body, source, module_lines, module_bindings);
                    }
                }
                Stmt::Try(t) => {
                    Self::record_bindings(s, module_lines, module_bindings, source);
                    Self::walk_into(state, &t.body, source, module_lines, module_bindings);
                    for h in &t.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(eh) = h;
                        Self::walk_into(state, &eh.body, source, module_lines, module_bindings);
                    }
                    Self::walk_into(state, &t.orelse, source, module_lines, module_bindings);
                    Self::walk_into(state, &t.finalbody, source, module_lines, module_bindings);
                }
                Stmt::With(w) => {
                    Self::record_bindings(s, module_lines, module_bindings, source);
                    Self::walk_into(state, &w.body, source, module_lines, module_bindings);
                }
                Stmt::Match(m) => {
                    Self::record_bindings(s, module_lines, module_bindings, source);
                    for case in &m.cases {
                        Self::walk_into(state, &case.body, source, module_lines, module_bindings);
                    }
                }
                _ => {
                    Self::record_bindings(s, module_lines, module_bindings, source);
                }
            }
        }
    }

    /// (P6) Record a module-level statement's bindings: first-binding lines for
    /// the function exclusion, and the empty/non-empty map for module loops.
    fn record_bindings(
        s: &Stmt,
        module_lines: &mut std::collections::HashMap<String, usize>,
        module_bindings: &mut std::collections::HashMap<String, bool>,
        source: &str,
    ) {
        let line = Source::line_of(source, s.range().start());
        let mut names: Vec<(String, Creation)> = Vec::new();
        LoopFamily::binding_names(s, &mut names);
        for (n, creation) in names {
            module_lines.entry(n.clone()).or_insert(line);
            LoopFamily::record_binding(module_bindings, &n, creation);
        }
    }
}
