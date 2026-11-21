//! `layer-mix`: one file whose functions mostly call into different layers.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::finding::Finding;

use super::contract::GraphContract;
use super::graph_paths::GraphPaths;
use super::node::GNode;

pub(crate) struct LayerMix;

/// The function count that makes a layer boundary real, and how many names the
/// message shows.
const FNS_MIN: usize = 6;
const NAMES_SHOWN: usize = 3;

impl LayerMix {
    /// File mixes layers — `_layer_mix_actions` / `_layer_mix_for_file`.
    pub fn findings(repo: &Path, contract: &GraphContract, files: &[String]) -> Vec<Finding> {
        let node_by_qn: HashMap<String, &GNode> =
            contract.nodes.iter().map(|n| (n.qualified_name.clone(), n)).collect();
        let mut out = Vec::new();
        for rel in files {
            if GraphPaths::is_test_rel(rel) {
                continue;
            }
            let abs = format!("{}/{}", repo.to_string_lossy(), rel);
            let fns: Vec<&GNode> = contract
                .nodes
                .iter()
                .filter(|n| n.file_path == abs && matches!(n.kind.as_str(), "Function" | "Method"))
                .collect();
            if fns.len() < FNS_MIN {
                continue;
            }
            let mut layers: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for n in fns {
                if let Some(layer) = contract.dominant_callee(repo, &n.qualified_name, rel, &node_by_qn) {
                    layers
                        .entry(layer)
                        .or_default()
                        .push(GraphPaths::base_name(&n.qualified_name).to_string());
                }
            }
            let big: Vec<(String, Vec<String>)> = layers.into_iter().filter(|(_, names)| names.len() >= 2).collect();
            if big.len() < 2 {
                continue;
            }
            let groups = big
                .iter()
                .take(NAMES_SHOWN)
                .map(|(m, names)| format!("{m} ({})", names.iter().take(4).cloned().collect::<Vec<_>>().join(", ")))
                .collect::<Vec<_>>()
                .join(", ");
            let metric: usize = big.iter().map(|(_, n)| n.len()).sum();
            let _ = metric;
            out.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                file: rel.clone(),
                line: 0,
                col: 0,
                function: String::new(),
                kind: "layer-mix".into(),
                severity: "fail".into(),
                message: format!("file '{rel}' mixes layers: {groups}"),
            });
        }
        out
    }
}
