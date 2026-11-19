//! `high-risk`: the graph's scored nodes — many callers, no test, or a
//! security-sensitive name.

use std::collections::HashMap;
use std::path::Path;

use crate::finding::Finding;

use super::contract::GraphContract;
use super::graph_paths::GraphPaths;
use super::node::GNode;

pub(crate) struct HighRisk;

impl HighRisk {
    /// High-risk nodes — `_high_risk_actions` (risk recomputed, order/limit kept).
    pub fn findings(repo: &Path, contract: &GraphContract, max_risk: f64, include_tests: bool) -> Vec<Finding> {
        let mut caller_counts: HashMap<String, usize> = HashMap::new();
        let mut tested_counts: HashMap<String, usize> = HashMap::new();
        for e in &contract.edges {
            if e.kind == "CALLS" {
                *caller_counts.entry(e.target.clone()).or_default() += 1;
            } else if e.kind == "TESTED_BY" {
                *tested_counts.entry(e.source.clone()).or_default() += 1;
            }
        }
        let mut scored: Vec<(&GNode, f64, usize, bool)> = Vec::new();
        for n in &contract.nodes {
            if !matches!(n.kind.as_str(), "Function" | "Class" | "Test") {
                continue;
            }
            if !n.file_path.ends_with(".py") {
                continue;
            }
            let risk = n.risk(&caller_counts, &tested_counts);
            if risk < max_risk {
                continue;
            }
            let rel = GraphPaths::repo_rel(repo, &n.file_path);
            if !include_tests && GraphPaths::is_test_rel(&rel) {
                continue;
            }
            let caller_count = caller_counts.get(&n.qualified_name).copied().unwrap_or(0);
            let tested = tested_counts.get(&n.qualified_name).copied().unwrap_or(0) > 0;
            scored.push((n, risk, caller_count, tested));
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(50);
        let mut out = Vec::new();
        for (n, risk, caller_count, _tested) in scored {
            let line = n.line_start.unwrap_or(1) as usize;
            out.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
        file: GraphPaths::repo_rel(repo, &n.file_path),
        line,
        function: n.name.clone(),
        kind: "high-risk".into(),
        severity: "fail".into(),
        message: format!("high risk score {risk:.2} (>= {max_risk}) — many callers, no test coverage, or a security-sensitive name; {caller_count} call site(s)"), });
        }
        out
    }
}
