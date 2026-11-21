//! One delegation candidate: the wrapper method, the chain it forwards
//! through, and the verdict the repo-wide index decides.

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::source::Source;

use super::super::expr_walk::base_name_of;
use super::classes::ModuleClasses;
use super::declaration::ParamShape;

/// One single-statement delegation, with everything the verdict needs and
/// nothing borrowed from the file it came from: the repo-wide pass reads these
/// after every file has been scanned.
/// The length above which a `__name__` is a dunder worth naming (not `__x`).
const DUNDER_NAME_MIN: usize = 4;

#[derive(Clone)]
pub struct MiddleManCandidate {
    pub(crate) rel: String,
    pub(crate) line: usize,
    /// The wrapper method's name — also the interface member the exemption
    /// asks about.
    pub(crate) function: String,
    /// The class the wrapper method belongs to.
    pub(crate) owner: String,
    /// The attribute chain the call delegates through (`self._inner.render` →
    /// ["_inner", "render"]); its last element is the delegated member.
    pub(crate) chain: Vec<String>,
    pub(crate) wrapper: ParamShape,
    /// The call's own shape, pre-read so the repo-wide pass never touches an
    /// AST owned by another file.
    pub(crate) positional_passed: usize,
    pub(crate) keywords: Vec<String>,
}

impl MiddleManCandidate {
    /// Every single-statement `return self.<chain>(...)` in the file, as a
    /// candidate. The owner class is read from the enclosing `class` (the
    /// method's own class — file-local by construction); the CHAIN is resolved
    /// later, repo-wide, because one class per file is the house convention.
    pub(super) fn collect(body: &[Stmt], source: &str) -> Vec<MiddleManCandidate> {
        fn walk(out: &mut Vec<MiddleManCandidate>, stmts: &[Stmt], source: &str, owner: Option<&str>) {
            for s in stmts {
                match s {
                    Stmt::FunctionDef(f) => {
                        if f.body.len() != 1 {
                            continue;
                        }
                        let Stmt::Return(r) = &f.body[0] else {
                            continue;
                        };
                        let Some(v) = &r.value else {
                            continue;
                        };
                        let Some(chain) = MiddleManCandidate::self_call_chain(v.as_ref()) else {
                            continue;
                        };
                        let Some(owner) = owner else {
                            continue;
                        };
                        let Expr::Call(call) = v.as_ref() else {
                            continue;
                        };
                        let mut positional_passed = 0usize;
                        for a in &call.arguments.args {
                            if !matches!(a, Expr::Starred(_)) {
                                positional_passed += 1;
                            }
                        }
                        let keywords: Vec<String> = call
                            .arguments
                            .keywords
                            .iter()
                            .filter_map(|k| k.arg.as_ref().map(|n| n.id.to_string()))
                            .collect();
                        out.push(MiddleManCandidate {
                            rel: String::new(), // filled by the caller (the file's own rel)
                            line: Source::line_of(source, f.name.range().start()),
                            function: f.name.to_string(),
                            owner: owner.to_string(),
                            chain,
                            wrapper: ParamShape::read(f),
                            positional_passed,
                            keywords,
                        });
                    }
                    Stmt::ClassDef(cd) => walk(out, &cd.body, source, Some(cd.name.as_str())),
                    Stmt::For(f) => walk(out, &f.body, source, owner),
                    Stmt::While(w) => walk(out, &w.body, source, owner),
                    Stmt::If(i) => {
                        walk(out, &i.body, source, owner);
                        for cl in &i.elif_else_clauses {
                            walk(out, &cl.body, source, owner);
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&mut out, body, source, None);
        out
    }

    /// The attribute chain a call delegates through: `self.a.b(...)` → ["a", "b"].
    /// None when the call is not rooted at `self`.
    fn self_call_chain(e: &Expr) -> Option<Vec<String>> {
        let Expr::Call(c) = e else {
            return None;
        };
        let Expr::Attribute(a) = c.func.as_ref() else {
            return None;
        };
        if base_name_of(&a.value).as_deref() != Some("self") {
            return None;
        }
        let mut chain = vec![a.attr.to_string()];
        let mut cur = a.value.as_ref();
        while let Expr::Attribute(inner) = cur {
            chain.push(inner.attr.to_string());
            cur = inner.value.as_ref();
        }
        chain.reverse();
        Some(chain)
    }

    /// Does the call fix policy — pass an argument the target does not require
    /// (a keyword the target defaults, or a positional beyond its required
    /// ones)? That IS the wrapper's job, so it is not a middle man. A
    /// `**mapping` keyword is forwarding, not policy (the collector never
    /// records one).
    fn policy_fixes(&self, shape: &ParamShape) -> bool {
        self.positional_passed > shape.positional_required || self.keywords.iter().any(|k| shape.optional.contains(k))
    }

    /// Fire? The chain resolves to a member this codebase declares, the method
    /// is not an operator the language invokes, no interface of the owner's own
    /// is satisfied by it, and no declaration of the target would be
    /// policy-fixed by the call.
    pub(super) fn fires(&self, module: &ModuleClasses) -> bool {
        if Self::is_dunder(&self.function) {
            return false;
        }
        let decls = module.declarations(&self.owner, &self.chain);
        if decls.is_empty() {
            return false;
        }
        if module.implements_interface_member(&self.owner, &self.rel, &self.function, &self.wrapper) {
            return false;
        }
        !decls.iter().any(|shape| self.policy_fixes(shape))
    }

    /// A dunder method (`__add__`, `__len__`, `__enter__`): the language invokes
    /// it, so no call site exists where "call the target directly" can be taken —
    /// the method IS the operator's contract, not a forwarding layer. Delegation
    /// inside it is the implementation, not a removable seam.
    pub(crate) fn is_dunder(name: &str) -> bool {
        name.len() > DUNDER_NAME_MIN && name.starts_with("__") && name.ends_with("__")
    }

    /// The finding this candidate produces — its own coordinates and message.
    pub(super) fn finding(&self) -> Finding {
        Finding {
            logical_start: None,
            seam_members: Vec::new(),
            col: 0,
            file: self.rel.clone(),
            line: self.line,
            function: self.function.clone(),
            kind: "middle-man".into(),
            severity: "warn".into(),
            message: format!(
                "'{}' is a pure delegation to self — Remove Middle Man: call the target directly",
                self.function
            ),
        }
    }
}
