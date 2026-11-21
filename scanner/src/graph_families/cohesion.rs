//! `module-cohesion`: one module whose nodes split across graph communities.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::finding::Finding;

use super::contract::GraphContract;
use super::graph_paths::GraphPaths;

pub(crate) struct Cohesion;

/// How many shared names the message shows.
const NAMES_SHOWN: usize = 3;

impl Cohesion {
    /// Module cohesion — `_module_cohesion_actions` (review-log §11.1, fail).
    /// A file whose nodes split across >= 2 graph communities (each >= 2 nodes)
    /// holds several sub-domains — a module-split candidate. The community
    /// detection is the domain grouping (the same signal folder-mix uses, at
    /// file granularity), so the finding names the exact seam the extract-module
    /// fix moves: the communities and their members. The edge bar keeps small or
    /// lightly-coupled modules out of the gate.
    pub fn findings(repo: &Path, contract: &GraphContract, max_edges: usize) -> Vec<Finding> {
        // edge count per REPO-RELATIVE file (keys are abs paths)
        let mut edge_counts: HashMap<String, usize> = HashMap::new();
        for e in &contract.edges {
            if !matches!(e.kind.as_str(), "CALLS" | "IMPORTS_FROM" | "INHERITS" | "REFERENCES") {
                continue;
            }
            if !e.file_path.ends_with(".py") {
                continue;
            }
            *edge_counts.entry(GraphPaths::repo_rel(repo, &e.file_path)).or_default() += 1;
        }
        let mut per_file: BTreeMap<String, BTreeMap<i64, Vec<String>>> = BTreeMap::new();
        for n in &contract.nodes {
            let Some(cid) = n.community_id else { continue };
            if !n.file_path.ends_with(".py") {
                continue;
            }
            per_file
                .entry(GraphPaths::repo_rel(repo, &n.file_path))
                .or_default()
                .entry(cid)
                .or_default()
                .push(n.name.clone());
        }
        let mut out = Vec::new();
        for (rel, communities) in per_file {
            if rel.is_empty() || GraphPaths::is_test_rel(&rel) {
                continue;
            }
            let edge_count = edge_counts.get(&rel).copied().unwrap_or(0);
            if edge_count < max_edges {
                continue;
            }
            let domains: Vec<(i64, Vec<String>)> = communities
                .into_iter()
                .filter(|(_, members)| members.len() >= 2)
                .collect();
            if domains.len() < 2 {
                continue;
            }
            let groups = domains
                .iter()
                .take(NAMES_SHOWN)
                .map(|(cid, members)| {
                    let name = contract
                        .communities
                        .get(&cid.to_string())
                        .cloned()
                        .unwrap_or_else(|| cid.to_string());
                    format!(
                        "{name} ({})",
                        members.iter().take(4).cloned().collect::<Vec<_>>().join(", ")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            out.push(Finding { logical_start: None, seam_members: Vec::new(), file: rel.clone(), line: 0, col: 0, function: String::new(), kind: "module-cohesion".into(), severity: "fail".into(), message: format!(
            "module '{rel}' holds {} domains of >= 2 nodes ({edge_count} edges): {groups} — split the module at the domain seams — fix: extract-module --fix-name <module> --params <members>",
            domains.len()
        ) });
        }
        out
    }
}
