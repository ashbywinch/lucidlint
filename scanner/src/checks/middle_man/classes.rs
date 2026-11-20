//! The file's classes and the repo-wide index: the queries every middle-man
//! exemption asks of them.

use std::collections::{HashMap, HashSet};

use ruff_python_ast::Stmt;

use super::super::push_stmt_children;
use super::class::MiddleManClass;
use super::declaration::{InterfaceDeclaration, ParamShape};

/// The file's classes plus the queries the rule asks of them: one home for the
/// resolution all three exemptions share.
pub(super) struct ModuleClasses {
    pub(crate) classes: HashMap<String, MiddleManClass>,
    /// member name → the declarations that require it (per declaration — an
    /// implementation conforms to at least one).
    pub(crate) interface_members: HashMap<String, Vec<InterfaceDeclaration>>,
}

impl ModuleClasses {
    /// Read the file's classes (nested ones included).
    pub(super) fn read(body: &[Stmt], module: &str) -> Self {
        let mut out = Self {
            classes: HashMap::new(),
            interface_members: HashMap::new(),
        };
        let mut sq: Vec<&Stmt> = body.iter().collect();
        while let Some(s) = sq.pop() {
            if let Stmt::ClassDef(c) = s {
                let cls = MiddleManClass::read(c, module);
                out.declare_interface_members(&cls);
                out.classes.insert(cls.name.clone(), cls);
            }
            match s {
                Stmt::FunctionDef(f) => sq.extend(&f.body),
                _ => push_stmt_children(s, &mut sq),
            }
        }
        out
    }

    /// The repo-wide index: every scanned file's classes merged by name.
    /// Same-named classes merge (methods, bases, attrs union) — an ambiguous
    /// name means "could be this target", which is exactly what the
    /// resolution asks, and the exemptions then read the union's most
    /// forgiving declaration.
    pub(super) fn merge(classes: &[MiddleManClass]) -> Self {
        let mut out = Self {
            classes: HashMap::new(),
            interface_members: HashMap::new(),
        };
        for c in classes {
            out.declare_interface_members(c);
            out.classes
                .entry(c.name.clone())
                .and_modify(|e: &mut MiddleManClass| {
                    e.methods.extend(c.methods.iter().map(|(k, v)| (k.clone(), v.clone())));
                    e.bases.extend(c.bases.iter().cloned());
                    e.attrs.extend(c.attrs.iter().map(|(k, v)| (k.clone(), v.clone())));
                    e.is_interface |= c.is_interface;
                })
                .or_insert_with(|| c.clone());
        }
        out
    }

    /// Record the members a class declares as an interface. Every declaration
    /// keeps the interface's own name and module: conformance alone is not
    /// evidence that the interface is about a given wrapper (issue #65).
    fn declare_interface_members(&mut self, cls: &MiddleManClass) {
        for (member, names) in cls.interface_declarations() {
            self.interface_members
                .entry(member.to_string())
                .or_default()
                .push(InterfaceDeclaration {
                    interface: cls.name.clone(),
                    module: cls.module.clone(),
                    names: names.clone(),
                });
        }
    }

    /// Every declaration of the chain's member the receiver reaches: the
    /// `self.<attr>` steps resolve through the index (the attribute's class,
    /// then that class's own attributes), and the member may sit on the
    /// receiver class or on any base of it. Empty = the chain leaves the
    /// scanned codebase (a library's method, an attribute with no type
    /// evidence): "call the target directly" is not advice anyone can take
    /// there.
    pub(super) fn declarations<'a>(&'a self, owner: &str, chain: &[String]) -> Vec<&'a ParamShape> {
        let Some((member, path)) = chain.split_last() else {
            return Vec::new();
        };
        let Some(mut cls) = self.classes.get(owner) else {
            return Vec::new();
        };
        for step in path {
            match cls.attrs.get(step).and_then(|name| self.classes.get(name)) {
                Some(next) => cls = next,
                None => return Vec::new(),
            }
        }
        let mut out = Vec::new();
        let mut queue: Vec<&MiddleManClass> = vec![cls];
        let mut seen: HashSet<&str> = HashSet::new();
        while let Some(c) = queue.pop() {
            if !seen.insert(c.name.as_str()) {
                continue;
            }
            if let Some(shape) = c.declares(member) {
                out.push(shape);
            }
            queue.extend(c.bases.iter().filter_map(|b| self.classes.get(b)));
        }
        out
    }

    /// The bases the owner names, directly or through another base this index
    /// knows — the class's own statement that it implements them.
    fn named_bases(&self, owner: &str) -> HashSet<String> {
        let mut named: HashSet<String> = HashSet::new();
        let mut queue: Vec<String> = vec![owner.to_string()];
        while let Some(name) = queue.pop() {
            if !named.insert(name.clone()) {
                continue;
            }
            if let Some(c) = self.classes.get(&name) {
                queue.extend(c.bases.iter().cloned());
            }
        }
        named.remove(owner);
        named
    }

    /// Is the method the owner's OWN interface contract? The wrapper's
    /// parameters must accept the declared member's — a conforming
    /// implementation, not a name collision — AND the declaration must be
    /// about this class: either the interface lives in the owner's module (the
    /// seam sits with the class, the same rule as `class-module`'s protocol
    /// exemption), or the owner names it as a base, at any distance through the
    /// repo's bases. An interface declared in an unrelated module that the
    /// class does not name is not evidence about this class: repo-wide state
    /// must not silence a delegation (issue #65).
    pub(super) fn implements_interface_member(
        &self,
        owner: &str,
        module: &str,
        name: &str,
        wrapper: &ParamShape,
    ) -> bool {
        let Some(decls) = self.interface_members.get(name) else {
            return false;
        };
        let named = self.named_bases(owner);
        decls
            .iter()
            .any(|d| d.names.is_subset(&wrapper.names) && (d.module == module || named.contains(d.interface.as_str())))
    }
}
