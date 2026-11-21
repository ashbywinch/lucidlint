//! One module-defined class, as the middle-man rule reads it: the module it is
//! declared in, whether it is an interface, the methods it declares (with the
//! parameter shape the policy check reads), its bases, and which class each
//! `self.<attr>` is built from.

use std::collections::{HashMap, HashSet};

use ruff_python_ast::{Expr, Stmt, StmtClassDef};

use super::super::class_facts::{class_methods, has_decorator};
use super::super::expr_walk::base_name_of;
use super::super::{annotation_base_name, push_stmt_children};
use super::declaration::ParamShape;

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
    pub(crate) methods: HashMap<String, ParamShape>,
    pub(crate) bases: Vec<String>,
    pub(crate) attrs: HashMap<String, String>,
}

impl MiddleManClass {
    /// The base names of `class X(A, B[T])` — a subscripted base is the base it
    /// wraps (`Protocol[T]` is a Protocol).
    pub(crate) fn base_names(c: &StmtClassDef) -> Vec<String> {
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

    /// The name in `class X(metaclass=ABCMeta)`. The metaclass is how an
    /// interface is spelled without a base — `class X(ABC)` and
    /// `class X(metaclass=ABCMeta)` declare the same thing — so the interface
    /// check reads both. None when the class names no metaclass.
    pub(super) fn metaclass_name(c: &StmtClassDef) -> Option<String> {
        let args = c.arguments.as_ref()?;
        args.keywords
            .iter()
            .find(|k| k.arg.as_ref().is_some_and(|n| n.id.as_str() == "metaclass"))
            .and_then(|k| annotation_base_name(&k.value))
    }

    /// Which module class each `self.<attr>` is built from: an annotated
    /// attribute (a class-body field or `self.x: T` in a method) or a
    /// constructor call (`self.x = T(...)`). An attribute with no such evidence
    /// resolves to nothing — as far as this rule can tell, its target is not
    /// in the scanned codebase.
    pub(super) fn attribute_classes(c: &StmtClassDef) -> HashMap<String, String> {
        let mut out = HashMap::new();
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

    /// The member's parameter shape when this class declares it.
    pub(super) fn declares(&self, member: &str) -> Option<&ParamShape> {
        self.methods.get(member)
    }

    /// The interface members this class declares: the name and the parameter
    /// names a conforming implementation must accept. Empty when the class is
    /// not an interface.
    pub(super) fn interface_declarations(&self) -> Vec<(&str, &HashSet<String>)> {
        if !self.is_interface {
            return Vec::new();
        }
        self.methods.iter().map(|(n, s)| (n.as_str(), &s.names)).collect()
    }

    /// Read one class from its syntax, in `module`'s file.
    pub(super) fn read(c: &StmtClassDef, module: &str) -> Self {
        let methods = class_methods(c);
        let is_interface = methods.iter().any(|f| has_decorator(f, "abstractmethod"))
            || Self::base_names(c)
                .iter()
                .any(|b| b == "ABC" || b == "ABCMeta" || b == "Protocol")
            || Self::metaclass_name(c).as_deref() == Some("ABCMeta");
        MiddleManClass {
            name: c.name.to_string(),
            module: module.to_string(),
            is_interface,
            methods: methods
                .iter()
                .map(|f| (f.name.to_string(), ParamShape::read(f)))
                .collect(),
            bases: Self::base_names(c),
            attrs: Self::attribute_classes(c),
        }
    }
}
