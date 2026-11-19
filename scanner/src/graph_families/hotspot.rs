//! `hotspot`: files that change often AND are complex (churn + max CC).

use std::collections::HashMap;

use crate::finding::Finding;

pub(crate) struct Hotspot;

impl Hotspot {
    /// Hotspot: files that change often AND are complex (churn + max CC).
    pub fn findings(
        churn: &HashMap<String, usize>,
        cc_by_file: &HashMap<String, u32>,
        top_frac: f64,
        min_cc: u32,
    ) -> Vec<Finding> {
        let cutoff = ((churn.len() as f64) * top_frac).max(1.0) as usize;
        let mut entries: Vec<(&String, &usize)> = churn.iter().collect();
        entries.sort_by_key(|(f, c)| (*c, (*f).clone()));
        entries.reverse();
        let mut out = Vec::new();
        for (rel, count) in entries.into_iter().take(cutoff) {
            if *count < 2 {
                continue;
            }
            let max_cc = cc_by_file.get(rel).copied().unwrap_or(0);
            if max_cc < min_cc {
                continue;
            }
            out.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                file: rel.clone(),
                line: 1,
                col: 0,
                function: String::new(),
                kind: "hotspot".into(),
                severity: "fail".into(),
                message: format!("changed {count}x (top {cutoff} by churn) — volatile part: max CC {max_cc} in {rel}"),
            });
        }
        out
    }
}
