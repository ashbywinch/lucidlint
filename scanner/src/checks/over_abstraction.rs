//! `over-abstraction`: an ABC with exactly one concrete subclass — the base
//! exists only to be implemented once, so it earns nothing the subclass lacks.

use ruff_python_ast::{AnyNodeRef, Expr, Stmt, StmtClassDef};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::skeleton::{skel_children, Q};
use crate::source::Source;

use super::latent_state::LatentState;
use super::record_shape::RecordShape;

/// `over-abstraction`: the rule object. Its methods read the class + import index; the evidence and the message live together.
pub struct OverAbstraction;

const ABSTRACT_DECORATORS: [&str; 4] = [
    "abstractmethod",
    "abstractproperty",
    "abstractclassmethod",
    "abstractstaticmethod",
];

impl OverAbstraction {
    /// Top-level classes with abstractness and base references.
    pub fn collect_classes(body: &[Stmt], source: &str) -> Vec<crate::ClassInfo> {
        let mut out = Vec::new();
        for s in body {
            let Stmt::ClassDef(cls) = s else { continue };
            let (bases, abc_base) = Self::bases_of(cls);
            out.push(crate::ClassInfo {
                name: cls.name.to_string(),
                line: Source::line_of(source, cls.name.range().start()),
                abstract_: Self::has_abstract_decorator(cls) || abc_base,
                bases,
                declared: Self::declared_fields(cls),
            });
        }
        out
    }

    /// The decorators name an abstract member (`@abstractmethod`, `.abstractproperty`,
    /// ...) — the class declares itself abstract.
    fn has_abstract_decorator(cls: &StmtClassDef) -> bool {
        for d in &cls.decorator_list {
            let expr = if let Expr::Call(c) = &d.expression {
                c.func.as_ref()
            } else {
                &d.expression
            };
            match expr {
                Expr::Name(n) if ABSTRACT_DECORATORS.contains(&n.id.as_str()) => return true,
                Expr::Attribute(a) if ABSTRACT_DECORATORS.contains(&a.attr.as_str()) => return true,
                _ => {}
            }
        }
        false
    }

    /// The class's bases as comparable keys (`Name:x` / `Attr:alias:x`), and whether
    /// one of them is `abc` / `ABCMeta` — inheriting the ABC machinery is itself the
    /// abstraction claim.
    fn bases_of(cls: &StmtClassDef) -> (Vec<String>, bool) {
        let mut bases: Vec<String> = Vec::new();
        let mut abc = false;
        if let Some(arguments) = &cls.arguments {
            for b in &arguments.args {
                match b {
                    Expr::Name(n) => {
                        if matches!(n.id.to_lowercase().as_str(), "abc" | "abcmeta") {
                            abc = true;
                        }
                        bases.push(format!("Name:{}", n.id.as_str()));
                    }
                    Expr::Attribute(a) => {
                        if matches!(a.attr.to_lowercase().as_str(), "abc" | "abcmeta") {
                            abc = true;
                        }
                        if let Expr::Name(v) = a.value.as_ref() {
                            bases.push(format!("Attr:{}:{}", v.id.as_str(), a.attr.as_str()));
                        }
                    }
                    _ => {}
                }
            }
        }
        (bases, abc)
    }

    /// The attributes `__init__` annotates on `self` — the data the class says it
    /// holds, in the one place a reader looks for it.
    fn init_self_attrs(cls: &StmtClassDef) -> Vec<String> {
        let mut attrs = Vec::new();
        let init = cls.body.iter().find_map(|m| match m {
            Stmt::FunctionDef(f) if f.name.as_str() == "__init__" => Some(f),
            _ => None,
        });
        let Some(init) = init else { return attrs };
        for st in &init.body {
            let Stmt::AnnAssign(a) = st else { continue };
            let Expr::Attribute(at) = a.target.as_ref() else {
                continue;
            };
            if matches!(at.value.as_ref(), Expr::Name(n) if n.id.as_str() == "self") {
                attrs.push(at.attr.to_string());
            }
        }
        attrs
    }

    /// What the class declares as its data: annotations, `__slots__`, and the
    /// attributes `__init__` assigns to `self`.
    fn declared_fields(cls: &StmtClassDef) -> Vec<String> {
        let mut declared: Vec<String> = Vec::new();
        for m in &cls.body {
            match m {
                Stmt::AnnAssign(a) => {
                    if let Expr::Name(n) = a.target.as_ref() {
                        declared.push(n.id.to_string());
                    }
                }
                Stmt::Assign(a) => {
                    for t in &a.targets {
                        if let Expr::Name(n) = t {
                            if n.id.as_str() == "__slots__" {
                                if let Some(tuple) = LatentState::slot_names(&a.value) {
                                    declared.extend(tuple);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        declared.extend(Self::init_self_attrs(cls));
        declared.sort();
        declared.dedup();
        declared
    }

    pub fn collect_imports(body: &[Stmt], _source: &str) -> Vec<crate::ImportInfo> {
        let mut out = Vec::new();
        let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
        let mut qi = 0usize;
        while qi < queue.len() {
            if let Q::N(n) = queue[qi] {
                match n {
                    AnyNodeRef::StmtImportFrom(imp) => {
                        if let Some(module) = &imp.module {
                            for a in &imp.names {
                                let alias = a
                                    .asname
                                    .as_ref()
                                    .map(|x| x.to_string())
                                    .unwrap_or_else(|| a.name.to_string());
                                out.push(crate::ImportInfo {
                                    alias,
                                    module: module.to_string(),
                                    imported: a.name.to_string(),
                                });
                            }
                        }
                    }
                    AnyNodeRef::StmtImport(imp) => {
                        for a in &imp.names {
                            let alias = a
                                .asname
                                .as_ref()
                                .map(|x| x.to_string())
                                .unwrap_or_else(|| a.name.to_string());
                            out.push(crate::ImportInfo {
                                alias,
                                module: a.name.to_string(),
                                imported: a.name.to_string(),
                            });
                        }
                    }
                    _ => {}
                }
                skel_children(n, &mut queue);
            }
            qi += 1;
        }
        out
    }

    /// Abstract classes with exactly one concrete subclass — `_abstraction_actions`.
    pub fn abstraction_findings(scans: &[(String, Vec<crate::ClassInfo>, Vec<crate::ImportInfo>)]) -> Vec<Finding> {
        use std::collections::HashMap;
        let classes = Self::class_index(scans);
        let mut concrete: HashMap<(String, String), Vec<String>> = HashMap::new();
        for (rel, cls_list, imports) in scans {
            let import_map: HashMap<&str, (&str, &str)> = imports
                .iter()
                .map(|i| (i.alias.as_str(), (i.module.as_str(), i.imported.as_str())))
                .collect();
            for c in cls_list {
                for base in &c.bases {
                    for (mrel, mname) in Self::base_candidates(base, rel, &import_map) {
                        let Some(key) = RecordShape::class_key(&classes, &mrel, &mname) else {
                            continue;
                        };
                        if key == (rel.clone(), c.name.clone()) {
                            continue;
                        }
                        if classes.get(&key).map(|k| k.abstract_).unwrap_or(false) {
                            concrete.entry(key).or_default().push(c.name.clone());
                        }
                    }
                }
            }
        }
        let mut out = Vec::new();
        let mut entries: Vec<((String, String), Vec<String>)> = concrete.into_iter().collect();
        entries.sort();
        for ((rel, name), subs) in entries {
            if subs.len() != 1 {
                continue;
            }
            let line = classes.get(&(rel.clone(), name.clone())).map(|c| c.line).unwrap_or(1);
            out.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                            file: rel.clone(),
                            line,
                            function: name.clone(),
                            kind: "over-abstraction".into(),
                            severity: "fail".into(),
                            message: format!(
                                "abstract class '{name}' in {rel} has exactly one concrete subclass ('{}') — an ABC with a single implementation is ceremony: fold the subclass into the base or drop the ABC; an abstraction earns its keep at two real, differing implementations",
                                subs[0]
                            ), });
        }
        out
    }

    /// Every top-level class in the scan, keyed by (file, class name).
    fn class_index(
        scans: &[(String, Vec<crate::ClassInfo>, Vec<crate::ImportInfo>)],
    ) -> std::collections::HashMap<(String, String), &crate::ClassInfo> {
        let mut classes = std::collections::HashMap::new();
        for (rel, cls_list, _) in scans {
            for c in cls_list {
                classes.insert((rel.clone(), c.name.clone()), c);
            }
        }
        classes
    }

    /// The classes one base reference could mean: the same-file class of that name,
    /// and the class the name is imported as. `Name:x` is a plain reference;
    /// `Attr:alias:x` is `alias.x` through an import.
    fn base_candidates(
        base: &str,
        rel: &str,
        import_map: &std::collections::HashMap<&str, (&str, &str)>,
    ) -> Vec<(String, String)> {
        if let Some(rest) = base.strip_prefix("Name:") {
            let mut cands = vec![(rel.to_string(), rest.to_string())];
            if let Some((module, imported)) = import_map.get(rest) {
                cands.push(((*module).to_string(), (*imported).to_string()));
            }
            return cands;
        }
        let Some(rest) = base.strip_prefix("Attr:") else {
            return Vec::new();
        };
        let mut parts = rest.splitn(2, ':');
        let alias = parts.next().unwrap_or("");
        let attr = parts.next().unwrap_or("");
        match import_map.get(alias) {
            Some((module, _name)) => vec![((*module).to_string(), attr.to_string())],
            None => Vec::new(),
        }
    }

    // ------------------------------------------------- unnamed-condition: named decisions
    // Each names one decision the checks make inline, so the call site reads as the
    // question it asks rather than a list of operators.
}
