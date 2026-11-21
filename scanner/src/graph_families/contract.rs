//! The graph contract and its edge record, plus the two lookups the mix
//! families ask of it.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::Deserialize;

use super::graph_paths::GraphPaths;
use super::node::GNode;

#[derive(Deserialize)]
pub struct GEdge {
    pub kind: String,
    pub source: String,
    pub target: String,
    pub file_path: String,
}

#[derive(Deserialize)]
pub struct GraphContract {
    pub contract_version: i64,
    pub nodes: Vec<GNode>,
    pub edges: Vec<GEdge>,
    #[serde(default)]
    pub communities: HashMap<String, String>,
}

impl GraphContract {
    /// The most-called external subsystem of a function — `_dominant_callee`.
    pub(super) fn dominant_callee(
        &self,
        repo: &Path,
        qn: &str,
        own_rel: &str,
        node_by_qn: &HashMap<String, &GNode>,
    ) -> Option<String> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        let own_mod = GraphPaths::module_key(repo, own_rel);
        for e in &self.edges {
            if e.kind != "CALLS" || e.source != qn {
                continue;
            }
            let callee_mod = self.resolve_callee_module(repo, &e.target, node_by_qn);
            if let Some(m) = callee_mod {
                if m != own_mod {
                    *counts.entry(m).or_default() += 1;
                }
            }
        }
        counts.into_iter().max_by_key(|(_, c)| *c).map(|(m, _)| m)
    }

    /// A callee target's module — the qualified-name map is the boundary;
    /// GNode only ever appears as its value type.
    pub(super) fn resolve_callee_module(
        &self,
        repo: &Path,
        target: &str,
        node_by_qn: &HashMap<String, &GNode>,
    ) -> Option<String> {
        if let Some(n) = node_by_qn.get(target) {
            return Some(GraphPaths::module_key(repo, &n.file_path));
        }
        let Some((prefix, _)) = target.split_once("::") else {
            return None; // a bare method name places no call
        };
        // The exporter prefixes an unresolved target with the file it was seen
        // in (`<file>::new`, `<file>::to_string`). A stdlib call is not a layer
        // boundary: it resolves to its own file, which the caller's module
        // check then ignores.
        if Path::new(prefix).is_absolute() {
            return Some(GraphPaths::module_key(repo, prefix));
        }
        // A bare name is evidence only when it names ONE node. `new` and
        // `visit_expr` collide across the tree; picking the first match
        // invents a layer the file does not have (every `Vec::new()` was
        // attributed to whichever file declared a `new` first).
        let name = GraphPaths::base_name(target);
        let mut found = self.nodes.iter().filter(|n| n.name == name);
        let first = found.next()?;
        if found.next().is_some() {
            return None;
        }
        Some(GraphPaths::module_key(repo, &first.file_path))
    }
}
