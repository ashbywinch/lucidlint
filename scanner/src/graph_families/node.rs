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
    pub(super) fn risk(&self, caller_counts: &HashMap<String, usize>, tested_counts: &HashMap<String, usize>) -> f64 {
        let caller_count = caller_counts.get(&self.qualified_name).copied().unwrap_or(0);
        let tested = tested_counts.get(&self.qualified_name).copied().unwrap_or(0) > 0;
        let mut risk: f64 = 0.0;
        if caller_count > 10 {
            risk += 0.3;
        } else if caller_count > 3 {
            risk += 0.15;
        }
        if !tested {
            risk += 0.3;
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
            risk += 0.4;
        }
        risk.min(1.0)
    }
}
