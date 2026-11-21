//! `scattered-instance`: module-level bindings that construct classes no
//! object owns — the objects have a class and no home.

use std::collections::HashSet;

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::finding::Finding;
use crate::source::Source;
use crate::ScanState;

/// `scattered-instance`: the rule object. Its methods read the collected class references.
pub struct Scattered;

/// One module-level binding that constructs a class: the raw material for the
/// "these objects have no owner" pass. The callee is resolved repo-wide — a
/// class defined anywhere in the scan set counts, so an imported client does.
#[derive(Clone)]
pub struct ScatteredInstance {
    pub line: usize,
    pub binding: String,
    pub callee: String,
}

impl Scattered {
    /// Collect the module-level `NAME = SomeClass(...)` bindings. Only a
    /// CamelCase callee can be a class; the repo-wide pass drops the ones that
    /// name no class in the scan set (stdlib constructors, third-party clients).
    pub fn collect_scattered_instances(state: &mut ScanState, body: &[Stmt]) {
        for stmt in body {
            let (target, value): (&ruff_python_ast::name::Name, &Expr) = match stmt {
                Stmt::Assign(a) if a.targets.len() == 1 => match &a.targets[0] {
                    Expr::Name(n) => (&n.id, &a.value),
                    _ => continue,
                },
                Stmt::AnnAssign(a) => match (a.target.as_ref(), a.value.as_deref()) {
                    (Expr::Name(n), Some(v)) => (&n.id, v),
                    _ => continue,
                },
                _ => continue,
            };
            let Expr::Call(call) = value else { continue };
            let Expr::Name(callee) = call.func.as_ref() else {
                continue;
            };
            // a private class is still a class: `_RustScan()` counts
            if !callee
                .id
                .as_str()
                .trim_start_matches('_')
                .chars()
                .next()
                .is_some_and(|c| c.is_uppercase())
            {
                continue;
            }
            state.scattered.push(ScatteredInstance {
                line: Source::line_of(state.source, stmt.range().start()),
                binding: target.to_string(),
                callee: callee.id.to_string(),
            });
        }
    }

    /// Several module-level objects of the repo's own classes are one value with no
    /// owner: nothing says which class holds them, so every caller reaches into the
    /// module. One such object is the container itself and is exempt.
    pub fn scattered_instance_findings(
        scans: &[(String, Vec<ScatteredInstance>)],
        classes: &HashSet<String>,
    ) -> Vec<Finding> {
        scans
            .iter()
            .filter_map(|(rel, instances)| Self::scattered_finding(rel, instances, classes))
            .collect()
    }

    /// One module's scatter, or None when its objects are the container itself.
    fn scattered_finding(rel: &str, instances: &[ScatteredInstance], classes: &HashSet<String>) -> Option<Finding> {
        let owned: Vec<&ScatteredInstance> = instances.iter().filter(|i| classes.contains(&i.callee)).collect();
        if !Self::is_scatter(&owned) {
            return None;
        }
        let names: Vec<&str> = owned.iter().map(|i| i.binding.as_str()).collect();
        Some(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            col: 0,
            file: rel.to_string(),
            line: owned[0].line,
            function: String::new(),
            kind: "scattered-instance".into(),
            severity: "warn".into(),
            message: format!(
                "{} module-level objects ({}) are one value, and nothing says which class holds it. \
                 Every caller reaches into the module to get them. Make one object that holds them, and \
                 make it an attribute of the class that uses it.",
                owned.len(),
                Self::join_names(&names)
            ),
        })
    }

    /// A scatter is one concept spread across objects. ONE module-level object is
    /// the container itself and is exempt; two is already a scattering, whatever
    /// their classes — nothing says which object holds them.
    fn is_scatter(owned: &[&ScatteredInstance]) -> bool {
        owned.len() >= 2
    }

    fn join_names(names: &[&str]) -> String {
        match names {
            [] => String::new(),
            [only] => (*only).to_string(),
            [a, b] => format!("{a} and {b}"),
            _ => match names.split_last() {
                Some((last, rest)) => format!("{}, and {last}", rest.join(", ")),
                None => String::new(),
            },
        }
    }

    // ------------------------------------------------------------ unnamed-condition
}
