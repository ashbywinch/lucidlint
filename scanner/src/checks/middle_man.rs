//! `middle-man`: a class that only forwards to another — the class in the
//! middle adds a hop, not a concept.
// lucidlint: ignore-file record-shape `MiddleManCandidate` is the repo-wide pass's
// intermediate record: it carries one delegation site between files so the pass can
// resolve the target's class, and it owns no rules of its own

use std::collections::HashSet;

use ruff_python_ast::{Expr, Stmt, StmtClassDef, StmtFunctionDef};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::{line_of, ScanState};

use super::advisory::base_name_of;
use super::{annotation_base_name, class_methods, has_decorator, push_stmt_children};

/// One module-defined class, as the middle-man rule reads it: the module it is
/// declared in, whether it is an interface, the methods it declares (with the
/// parameter shape the policy check reads), its bases, and which class each
/// `self.<attr>` is built from.
///
/// Carried to the repo-wide pass: one class per file is the house convention,
/// so the target of a delegation usually lives in ANOTHER file — a file-local
/// index would read almost every real target as "outside the codebase".
#[derive(Clone)]
pub struct MiddleManClass {
    pub(crate) name: String,
    pub(crate) module: String,
    pub(crate) is_interface: bool,
    pub(crate) methods: std::collections::HashMap<String, ParamShape>,
    pub(crate) bases: Vec<String>,
    pub(crate) attrs: std::collections::HashMap<String, String>,
}

/// One member an interface declares: which interface declares it, the module
/// that interface lives in, and the parameter names a conforming
/// implementation must accept. The interface and the module are the evidence
/// that the declaration is about a particular wrapper (issue #65) — a name
/// alone is not.
#[derive(Clone)]
pub(super) struct InterfaceDeclaration {
    pub(crate) interface: String,
    pub(crate) module: String,
    pub(crate) names: HashSet<String>,
}

/// A method's parameter shape: how many positionals a caller must pass, which
/// names carry a default (the policy check reads that), and every
/// non-receiver name (the interface-conformance comparison reads that).
#[derive(Default, Clone)]
pub(crate) struct ParamShape {
    pub(crate) positional_required: usize,
    pub(crate) optional: HashSet<String>,
    pub(crate) names: HashSet<String>,
}

/// One single-statement delegation, with everything the verdict needs and
/// nothing borrowed from the file it came from: the repo-wide pass reads these
/// after every file has been scanned.
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

/// The receiver is skipped for the positional count (a `@staticmethod` has
/// none), so the count is what a CALLER must supply.
pub(super) fn param_shape(f: &StmtFunctionDef) -> ParamShape {
    let mut shape = ParamShape::default();
    let skip_receiver = !has_decorator(f, "staticmethod");
    let mut idx = 0usize;
    for pwd in f.parameters.posonlyargs.iter().chain(&f.parameters.args) {
        idx += 1;
        if idx == 1 && skip_receiver {
            continue;
        }
        let name = pwd.parameter.name.to_string();
        if pwd.default.is_some() {
            shape.optional.insert(name.clone());
        } else {
            shape.positional_required += 1;
        }
        shape.names.insert(name);
    }
    for k in &f.parameters.kwonlyargs {
        let name = k.parameter.name.to_string();
        if k.default.is_some() {
            shape.optional.insert(name.clone());
        }
        shape.names.insert(name);
    }
    if let Some(v) = &f.parameters.vararg {
        shape.names.insert(v.name.to_string());
    }
    if let Some(k) = &f.parameters.kwarg {
        shape.names.insert(k.name.to_string());
    }
    shape
}

/// The base names of `class X(A, B[T])` — a subscripted base is the base it
/// wraps (`Protocol[T]` is a Protocol).
pub(super) fn class_base_names(c: &StmtClassDef) -> Vec<String> {
    let Some(args) = &c.arguments else {
        return Vec::new();
    };
    args.args
        .iter()
        .filter_map(|b| match b {
            Expr::Subscript(s) => annotation_base_name(&s.value),
            _ => annotation_base_name(b),
        })
        .collect()
}

/// The name in `class X(metaclass=ABCMeta)`. The metaclass is how an interface
/// is spelled without a base — `class X(ABC)` and `class X(metaclass=ABCMeta)`
/// declare the same thing — so the interface check reads both. None when the
/// class names no metaclass.
pub(super) fn class_metaclass_name(c: &StmtClassDef) -> Option<String> {
    let args = c.arguments.as_ref()?;
    args.keywords
        .iter()
        .find(|k| k.arg.as_ref().is_some_and(|n| n.id.as_str() == "metaclass"))
        .and_then(|k| annotation_base_name(&k.value))
}

/// Which module class each `self.<attr>` is built from: an annotated attribute
/// (a class-body field or `self.x: T` in a method) or a constructor call
/// (`self.x = T(...)`). An attribute with no such evidence resolves to
/// nothing — as far as this rule can tell, its target is not in the scanned
/// codebase.
pub(super) fn class_attribute_classes(c: &StmtClassDef) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for m in &c.body {
        if let Stmt::AnnAssign(a) = m {
            if let (Expr::Name(n), Some(ty)) = (a.target.as_ref(), annotation_base_name(a.annotation.as_ref())) {
                out.insert(n.id.to_string(), ty);
            }
        }
    }
    for f in class_methods(c) {
        let mut stack: Vec<&Stmt> = f.body.iter().collect();
        while let Some(s) = stack.pop() {
            match s {
                Stmt::AnnAssign(a) => {
                    if let (Expr::Attribute(at), Some(ty)) =
                        (a.target.as_ref(), annotation_base_name(a.annotation.as_ref()))
                    {
                        if matches!(at.value.as_ref(), Expr::Name(n) if n.id.as_str() == "self") {
                            out.insert(at.attr.to_string(), ty);
                        }
                    }
                }
                Stmt::Assign(a) if a.targets.len() == 1 => {
                    if let (Expr::Attribute(at), Expr::Call(call)) = (&a.targets[0], a.value.as_ref()) {
                        if matches!(at.value.as_ref(), Expr::Name(n) if n.id.as_str() == "self") {
                            if let Some(cls) = base_name_of(&call.func) {
                                out.insert(at.attr.to_string(), cls);
                            }
                        }
                    }
                }
                _ => {}
            }
            push_stmt_children(s, &mut stack);
        }
    }
    out
}

/// The file's classes plus the queries the rule asks of them: one home for the
/// resolution all three exemptions share.
pub(super) struct ModuleClasses {
    pub(crate) classes: std::collections::HashMap<String, MiddleManClass>,
    /// member name → the declarations that require it (per declaration — an
    /// implementation conforms to at least one).
    pub(crate) interface_members: std::collections::HashMap<String, Vec<InterfaceDeclaration>>,
}

/// A dunder method (`__add__`, `__len__`, `__enter__`): the language invokes
/// it, so no call site exists where "call the target directly" can be taken —
/// the method IS the operator's contract, not a forwarding layer. Delegation
/// inside it is the implementation, not a removable seam.
pub(super) fn is_dunder(name: &str) -> bool {
    name.len() > 4 && name.starts_with("__") && name.ends_with("__")
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

/// Every single-statement `return self.<chain>(...)` in the file, as a
/// candidate. The owner class is read from the enclosing `class` (the method's
/// own class — file-local by construction); the CHAIN is resolved later,
/// repo-wide, because one class per file is the house convention.
fn middle_man_candidates(body: &[Stmt], source: &str) -> Vec<MiddleManCandidate> {
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
                    let Some(chain) = self_call_chain(v.as_ref()) else {
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
                        line: line_of(source, f.name.range().start()),
                        function: f.name.to_string(),
                        owner: owner.to_string(),
                        chain,
                        wrapper: param_shape(f),
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

/// Remove Middle Man: a method whose body is a single delegation call
/// (`return self.x.y(...)` / `return self.y(...)`) — it only forwards.
///
/// Not a middle man when the advice cannot be taken (issue #44): the
/// delegated member is not declared anywhere in the scanned codebase (a
/// library's method — the wrapper is where that library meets our interface),
/// the method is a member of an interface the codebase declares (callers are
/// typed against it), or the call fixes an argument the target defaults (the
/// policy lives in the named method).
///
/// Runs over the whole repo's candidates and classes: a wrapper's target is
/// usually a class in another file (one class per file), so a file-local index
/// would read almost every real target as outside the codebase.
pub fn middle_man_findings(candidates: &[MiddleManCandidate], classes: &[MiddleManClass]) -> Vec<Finding> {
    let module = ModuleClasses::merge(classes);
    candidates
        .iter()
        .filter(|cand| cand.fires(&module))
        .map(MiddleManCandidate::finding)
        .collect()
}

/// Per-file collection for the repo-wide pass: the file's classes and its
/// delegation candidates. A single-buffer scan (`--file`, the LSP's typing
/// path) never runs a repo-wide pass, so it decides here against the file's
/// own classes — the conservative reading, since a target in another file
/// cannot be seen from one buffer.
pub fn collect_middle_man(state: &mut ScanState, body: &[Stmt], source: &str, repo_wide: bool) {
    let module = ModuleClasses::read(body, state.file);
    let mut candidates = middle_man_candidates(body, source);
    for c in &mut candidates {
        c.rel = state.file.to_string();
    }
    if !repo_wide {
        let classes: Vec<MiddleManClass> = module.classes.values().cloned().collect();
        state.findings.extend(middle_man_findings(&candidates, &classes));
    }
    state.middle_man_classes.extend(module.classes.into_values());
    state.middle_man_candidates.extend(candidates);
}
