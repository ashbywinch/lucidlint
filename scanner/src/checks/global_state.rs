//! `global-state`: module-level mutable state makes a module's behaviour depend
//! on what happened before the call.

use ruff_python_ast::{Expr, Operator, Stmt};

use crate::finding::Finding;

use super::middle_man::MiddleManCandidate;
use crate::source::Source;
use crate::ScanState;

/// `global-state`: the rule object. Its methods read one module-level binding; the evidence and the message live together.
pub struct GlobalState;

#[derive(Clone, Copy)]
enum GlobalValueKind {
    MutableContainer,
    Rebinding,
    Constant,
    Other,
}

impl GlobalState {
    /// Is this assignment value a TYPE rather than a value? Union aliases
    /// (`ToolArgs = A | B`) and typing subscripts (`Answer = tuple[str, int]`,
    /// `Callback = Callable[[int], bool]`) are names for types — nothing a caller
    /// can vary, inject, or mutate, so the global-state message is false for them
    /// (album round: five aliases reported as "module-level variables").
    fn is_type_alias_value(name: &str, value: &Expr) -> bool {
        /// Subscription heads whose argument is a type expression.
        const TYPE_HEADS: &[&str] = &[
            "list",
            "dict",
            "tuple",
            "set",
            "frozenset",
            "type",
            "Callable",
            "Union",
            "Optional",
            "Literal",
            "Annotated",
            "Sequence",
            "Mapping",
            "Iterable",
            "Iterator",
            "Awaitable",
            "TypeVar",
            "TypeAlias",
        ];
        /// A CamelCase target names a type; ALL_CAPS and snake_case name values.
        fn camel_case(name: &str) -> bool {
            name.chars().next().is_some_and(|c| c.is_uppercase())
                && name.chars().any(|c| c.is_lowercase())
                && !name.contains('_')
        }
        fn is_type_term(e: &Expr) -> bool {
            match e {
                Expr::Name(_) | Expr::Attribute(_) => true,
                Expr::Subscript(s) => is_type_term(s.value.as_ref()),
                Expr::BinOp(b) => {
                    matches!(b.op, Operator::BitOr) && is_type_term(b.left.as_ref()) && is_type_term(b.right.as_ref())
                }
                _ => false,
            }
        }
        match value {
            // `A | B | None` — every operand a name/attribute/subscript
            Expr::BinOp(b) => {
                matches!(b.op, Operator::BitOr) && is_type_term(b.left.as_ref()) && is_type_term(b.right.as_ref())
            }
            Expr::Subscript(s) => match s.value.as_ref() {
                Expr::Name(n) => TYPE_HEADS.contains(&n.id.as_str()),
                // `collections.abc.Sequence[int]` — the head is an attribute
                Expr::Attribute(a) => TYPE_HEADS.contains(&a.attr.as_str()),
                _ => false,
            },
            // `MyAlias = SomeExistingType` / `Base = pydantic.BaseModel` — a bare
            // reference is an alias when the target is CamelCase; `X = os.environ`
            // is a value, not a type
            Expr::Name(_) | Expr::Attribute(_) => camel_case(name),
            _ => false,
        }
    }

    /// Module-level variable assignments (ANY — mutable or constant, mutated or
    /// not; third-pass ruling 3: "values are a class's private internals") plus
    /// `global` statements. The narrow "mutable container mutated inside a
    /// function" arm is merged into this — one family, one message. Carve-out:
    /// global services containers and framework-required globals (`app =
    /// Flask(...)`, DI registries) — a CALL-valued assignment is a service
    /// registration, not a domain value.
    /// What the AST already says about a module-level binding. The message names
    /// the case so the reader does not have to classify it.
    fn is_mutable_container(value: &Expr) -> bool {
        matches!(
            value,
            Expr::List(_) | Expr::Dict(_) | Expr::Set(_) | Expr::ListComp(_) | Expr::DictComp(_) | Expr::SetComp(_)
        )
    }

    fn is_rebinding(value: &Expr) -> bool {
        matches!(value, Expr::Name(_) | Expr::Attribute(_) | Expr::Subscript(_))
    }

    fn is_constant_binding(value: &Expr) -> bool {
        matches!(
            value,
            Expr::StringLiteral(_)
                | Expr::BytesLiteral(_)
                | Expr::NumberLiteral(_)
                | Expr::BooleanLiteral(_)
                | Expr::NoneLiteral(_)
                | Expr::EllipsisLiteral(_)
                | Expr::FString(_)
                | Expr::TString(_)
                | Expr::Tuple(_)
                | Expr::UnaryOp(_)
                | Expr::BinOp(_)
                | Expr::If(_)
                | Expr::BoolOp(_)
                | Expr::Compare(_)
        )
    }

    fn global_value_kind(value: Option<&Expr>) -> GlobalValueKind {
        match value {
            Some(value) if Self::is_mutable_container(value) => GlobalValueKind::MutableContainer,
            Some(value) if Self::is_rebinding(value) => GlobalValueKind::Rebinding,
            Some(value) if Self::is_constant_binding(value) => GlobalValueKind::Constant,
            _ => GlobalValueKind::Other,
        }
    }

    /// A module-level binding the rule reports as a constant: a literal-ish
    /// value, not a dunder, not a type alias. Mirrors the finding loop's filters
    /// so the message's count matches the constant findings.
    fn is_reported_module_constant(name: &str, value: &Expr) -> bool {
        !MiddleManCandidate::is_dunder(name)
            && Self::is_constant_binding(value)
            && !Self::is_type_alias_value(name, value)
    }

    /// The module-level constants the rule will report, in source order. Mirrors
    /// the finding loop's filters so the message's count matches the constant
    /// findings.
    pub fn module_constant_names(body: &[Stmt]) -> Vec<String> {
        fn names_of<'a>(targets: impl Iterator<Item = &'a Expr>, value: &Expr) -> Vec<String> {
            targets
                .filter_map(|target| match target {
                    Expr::Name(n) => Some(n.id.as_str()),
                    _ => None,
                })
                .filter(|name| GlobalState::is_reported_module_constant(name, value))
                .map(str::to_string)
                .collect()
        }
        let mut names = Vec::new();
        for stmt in body {
            match stmt {
                Stmt::Assign(a) => names.extend(names_of(a.targets.iter(), &a.value)),
                Stmt::AnnAssign(a) => {
                    if let Some(value) = a.value.as_deref() {
                        names.extend(names_of(std::iter::once(a.target.as_ref()), value));
                    }
                }
                _ => {}
            }
        }
        names
    }

    fn global_state_message(name: &str, value: Option<&Expr>, module_constants: &[String]) -> String {
        let kind = Self::global_value_kind(value);
        let (opening, middle, action) = match kind {
            GlobalValueKind::MutableContainer => (
                "is a mutable container bound at import time",
                "Every reader shares the same object, and a caller cannot vary it without monkeypatching the module.",
                "Make it an attribute of the class that owns the work.",
            ),
            GlobalValueKind::Rebinding => (
                "is bound to another value at import time",
                "Every reader shares that binding, and a caller cannot vary it without monkeypatching the module.",
                "Give that value one home and import it here; do not re-bind it.",
            ),
            GlobalValueKind::Constant => (
                "is a constant bound at import time",
                "Every reader shares one value, and a caller cannot vary it without monkeypatching the module.",
                "Make it a parameter default of the function that reads it, or an attribute of the class that owns the work.",
            ),
            GlobalValueKind::Other => (
                "binds at import time",
                "Every reader shares one value, and a caller cannot vary it without monkeypatching the module.",
                "Make it an attribute of the class that owns the work.",
            ),
        };
        let message = format!("{name} {opening}. {middle} {action}");
        // The set-level nudge is one observation about the module, so it rides
        // the first constant only: a module with eight constants must not repeat
        // the same sentence eight times (#80).
        let is_first_constant = module_constants.first().is_some_and(|first| first == name);
        let nudge_rides_this_constant =
            module_constants.len() >= 3 && is_first_constant && matches!(kind, GlobalValueKind::Constant);
        if nudge_rides_this_constant {
            return format!(
                "{message} If the module's {} constants are one concept, put them on one class — a new class, or the module's own class.",
                module_constants.len()
            );
        }
        message
    }

    pub fn global_state_findings(state: &mut ScanState, stmt: &Stmt, module_level: bool) {
        let fn_name = state.current_fn.as_ref().map(|f| f.0.clone()).unwrap_or_default();
        if let Stmt::Global(g) = stmt {
            let line = Source::stmt_line(state.source, stmt);
            let names: Vec<&str> = g.names.iter().map(|n| n.as_str()).collect();
            state.findings.push(Finding { logical_start: None, seam_members: Vec::new(),
            col: 0,
            file: state.file.to_string(),
            line,
            function: fn_name.clone(),
            kind: "global-state".into(),
            severity: "fail".into(),
            message: format!(
                "{} is a module-level variable: a global read that binds at import time. Callers cannot inject or vary it without monkeypatching the module. Find the class this value serves; make it that class's attribute.",
                names.join(", ")
            ), });
            return;
        }
        if module_level {
            // both Assign and AnnAssign (typed dicts like `_oauth_states: dict =
            // {}` are module state exactly like bare literals); AugAssign too —
            // `X += ...` at module level is a module-level variable write.
            let value: Option<&Expr> = match stmt {
                Stmt::Assign(a) => Some(a.value.as_ref()),
                Stmt::AnnAssign(a) => a.value.as_deref(),
                Stmt::AugAssign(_) => None,
                _ => None,
            };
            let targets: Vec<&Expr> = match stmt {
                Stmt::Assign(a) => a.targets.iter().collect(),
                Stmt::AnnAssign(a) => vec![a.target.as_ref()],
                Stmt::AugAssign(a) => vec![a.target.as_ref()],
                _ => Vec::new(),
            };
            let call_valued = value.is_some_and(|v| matches!(v, Expr::Call(_)));
            for target in targets {
                let Expr::Name(n) = target else { continue };
                let name = n.id.as_str();
                if MiddleManCandidate::is_dunder(name) {
                    continue; // module protocol metadata (__all__, __version__)
                }
                if call_valued {
                    continue; // a service/framework registration, not a domain value
                }
                if value.is_some_and(|v| Self::is_type_alias_value(name, v)) {
                    continue; // a name for a type, not a variable a caller varies
                }
                let line = Source::stmt_line(state.source, stmt);
                let message = Self::global_state_message(name, value, &state.module_constants);
                state.findings.push(Finding {
                    logical_start: None,
                    seam_members: Vec::new(),
                    col: 0,
                    file: state.file.to_string(),
                    line,
                    function: fn_name.clone(),
                    kind: "global-state".into(),
                    severity: "fail".into(),
                    message,
                });
            }
        }
    }
}
