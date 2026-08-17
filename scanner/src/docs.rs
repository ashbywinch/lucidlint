//! The documentation family: links resolve, docs discoverable from AGENTS.md
//! (any number of hops — AGENTS.md links group indexes, not flat lists).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::Finding;

const SKIP_PREFIXES: [&str; 10] = [
    "http://",
    "https://",
    "#",
    "mailto:",
    "tel:",
    "skill://",
    "rule://",
    "agent://",
    "memory://",
    "artifact://",
];

/// Fence-aware line walker shared by both extractions.
fn for_each_fenced_line(text: &str, mut f: impl FnMut(&str)) {
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            f(line);
        }
    }
}

/// `[text](target)` link targets (parent-relative), fences skipped.
fn md_link_targets(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for_each_fenced_line(text, |line| {
        let bytes = line.as_bytes();
        let mut i = 0usize;
        while i + 2 < bytes.len() {
            if bytes[i] == b']' && bytes[i + 1] == b'(' {
                if let Some(end) = line[i + 2..].find(')') {
                    let target = line[i + 2..i + 2 + end].trim().to_string();
                    let target = target.split('#').next().unwrap_or("").to_string();
                    if !target.is_empty() && !SKIP_PREFIXES.iter().any(|p| target.starts_with(p)) {
                        out.push(target);
                    }
                    i += 2 + end + 1;
                    continue;
                }
            }
            i += 1;
        }
    });
    out
}

/// Backticked .md paths (repo-root-relative), fences skipped.
fn md_backtick_paths(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for_each_fenced_line(text, |line| {
        let mut rest = line;
        while let Some(start) = rest.find('`') {
            if let Some(end) = rest[start + 1..].find('`') {
                let inner = &rest[start + 1..start + 1 + end];
                let path = inner.split('#').next().unwrap_or("").to_string();
                if path.ends_with(".md") && path.chars().all(|c| c.is_ascii_alphanumeric() || "_./*-".contains(c)) {
                    out.push(path);
                }
                rest = &rest[start + 1 + end + 1..];
            } else {
                break;
            }
        }
    });
    out
}

/// All references (links + backticks) — the reachability BFS uses both.
fn all_targets(text: &str) -> Vec<String> {
    let mut out = md_link_targets(text);
    out.extend(md_backtick_paths(text));
    out
}

/// The base a backticked path resolves against: `docs/`/`standards/`
/// absolute forms against the repo root, a parent-relative (`../`, `./`)
/// form against the doc's own directory — mirroring markdown links.
fn _backtick_base<'a>(repo: &'a Path, parent: &'a Path, path: &str) -> &'a Path {
    if path.starts_with("../") || path.starts_with("./") {
        parent
    } else {
        repo
    }
}

/// `_docs_actions`: links resolve + docs reachable from AGENTS.md.
pub fn docs_findings(repo: &Path) -> Vec<Finding> {
    let mut mds: Vec<PathBuf> = Vec::new();
    if repo.join("docs").exists() {
        let mut all: Vec<PathBuf> = Vec::new();
        collect_md(repo.join("docs").as_path(), &mut all);
        all.sort();
        mds.extend(all);
    }
    for root in ["README.md", "AGENTS.md"] {
        let p = repo.join(root);
        if p.exists() {
            mds.push(p);
        }
    }
    let mut out = Vec::new();
    for md in &mds {
        let Ok(text) = std::fs::read_to_string(md) else {
            continue;
        };
        let rel = md.strip_prefix(repo).unwrap_or(md).to_string_lossy().replace('\\', "/");
        let parent = md.parent().unwrap_or(repo);
        for target in md_link_targets(&text) {
            if !parent.join(&target).exists() {
                out.push(Finding {
                    file: rel.clone(),
                    line: 0,
                    function: String::new(),
                    kind: "docs-link".into(),
                    severity: "fail".into(),
                    message: format!(
                        "link to '{target}' from {rel} does not resolve — a doc that links nowhere is a finding"
                    ),
                });
            }
        }
        for path in md_backtick_paths(&text) {
            // backtick paths resolve like markdown links: `docs/`- and
            // `standards/`-prefixed paths against the REPO root, a parent-
            // relative (`../`, `./`) path against the DOC'S OWN directory.
            // A bare name (coding-standards.md) is a reference, not a path.
            if !path.starts_with("docs/")
                && !path.starts_with("standards/")
                && !path.starts_with("./")
                && !path.starts_with("../")
            {
                continue;
            }
            if !_backtick_base(repo, parent, &path).join(&path).exists() {
                out.push(Finding {
                    file: rel.clone(),
                    line: 0,
                    function: String::new(),
                    kind: "docs-link".into(),
                    severity: "fail".into(),
                    message: format!(
                        "backtick path '{path}' from {rel} does not resolve — a doc that links nowhere is a finding"
                    ),
                });
            }
        }
    }
    out.extend(docs_reachability(repo));
    out
}

fn collect_md(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_md(&p, out);
            } else if p.extension().is_some_and(|x| x == "md") {
                out.push(p);
            }
        }
    }
}

/// `_docs_reachability_actions`: docs/ files reachable from AGENTS.md.
fn docs_reachability(repo: &Path) -> Vec<Finding> {
    let agents = repo.join("AGENTS.md");
    if !agents.exists() || !repo.join("docs").exists() {
        return Vec::new();
    }
    let mut doc_set: HashSet<String> = HashSet::new();
    let mut docs: Vec<PathBuf> = Vec::new();
    collect_md(&repo.join("docs"), &mut docs);
    docs.sort();
    for d in &docs {
        doc_set.insert(d.strip_prefix(repo).unwrap_or(d).to_string_lossy().replace('\\', "/"));
    }
    let mut links: std::collections::HashMap<String, HashSet<String>> = std::collections::HashMap::new();
    let agents_rel = agents
        .strip_prefix(repo)
        .unwrap_or(&agents)
        .to_string_lossy()
        .replace('\\', "/");
    for md in std::iter::once(&agents).chain(docs.iter()) {
        let Ok(text) = std::fs::read_to_string(md) else {
            continue;
        };
        let src = md.strip_prefix(repo).unwrap_or(md).to_string_lossy().replace('\\', "/");
        let parent = md.parent().unwrap_or(repo);
        let mut targets = HashSet::new();
        for target in all_targets(&text) {
            let cand_abs = parent.join(&target);
            if let Ok(rel) = cand_abs.canonicalize() {
                if let Ok(rel) = rel.strip_prefix(repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf())) {
                    let cand = rel.to_string_lossy().replace('\\', "/");
                    if doc_set.contains(&cand) {
                        targets.insert(cand);
                    }
                }
            }
        }
        links.insert(src, targets);
    }
    let mut reachable: HashSet<String> = HashSet::from([agents_rel]);
    loop {
        let frontier: HashSet<String> = reachable
            .iter()
            .flat_map(|src| links.get(src).cloned().unwrap_or_default())
            .collect();
        if frontier.is_subset(&reachable) {
            break;
        }
        reachable.extend(frontier);
    }
    let mut out = Vec::new();
    for d in &docs {
        let rel = d.strip_prefix(repo).unwrap_or(d).to_string_lossy().replace('\\', "/");
        if !reachable.contains(&rel) {
            out.push(Finding {
                file: "AGENTS.md".into(),
                line: 0,
                function: String::new(),
                kind: "docs-undiscoverable".into(),
                severity: "fail".into(),
                message: format!(
                    "doc '{rel}' is not reachable from AGENTS.md at any hop — a doc the reader cannot reach from where everyone starts does not exist. Link it from its group's index"
                ),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_targets_extract_and_skip_fences() {
        let text = "see [here](docs/PRD.md) and [x](skill://foo)\n```\n[fence](nope.md)\n```\n[a](after.md)\n";
        let targets = md_link_targets(text);
        assert_eq!(targets, vec!["docs/PRD.md", "after.md"]); // skill:// skipped, fence skipped
    }

    #[test]
    fn backtick_paths_extract_outside_fences() {
        let text = "use `docs/PRD.md` and \n```\n`docs/PLAN.md`\n```\n";
        let paths = md_backtick_paths(text);
        assert_eq!(paths, vec!["docs/PRD.md"]); // the fenced one is skipped
    }

    #[test]
    fn anchors_are_stripped() {
        let targets = md_link_targets("[go](docs/PRD.md#goals)");
        assert_eq!(targets, vec!["docs/PRD.md"]);
    }

    #[test]
    fn broken_link_is_found_in_docs_findings() {
        let dir = std::env::temp_dir().join(format!("docs_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("docs/guide.md"), "see [missing](nope.md)\n").unwrap();
        let f = docs_findings(&dir);
        assert!(f.iter().any(|x| x.kind == "docs-link" && x.message.contains("nope.md")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backtick_path_resolves_against_repo_root_not_parent() {
        let dir = std::env::temp_dir().join(format!("docs_root_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("docs/PRD.md"), "").unwrap();
        // docs/guide.md backticks docs/PRD.md — resolves at the REPO root
        std::fs::write(dir.join("docs/guide.md"), "see `docs/PRD.md`\n").unwrap();
        let f = docs_findings(&dir);
        assert!(!f.iter().any(|x| x.kind == "docs-link"), "{f:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parent_relative_backtick_resolves_against_doc_parent() {
        // a doc at docs/plans/guide.md backticks `../prd/PRD.md` — the
        // parent-relative path must resolve against docs/plans/.. (== docs/),
        // exactly like a markdown link. The current code resolves backtick
        // paths against the REPO root, so `repo/../prd/PRD.md` misses the
        // existing docs/prd/PRD.md and fires a false positive.
        let dir = std::env::temp_dir().join(format!("docs_paren_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs/plans")).unwrap();
        std::fs::create_dir_all(dir.join("docs/prd")).unwrap();
        std::fs::write(dir.join("docs/prd/PRD.md"), "").unwrap();
        std::fs::write(dir.join("docs/plans/guide.md"), "see `../prd/PRD.md`\n").unwrap();
        let f = docs_findings(&dir);
        assert!(
            !f.iter().any(|x| x.kind == "docs-link"),
            "parent-relative backtick path must resolve: {f:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn orphan_doc_is_undiscoverable() {
        let dir = std::env::temp_dir().join(format!("docs_orphan_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        // AGENTS.md exists but links nothing; the doc is unreachable
        std::fs::write(dir.join("AGENTS.md"), "# agent\n").unwrap();
        std::fs::write(dir.join("docs/lost.md"), "orphan\n").unwrap();
        let f = docs_findings(&dir);
        assert!(f
            .iter()
            .any(|x| x.kind == "docs-undiscoverable" && x.message.contains("lost.md")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reachable_doc_via_index_passes() {
        let dir = std::env::temp_dir().join(format!("docs_reach_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "see [index](docs/index.md)\n").unwrap();
        std::fs::write(dir.join("docs/index.md"), "see [guide](guide.md)\n").unwrap();
        std::fs::write(dir.join("docs/guide.md"), "reachable\n").unwrap();
        let f = docs_findings(&dir);
        assert!(!f.iter().any(|x| x.kind == "docs-undiscoverable"), "{f:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
