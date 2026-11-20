//! One graph node — the record every family scores or groups.

use std::collections::HashMap;

use serde::Deserialize;

#[derive(Deserialize)]
pub struct GNode {
    pub kind: String,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub line_start: Option<i64>,
    pub community_id: Option<i64>,
}

impl GNode {
    /// The graph tool's own risk formula — recomputed from CALLS/TESTED_BY.
    /// A method: the record-shape rule's advice — the risk rules ARE the
    /// struct's behavior, not a free function's.
    /// The risk formula's bars and weights: a hub caller count, a warm count,
    /// and the three state weights (hub, untested, security-sensitive).
    const HUB_CALLERS: usize = 10;
    const WARM_CALLERS: usize = 3;
    const RISK_HUB: f64 = 0.3;
    const RISK_WARM: f64 = 0.15;
    const RISK_UNTESTED: f64 = 0.3;
    const RISK_SENSITIVE: f64 = 0.4;
    const RISK_MAX: f64 = 1.0;

    pub(super) fn risk(&self, caller_counts: &HashMap<String, usize>, tested_counts: &HashMap<String, usize>) -> f64 {
        let caller_count = caller_counts.get(&self.qualified_name).copied().unwrap_or(0);
        let tested = tested_counts.get(&self.qualified_name).copied().unwrap_or(0) > 0;
        let mut risk: f64 = 0.0;
        if caller_count > GNode::HUB_CALLERS {
            risk += GNode::RISK_HUB;
        } else if caller_count > GNode::WARM_CALLERS {
            risk += GNode::RISK_WARM;
        }
        if !tested {
            risk += GNode::RISK_UNTESTED;
        }
        let name_lower = self.name.to_lowercase();
        let sec_kw = [
            "auth",
            "login",
            "password",
            "token",
            "session",
            "crypt",
            "secret",
            "credential",
            "permission",
            "sql",
            "execute",
        ];
        if sec_kw.iter().any(|kw| name_lower.contains(kw)) {
            risk += GNode::RISK_SENSITIVE;
        }
        risk.min(GNode::RISK_MAX)
    }
}
