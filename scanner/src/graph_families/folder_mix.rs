//! `folder-mix`: a folder whose files split across graph communities.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::finding::Finding;

use super::contract::GraphContract;
use super::graph_paths::GraphPaths;

pub(crate) struct FolderMix;

/// How many names the message shows, and the file count that makes a folder a
/// mix.
const NAMES_SHOWN: usize = 3;
const FILES_MIN: usize = 5;

impl FolderMix {
    /// Folder grab bags — `_folder_mix_actions` / `_folder_mix_for_dir`.
    pub fn findings(repo: &Path, contract: &GraphContract) -> Vec<Finding> {
        // best community per file: the max count, first-wins on tie — Python's
        // dict-iteration order with a strict > comparison
        //
        // The key fix: count PER COMMUNITY per file, not a single cumulative
        // counter. The previous code stored (cumulative_total, community) and
        // compared totals — the LAST node's community always won, not the max.
        let mut best: HashMap<String, i64> = HashMap::new(); // (community) per file
        let mut per_cc: HashMap<(String, i64), usize> = HashMap::new(); // (file, community) -> count
        for n in &contract.nodes {
            let Some(cid) = n.community_id else { continue };
            if !n.file_path.ends_with(".py") {
                continue;
            }
            let file = GraphPaths::repo_rel(repo, &n.file_path);
            let key = (file.clone(), cid);
            let cnt = per_cc.entry(key).or_insert(0);
            *cnt += 1;
            // keep the max count community; first-wins on tie via strict >
            match best.get(&file) {
                None => {
                    best.insert(file, cid);
                }
                Some(_)
                    if *cnt
                        > *per_cc
                            .get(&(file.clone(), best.get(&file).copied().unwrap_or_default()))
                            .unwrap_or(&0) =>
                {
                    best.insert(file, cid);
                }
                Some(_) => {}
            }
        }
        let mut dirs: BTreeMap<String, Vec<(String, i64)>> = BTreeMap::new();
        for (fp, cid) in best {
            let parts: Vec<&str> = fp.split('/').collect();
            if parts.len() < 2 {
                continue;
            }
            let dir = parts[..parts.len() - 1].join("/");
            if let Some(last) = parts.last() {
                dirs.entry(dir).or_default().push((last.to_string(), cid));
            }
        }
        let mut out = Vec::new();
        for (d, files) in dirs {
            let rel = GraphPaths::repo_rel(repo, &d);
            if !Self::is_spread_target(&files, &rel) {
                continue;
            }
            let mut spread: BTreeMap<i64, Vec<String>> = BTreeMap::new();
            for (f, cid) in &files {
                spread.entry(*cid).or_default().push(f.clone());
            }
            let big: Vec<(i64, Vec<String>)> = spread.into_iter().filter(|(_, fns)| fns.len() >= 2).collect();
            if big.len() < 2 {
                continue;
            }
            let groups = big
                .iter()
                .take(NAMES_SHOWN)
                .map(|(cid, fns)| {
                    let name = contract
                        .communities
                        .get(&cid.to_string())
                        .cloned()
                        .unwrap_or_else(|| cid.to_string());
                    format!(
                        "{name} ({})",
                        fns.iter().take(4).cloned().collect::<Vec<_>>().join(", ")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            out.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                file: rel.clone(),
                line: 0,
                col: 0,
                function: String::new(),
                kind: "folder-mix".into(),
                severity: "fail".into(),
                message: format!(
                    "folder '{rel}' has {} files split across {} graph communities: {groups}",
                    files.len(),
                    big.len()
                ),
            });
        }
        out
    }

    /// The file is a directory-level scan target with enough files to spread
    /// across folders.
    fn is_spread_target(files: &[(String, i64)], rel: &str) -> bool {
        files.len() >= FILES_MIN && !rel.starts_with("tests") && !rel.is_empty() && rel != "."
    }
}
