//! `feature-envy`, free-function form: a module-level function that reads an
//! imported project class's fields and has no state of its own. The repo-wide
//! merge resolves the class against the repo's definitions, so a `Path` or
//! `Request` is never named as a home.

use std::collections::HashSet;

use ruff_python_ast::visitor::source_order::{walk_expr, SourceOrderVisitor};
use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::{finding::Finding, source::Source, ScanState};

use super::class_facts::annotation_base_name;
use super::expr_walk::{push_stmt_children, stmt_exprs};
use super::latent_state::READS_MIN;

/// One free function that reads an imported class: the class, the read count,
/// and where the function is.
#[derive(Clone)]
pub struct EnvyCandidate {
    pub class: String,
    pub line: usize,
    pub function: String,
    pub reads: usize,
}

impl EnvyCandidate {
    /// Collect the candidates in one module. The repo-wide pass resolves the
    /// class against the repo's definitions; this pass only records the reads.
    pub(crate) fn collect(state: &mut ScanState, body: &[Stmt]) {
        let module_classes: HashSet<String> = body
            .iter()
            .filter_map(|s| match s {
                Stmt::ClassDef(c) => Some(c.name.to_string()),
                _ => None,
            })
            .collect();
        let source = state.source;
        let candidates: Vec<Self> = body
            .iter()
            .filter_map(|s| match s {
                Stmt::FunctionDef(f) => Self::from_function(f, &module_classes, source),
                _ => None,
            })
            .collect();
        state.free_envy.extend(candidates);
    }

    /// One candidate from one module function, when it is the shape: one
    /// parameter, a class this module does not define, four reads, no bare use.
    fn from_function(
        f: &ruff_python_ast::StmtFunctionDef,
        module_classes: &HashSet<String>,
        source: &str,
    ) -> Option<Self> {
        if !f.decorator_list.is_empty() || !Self::takes_one_parameter(f) {
            return None;
        }
        let param = f.parameters.posonlyargs.first().or_else(|| f.parameters.args.first())?;
        let annotation = param.parameter.annotation.as_deref()?;
        let class = annotation_base_name(annotation)?;
        // the same-module class is `misplaced-method`'s direct form
        if module_classes.contains(&class) {
            return None;
        }
        let (reads, bare) = Self::receiver_use(&f.body, param.parameter.name.as_str());
        if reads < READS_MIN || bare {
            return None;
        }
        Some(Self {
            class,
            line: Source::line_of(source, f.name.range().start()),
            function: f.name.to_string(),
            reads,
        })
    }

    /// One finding per function whose class the repo defines. The class is the
    /// destination; the function name is the method name.
    pub(crate) fn findings(scans: &[(String, Vec<EnvyCandidate>)], classes: &HashSet<String>) -> Vec<Finding> {
        scans
            .iter()
            .flat_map(|(rel, candidates)| {
                candidates
                    .iter()
                    .filter(|candidate| classes.contains(&candidate.class))
                    .map(move |candidate| Finding {
                        logical_start: None,
                        seam_members: Vec::new(),
                        file: rel.clone(),
                        line: candidate.line,
                        col: 0,
                        function: candidate.function.clone(),
                        kind: "feature-envy".into(),
                        severity: "fail".into(),
                        message: format!(
                            "'{}' reads '{}' fields {} times and has no state of its own; move the computation onto '{}' as the method '{}'",
                            candidate.function, candidate.class, candidate.reads, candidate.class, candidate.function
                        ),
                    })
            })
            .collect()
    }

    /// A free function with more than one input is a service over the object,
    /// not a derivation of it (the same boundary as `misplaced-method`).
    fn takes_one_parameter(f: &ruff_python_ast::StmtFunctionDef) -> bool {
        f.parameters.posonlyargs.len() + f.parameters.args.len() == 1
            && f.parameters.kwonlyargs.is_empty()
            && f.parameters.vararg.is_none()
            && f.parameters.kwarg.is_none()
    }

    /// How a function body uses one name: reads through the name's attributes,
    /// and whether the name is also used as a value (passed, returned,
    /// assigned).
    fn receiver_use(body: &[Stmt], param: &str) -> (usize, bool) {
        struct Use<'a> {
            param: &'a str,
            reads: usize,
            bare: bool,
        }
        impl<'a> SourceOrderVisitor<'a> for Use<'a> {
            fn visit_expr(&mut self, expr: &'a Expr) {
                if let Expr::Attribute(a) = expr {
                    if matches!(a.value.as_ref(), Expr::Name(n) if n.id.as_str() == self.param) {
                        self.reads += 1;
                        return;
                    }
                }
                if matches!(expr, Expr::Name(n) if n.id.as_str() == self.param) {
                    self.bare = true;
                }
                walk_expr(self, expr);
            }
        }
        let mut usage = Use {
            param,
            reads: 0,
            bare: false,
        };
        let mut stack: Vec<&Stmt> = body.iter().collect();
        while let Some(st) = stack.pop() {
            if matches!(st, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
                continue;
            }
            for expr in stmt_exprs(st) {
                usage.visit_expr(expr);
            }
            push_stmt_children(st, &mut stack);
        }
        (usage.reads, usage.bare)
    }
}
