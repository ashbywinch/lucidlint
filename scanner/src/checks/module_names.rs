//! The module-scope naming families: `class-module` (a public class the file
//! is not named after), `vague-name` (Manager/Handler/...), and `strewing` (free
//! functions sharing a leading parameter).

// lucidlint: ignore-file complexity the class-module walker is a dispatch table over
// the statement kinds — arm count is the AST's surface, not branching
// lucidlint: ignore-file closures the nested visit is the walker's own scope, not a
// latent class — it collects the module's classes in one pass
// lucidlint: ignore-file large-function the class-module walker stays one mapping so
// every module-level binding is read in one place — the alternative is five partial
// walkers

use ruff_python_ast::{Expr, Stmt, StmtClassDef};
use ruff_text_size::Ranged;

use crate::finding::Finding;

use super::annotation_base_name;
use super::function_facts::FunctionFacts;
use super::middle_man::class_base_names;
use crate::naming::VAGUE_SUFFIXES;
use crate::{line_of, stmt_line, ScanState};

/// A module whose public class(es) aren't findable by name — the
/// class-module rule. The file system is a name index: a class belongs in a
/// file named after it. Single arm: exactly one public class whose name
/// doesn't match the file stem (rename the file). Multi arm (#31): >=2
/// public classes where AT LEAST ONE name doesn't match the stem — a
/// matching sibling excuses nothing; the misplaced classes move to files
/// named after them (split-module). A module that ALSO defines module-level
/// functions is a tool script, not a class file (the class is a component,
/// not the module's identity) — renaming it after the class would be wrong.
/// Private classes (`_`-prefixed) are never the module's identity.
pub fn class_module_findings(state: &mut ScanState, module_body: &[Stmt], rel: &str) {
    if rel.ends_with("__init__.py") {
        return;
    }
    let classes: Vec<&Stmt> = module_body.iter().filter(|s| matches!(s, Stmt::ClassDef(_))).collect();
    if classes.is_empty() {
        return;
    }
    let has_module_fns = module_body.iter().any(|s| matches!(s, Stmt::FunctionDef(_)));
    if has_module_fns {
        return; // tool script — the class is a component, not the module identity
    }
    let public: Vec<&Stmt> = classes
        .iter()
        .filter(|s| match s {
            Stmt::ClassDef(c) => !c.name.as_str().starts_with('_'),
            _ => true,
        })
        .copied()
        .collect();
    if public.is_empty() {
        return;
    }
    let stem = rel
        .rsplit('/')
        .next()
        .unwrap_or(rel)
        .trim_end_matches(".py")
        .to_lowercase();
    let matches_stem = |name: &str| {
        let n = name.to_lowercase();
        n == stem || n == stem.replace('_', "")
    };
    // Python's ast.ClassDef.lineno is the `class` keyword line; ruff's range
    // starts at the first decorator — use the name's range for parity
    if public.len() == 1 {
        let Stmt::ClassDef(cls) = public[0] else { return };
        if matches_stem(cls.name.as_str()) {
            return;
        }
        let line = line_of(state.source, cls.name.range().start());
        state.findings.push(Finding { logical_start: None, seam_members: Vec::new(),
        col: 0,
        file: state.file.to_string(),
        line,
        function: cls.name.to_string(),
        kind: "class-module".into(),
        severity: "fail".into(),
        message: format!(
            "{rel} holds one class named '{cls}' — readers browse the codebase by file structure, and a differently named file hides the class from that walk. Rename the file to '{stem}.py' (exception: closely related models).",
            cls = cls.name.as_str(),
            stem = cls.name.to_lowercase()
        ), });
        return;
    }
    // multi arm: the misplaced public classes move to files named after them —
    // except a Protocol the module's OWN named class inherits: the interface
    // exists because that class does, so it belongs beside it (issue #52).
    // Two boundaries the ticket states: inheritance, not structural
    // conformance (a Protocol the named class does not declare as a base stays
    // misplaced), and a public DATACLASS keeps its own module even when it also
    // types the seam — "they belong in their own module; this exemption must
    // not swallow that".
    /// The names this module binds to `dataclasses.dataclass` — the decorator can
    /// be aliased (`from dataclasses import dataclass as dc`), and the ticket's
    /// constraint reads the class's ROLE, not the decorator's spelling.
    fn dataclass_names(module_body: &[Stmt]) -> std::collections::HashSet<String> {
        let mut out: std::collections::HashSet<String> = std::collections::HashSet::from(["dataclass".to_string()]);
        for s in module_body {
            let Stmt::ImportFrom(im) = s else { continue };
            if im.module.as_ref().map(|m| m.as_str()) != Some("dataclasses") {
                continue;
            }
            for a in &im.names {
                if a.name.as_str() == "dataclass" {
                    out.insert(a.asname.as_ref().map_or("dataclass".to_string(), |n| n.to_string()));
                }
            }
        }
        out
    }
    /// A class decorated `@dataclass`, bare or called (`@dataclass(frozen=True)`),
    /// under any of this module's names for it.
    fn decorated_dataclass(c: &StmtClassDef, names: &std::collections::HashSet<String>) -> bool {
        c.decorator_list.iter().any(|d| {
            let e = match &d.expression {
                Expr::Call(call) => call.func.as_ref(),
                other => other,
            };
            match e {
                Expr::Name(n) => names.contains(n.id.as_str()),
                Expr::Attribute(a) => a.attr.as_str() == "dataclass",
                _ => false,
            }
        })
    }
    let named_bases: std::collections::HashSet<String> = public
        .iter()
        .filter_map(|s| match s {
            Stmt::ClassDef(c) if matches_stem(c.name.as_str()) => Some(class_base_names(c)),
            _ => None,
        })
        .flatten()
        .collect();
    let dataclass_decorators = dataclass_names(module_body);
    /// The members a class declares (its own defs) — the interface's contract.
    fn declared_members(c: &StmtClassDef) -> std::collections::HashSet<String> {
        c.body
            .iter()
            .filter_map(|m| match m {
                Stmt::FunctionDef(f) => Some(f.name.to_string()),
                _ => None,
            })
            .collect()
    }
    /// A body that only DECLARES: docstrings, `...`, `pass`, and defs whose own
    /// bodies are only those. The shape of an interface, not an implementation.
    fn declarations_only(c: &StmtClassDef) -> bool {
        fn decl(stmts: &[Stmt]) -> bool {
            stmts.iter().all(|s| match s {
                Stmt::Pass(_) => true,
                Stmt::Expr(e) => matches!(e.value.as_ref(), Expr::EllipsisLiteral(_) | Expr::StringLiteral(_)),
                _ => false,
            })
        }
        c.body.iter().all(|s| match s {
            Stmt::Pass(_) => true,
            Stmt::Expr(e) => matches!(e.value.as_ref(), Expr::EllipsisLiteral(_) | Expr::StringLiteral(_)),
            Stmt::FunctionDef(f) => decl(&f.body),
            _ => false,
        })
    }
    // every member the module's namesake class implements — the evidence that a
    // declarations-only interface exists because that class does
    let namesake_members: std::collections::HashSet<String> = public
        .iter()
        .filter_map(|s| match s {
            Stmt::ClassDef(c) if matches_stem(c.name.as_str()) => Some(declared_members(c)),
            _ => None,
        })
        .flatten()
        .collect();
    // a Protocol with ANOTHER implementer in this module is not the namesake
    // class's own seam (issue #52: "the namesake class and test doubles")
    let has_other_implementer = |name: &str| {
        public.iter().any(|s| match s {
            Stmt::ClassDef(c) => c.name.as_str() != name && class_base_names(c).iter().any(|b| b == name),
            _ => false,
        })
    };
    let is_the_seam = |c: &StmtClassDef| {
        let interface = class_base_names(c).iter().any(|b| b == "Protocol" || b == "ABC");
        if !interface || decorated_dataclass(c, &dataclass_decorators) {
            return false;
        }
        // the namesake class declares it as a base (PR for #52) …
        if named_bases.contains(c.name.as_str()) {
            return true;
        }
        // … or satisfies it structurally: declarations-only, every declared
        // member implemented by the namesake class, and no other implementer
        // in this module
        let members = declared_members(c);
        declarations_only(c)
            && !members.is_empty()
            && members.iter().all(|m| namesake_members.contains(m))
            && !has_other_implementer(c.name.as_str())
    };
    let misplaced: Vec<&Stmt> = public
        .iter()
        .filter(|s| match s {
            Stmt::ClassDef(c) => !matches_stem(c.name.as_str()) && !is_the_seam(c),
            _ => false,
        })
        .copied()
        .collect();
    if misplaced.is_empty() {
        return;
    }
    let names: Vec<String> = misplaced
        .iter()
        .filter_map(|s| match s {
            Stmt::ClassDef(c) => Some(c.name.to_string()),
            _ => None,
        })
        .collect();
    let all_names: Vec<String> = public
        .iter()
        .filter_map(|s| match s {
            Stmt::ClassDef(c) => Some(c.name.to_string()),
            _ => None,
        })
        .collect();
    let Stmt::ClassDef(first) = misplaced[0] else { return };
    let line = line_of(state.source, first.name.range().start());
    state.findings.push(Finding { logical_start: None, seam_members: Vec::new(),
    col: 0,
    file: state.file.to_string(),
    line,
    function: names.join(", "),
    kind: "class-module".into(),
    severity: "fail".into(),
    message: format!(
        "module '{rel}' holds the classes {all} but {misplaced} don't match the file's name — each public class belongs in a file named after it — fix: split-module",
        all = all_names.join(", "),
        misplaced = names.join(", "),
    ), });
}

/// Vague role-suffix class names hiding load-bearing code.
pub fn vague_name_findings(state: &mut ScanState, module_body: &[Stmt]) {
    for s in module_body {
        let Stmt::ClassDef(cls) = s else { continue };
        for suffix in VAGUE_SUFFIXES {
            if !cls.name.as_str().ends_with(suffix) {
                continue;
            }
            let methods = cls.body.iter().filter(|m| matches!(m, Stmt::FunctionDef(_))).count();
            let span =
                line_of(state.source, cls.range().end()).saturating_sub(line_of(state.source, cls.range().start()));
            if span < 120 && methods < 6 {
                break; // thin role class — the name is the communication
            }
            let line = stmt_line(state.source, s);
            state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                            file: state.file.to_string(),
                            line,
                            function: cls.name.to_string(),
                            kind: "vague-name".into(),
                            severity: "fail".into(),
                            message: format!(
                                "'{suffix}' name carries a {span}-line class with {methods} methods — the domain noun should take the name — fix: vague-name --fix-name <Name>"
                            ), });
            break;
        }
    }
}

/// Strewing: 3+ free functions sharing a leading parameter that is a class
/// defined in this module — a missed class.
pub fn strewing_findings(state: &mut ScanState, module_body: &[Stmt]) {
    let class_names: Vec<String> = module_body
        .iter()
        .filter_map(|s| match s {
            Stmt::ClassDef(c) => Some(c.name.to_string()),
            _ => None,
        })
        .collect();
    let mut groups: std::collections::HashMap<String, Vec<(String, usize)>> = std::collections::HashMap::new();
    for s in module_body {
        let Stmt::FunctionDef(f) = s else { continue };
        if f.parameters.args.is_empty() {
            continue;
        }
        let ann = match &f.parameters.args[0].parameter.annotation {
            Some(a) => annotation_base_name(a),
            None => None,
        };
        if let Some(base) = ann {
            // strewing is "they SHARE DATA": a fn that never reads its leading
            // param has nothing to share — moving it into the class would make
            // the class field-disjoint (partition) and the fix would thrash
            let recv = f.parameters.args[0].parameter.name.id.as_str();
            if class_names.contains(&base) && FunctionFacts::body_refs_name(&f.body, recv) {
                groups
                    .entry(base)
                    .or_default()
                    .push((f.name.to_string(), stmt_line(state.source, s)));
            }
        }
    }
    for (base, mut members) in groups {
        if members.len() < 3 {
            continue;
        }
        members.sort_by_key(|m| m.1);
        let names: Vec<String> = members.iter().map(|(n, l)| format!("{n} (line {l})")).collect();
        let line = members[0].1;
        state.findings.push(Finding { logical_start: None, seam_members: members.iter().map(|(n, _)| n.clone()).collect(),
        col: 0,
        file: state.file.to_string(),
        line,
        function: String::new(),
        kind: "strewing".into(),
        severity: "fail".into(),
        message: format!(
            "{} free functions all take '{base}' as their first argument — a '{base}' class is missing, and these functions are its methods: {} — fix: extract-class",
            members.len(),
            names.join(", ")
        ), });
    }
}
