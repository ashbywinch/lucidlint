//! The parameter shape a member declares, and one interface declaration.

use std::collections::HashSet;

use ruff_python_ast::StmtFunctionDef;

use super::super::class_facts::has_decorator;

/// A method's parameter shape: how many positionals a caller must pass, which
/// names carry a default (the policy check reads that), and every
/// non-receiver name (the interface-conformance comparison reads that).
#[derive(Default, Clone)]
pub(crate) struct ParamShape {
    pub(crate) positional_required: usize,
    pub(crate) optional: HashSet<String>,
    pub(crate) names: HashSet<String>,
}

impl ParamShape {
    /// The receiver is skipped for the positional count (a `@staticmethod` has
    /// none), so the count is what a CALLER must supply.
    pub(super) fn read(f: &StmtFunctionDef) -> ParamShape {
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
