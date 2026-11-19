//! `static-husk`: a class whose members are all staticmethods and which holds no
//! state of its own — a namespace wearing a domain noun.

use ruff_python_ast::{Expr, Stmt, StmtClassDef};

use crate::finding::Finding;
use crate::{stmt_line, ScanState};

use super::class_facts::{annotation_base_name, class_methods, has_decorator};
use super::expr_walk::push_stmt_children;
use super::function_facts::FunctionFacts;

/// `static-husk`: the rule object. Its methods read one class; the evidence and the message live together.
pub struct StaticHusk;

impl StaticHusk {
    fn is_self_attr_expr(e: &Expr) -> bool {
        matches!(e, Expr::Attribute(a) if matches!(a.value.as_ref(), Expr::Name(n) if n.id.as_str() == "self"))
    }

    /// Does any statement in `body` write `self.<attr>` (assign/annassign/
    /// augassign)? The static-husk rule's "no state of its own" test.
    fn body_writes_self_attr(body: &[Stmt]) -> bool {
        let mut stack: Vec<&Stmt> = body.iter().collect();
        while let Some(s) = stack.pop() {
            match s {
                Stmt::Assign(a) => {
                    if a.targets.iter().any(Self::is_self_attr_expr) {
                        return true;
                    }
                }
                Stmt::AnnAssign(a) => {
                    if Self::is_self_attr_expr(a.target.as_ref()) {
                        return true;
                    }
                }
                Stmt::AugAssign(a) if Self::is_self_attr_expr(a.target.as_ref()) => {
                    return true;
                }
                _ => {}
            }
            push_stmt_children(s, &mut stack);
        }
        false
    }

    /// A static husk: every method static, nothing property-like, no dataclass
    /// fields, no state written.
    fn is_static_husk(all_static: bool, has_property: bool, has_dataclass_fields: bool, writes_state: bool) -> bool {
        all_static && !has_property && !has_dataclass_fields && !writes_state
    }

    /// Class with NO state of its own (no `self.X =` anywhere, no dataclass
    /// fields, no properties) whose members are ALL staticmethods — a namespace
    /// wearing a domain noun (#27 static-husk). Exclusions: inherited base (state
    /// may live in the base), ABC/Protocol (pure interfaces), pass-only subclass,
    /// `*Error`/`*Exception` (exception namespace).
    /// Is this class a husk: no state of its own, and every member stateless?
    /// The exclusions are the rule's own (a base, an interface, a pass-only
    /// subclass, an exception namespace).
    fn is_static_husk_class(cls: &StmtClassDef) -> bool {
        let has_base = cls.arguments.as_ref().is_some_and(|a| !a.args.is_empty());
        if has_base {
            return false; // inherited base — the state may live in the base
        }
        let name = cls.name.as_str();
        if name.ends_with("Error") || name.ends_with("Exception") {
            return false; // exception namespace
        }
        let methods = class_methods(cls);
        if methods.is_empty() {
            return false; // pass-only subclass / re-export marker
        }
        let abstract_ = methods.iter().any(|m| has_decorator(m, "abstractmethod"));
        let is_abc = cls.arguments.as_ref().is_some_and(|a| {
            a.args
                .iter()
                .any(|b| annotation_base_name(b).is_some_and(|n| n == "ABC" || n == "Protocol"))
        });
        if abstract_ || is_abc {
            return false; // pure interface
        }
        let has_property = methods.iter().any(|m| has_decorator(m, "property"));
        let has_dataclass_fields = cls.body.iter().any(|m| matches!(m, Stmt::AnnAssign(_)))
            || cls.decorator_list.iter().any(|d| match &d.expression {
                Expr::Name(n) => n.id.as_str() == "dataclass",
                Expr::Attribute(a) => a.attr.as_str() == "dataclass",
                _ => false,
            });
        let writes_state = methods.iter().any(|m| Self::body_writes_self_attr(&m.body));
        // a classmethod that never touches `cls` is a staticmethod in all but
        // name: it holds no state either
        let all_static = methods.iter().all(|m| {
            has_decorator(m, "staticmethod")
                || (has_decorator(m, "classmethod") && !FunctionFacts::body_refs_name(&m.body, "cls"))
        });
        Self::is_static_husk(all_static, has_property, has_dataclass_fields, writes_state)
    }

    pub fn findings(state: &mut ScanState, body: &[Stmt]) {
        for s in body {
            let Stmt::ClassDef(cls) = s else { continue };
            if !Self::is_static_husk_class(cls) {
                continue;
            }
            let line = stmt_line(state.source, s);
            state.findings.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                col: 0,
                file: state.file.to_string(),
                line,
                function: cls.name.to_string(),
                kind: "static-husk".into(),
                severity: "warn".into(),
                message: format!(
                    "Class {} has no state of its own and only staticmethods — a namespace wearing a domain noun. \
                 The fix: find the state these operations work on and put it IN the class (fields + method bodies \
                 reading them); if there is no such state, the operations still belong with SOME abstraction or \
                 another — possibly more than one: the class(es) that own the state they work on. Finding it is \
                 the task; reconsider whether THIS class is the correct abstraction. The methods stop being static: \
                 give the class a constructor that takes the state, instance attributes that hold it, and instance \
                 methods that read it.",
                    cls.name.as_str()
                ),
            });
        }
    }
}
