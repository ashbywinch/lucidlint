//! `partition`: a class whose methods fall into field-disjoint groups — two
//! classes and a shared data object trying to be one.

use ruff_python_ast::{AnyNodeRef, Expr, Stmt, StmtClassDef};
use ruff_text_size::Ranged;
use std::collections::HashSet;

use crate::finding::Finding;
use crate::skeleton::{skel_children, Q};
use crate::source::Source;
use crate::ScanState;

/// `partition`: the rule object. Its methods read one class's field use; the evidence and the message live together.
/// The method count and span that make a class a field-disjoint partition in
/// waiting.
const PARTITION_METHODS_MIN: usize = 6;
const PARTITION_SPAN_MIN: usize = 150;

pub struct Partition;

impl Partition {
    /// `_partition_for_class`: a class whose methods split into >= 2
    /// field-disjoint groups after removing at most 2 connectors.
    fn partition_for_class(source: &str, cls: &StmtClassDef) -> Option<(Vec<String>, Vec<Vec<String>>)> {
        let methods: Vec<&Stmt> = cls.body.iter().filter(|s| matches!(s, Stmt::FunctionDef(_))).collect();
        if methods.len() < PARTITION_METHODS_MIN {
            return None;
        }
        let span = Source::line_of(source, cls.range().end()) - Source::line_of(source, cls.name.range().start());
        if span < PARTITION_SPAN_MIN {
            return None;
        }
        // method name -> set of self.<attr> fields accessed anywhere in the body
        let mut mf: Vec<(String, HashSet<String>)> = Vec::new();
        for m in &methods {
            let Stmt::FunctionDef(f) = m else { continue };
            let mut fields = HashSet::new();
            let mut queue: Vec<Q> = f.body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
            queue.extend(f.decorator_list.iter().map(|d| Q::N(AnyNodeRef::from(&d.expression))));
            let mut qi = 0usize;
            while qi < queue.len() {
                if let Q::N(n) = queue[qi] {
                    if let AnyNodeRef::ExprAttribute(a) = n {
                        if let Expr::Name(name) = a.value.as_ref() {
                            if name.id == "self" {
                                fields.insert(a.attr.to_string());
                            }
                        }
                    }
                    skel_children(n, &mut queue);
                }
                qi += 1;
            }
            mf.push((f.name.to_string(), fields));
        }
        let names: Vec<String> = mf.iter().map(|(n, _)| n.clone()).collect();
        // smallest connector removal exposing >= 2 disjoint groups
        for removal in 0..3usize {
            for removed in Self::combinations(&names, removal) {
                let kept: Vec<String> = names.iter().filter(|n| !removed.contains(n)).cloned().collect();
                let groups = Self::connected_groups(&kept, &mf);
                let big: Vec<Vec<String>> = groups
                    .into_iter()
                    .filter(|g| {
                        let fields: HashSet<&String> = g
                            .iter()
                            .flat_map(|m| {
                                mf.iter()
                                    .find(|(n, _)| n == m)
                                    .map(|(_, f)| f.iter())
                                    .unwrap_or_default()
                            })
                            .collect();
                        g.len() >= 2 && fields.len() >= 2
                    })
                    .collect();
                if big.len() >= 2 {
                    return Some((removed, big));
                }
            }
        }
        None
    }

    /// Self::combinations of `names` taken `k` at a time (k <= 2, so O(n^2) max).
    fn combinations(names: &[String], k: usize) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        if k == 0 {
            out.push(Vec::new());
            return out;
        }
        if k == 1 {
            out.extend(names.iter().map(|n| vec![n.clone()]));
            return out;
        }
        for i in 0..names.len() {
            out.extend(((i + 1)..names.len()).map(|j| vec![names[i].clone(), names[j].clone()]));
        }
        out
    }

    /// Methods connected by sharing at least one field — `_connected_groups`.
    fn connected_groups(kept: &[String], mf: &[(String, HashSet<String>)]) -> Vec<Vec<String>> {
        let mut groups: Vec<Vec<String>> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for start in kept {
            if seen.contains(start.as_str()) {
                continue;
            }
            let mut group: Vec<String> = Vec::new();
            let mut stack: Vec<&str> = vec![start];
            while let Some(m) = stack.pop() {
                if !seen.insert(m) {
                    continue;
                }
                group.push(m.to_string());
                let fields = mf
                    .iter()
                    .find(|(n, _)| n == m)
                    .map(|(_, f)| f)
                    .cloned()
                    .unwrap_or_default();
                for other in kept {
                    if seen.contains(other.as_str()) {
                        continue;
                    }
                    let other_fields = mf
                        .iter()
                        .find(|(n, _)| n == other)
                        .map(|(_, f)| f)
                        .cloned()
                        .unwrap_or_default();
                    if !fields.is_disjoint(&other_fields) {
                        stack.push(other);
                    }
                }
            }
            groups.push(group);
        }
        groups
    }

    /// `_partition_findings`: the latent-class field-partition family.
    pub fn findings(state: &mut ScanState, body: &[Stmt], source: &str) {
        for s in body {
            let Stmt::ClassDef(cls) = s else { continue };
            if let Some((connectors, groups)) = Self::partition_for_class(source, cls) {
                let groups_text = groups
                    .iter()
                    .map(|g| format!("{{{}}}", g.join(",")))
                    .collect::<Vec<_>>()
                    .join("/");
                let conn_text = if connectors.is_empty() {
                    "none".to_string()
                } else {
                    format!("{{{}}}", connectors.join(","))
                };
                state.findings.push(Finding { logical_start: None, seam_members: groups.iter().flatten().cloned().collect(),
                col: 0,
                file: state.file.to_string(),
                line: Source::line_of(source, cls.name.range().start()),
                function: cls.name.to_string(),
                kind: "partition".into(),
                severity: "fail".into(),
                message: format!(
                    "methods split into {count} field-disjoint groups ({groups_text}), connectors removed: {conn_text} — each group touches only its own fields, so the class is really {count} independent classes. Split it into {count} classes. — fix: extract-class",
                    count = groups.len()
                ), });
            }
        }
    }
}
