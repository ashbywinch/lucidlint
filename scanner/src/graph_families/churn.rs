//! `churn-untested`: the top churn with no covered line in the repo's own
//! coverage snapshot.

use std::collections::HashMap;

use crate::finding::Finding;

use super::coverage::CoverageSnapshot;
use super::graph_paths::GraphPaths;

pub(crate) struct ChurnUntested;

/// The smallest cut-off a top-fraction ranking may pick.
const MIN_CUTOFF: f64 = 1.0;

impl ChurnUntested {
    /// A file a test can exercise — the only population the churn-untested rule
    /// can make a coverage claim about. Docs, skills and config are churned but
    /// have no tests to lack, so they are never in the population.
    fn is_testable_code(rel: &str) -> bool {
        rel.ends_with(".py") || rel.ends_with(".rs")
    }

    /// Churn-untested: the top fraction by churn, limited to testable code files,
    /// whose OWN coverage snapshot shows no covered line — volatile AND
    /// unverified. Test status comes from the repo's data (coverage.xml, else
    /// `.coverage`), never from the graph's TESTED_BY approximation: that edge
    /// misses tests that import a module in a body, and it has no opinion about
    /// Rust at all (R6). A stale snapshot reports "verify" as a warning.
    pub fn findings(
        churn: &HashMap<String, usize>,
        coverage: Option<&CoverageSnapshot>,
        top_frac: f64,
    ) -> Vec<Finding> {
        let Some(cov) = coverage else {
            // no snapshot at all: every file's test status is UNKNOWN — the rule
            // has nothing true to say (R6), so it says nothing
            return Vec::new();
        };
        let mut entries: Vec<(&String, &usize)> = churn
            .iter()
            .filter(|(rel, _)| Self::is_testable_code(rel) && !GraphPaths::is_test_rel(rel))
            .collect();
        entries.sort_by_key(|(f, c)| (*c, (*f).clone()));
        entries.reverse();
        let cutoff = ((entries.len() as f64) * top_frac).max(MIN_CUTOFF) as usize;
        let mut out = Vec::new();
        for (rel, count) in entries.into_iter().take(cutoff) {
            if *count < 2 {
                continue; // one change isn't volatile — same gate as hotspot
            }
            let Some(lines) = cov.covered.get(rel) else {
                continue; // absent from the snapshot = UNKNOWN, never uncovered
            };
            if !lines.is_empty() {
                continue; // a covered line is evidence the tests exercise the file
            }
            let source = cov.source.split(' ').next().unwrap_or("the coverage snapshot");
            let (severity, message) = if cov.stale {
                (
                "warn",
                format!(
                    "changed {count}x (top {cutoff} by churn) and {source} shows no covered line — the snapshot predates the repo's tests: re-run coverage, then verify a test exercises it"
                ),
            )
            } else {
                (
                "fail",
                format!(
                    "changed {count}x (top {cutoff} by churn) and {source} shows no covered line — volatile and unverified: add a test that exercises it"
                ),
            )
            };
            out.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                file: rel.clone(),
                line: 1,
                col: 0,
                function: String::new(),
                kind: "churn-untested".into(),
                severity: severity.into(),
                message,
            });
        }
        out
    }
}
