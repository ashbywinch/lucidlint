//! The documentation family: links resolve, docs discoverable from AGENTS.md
//! (any number of hops — AGENTS.md links group indexes, not flat lists).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::Finding;

const SKIP_PREFIXES: [&str; 10] = [
    "http://", "https://", "#", "mailto:", "tel:", "skill://", "rule://", "agent://",
    "memory://", "artifact://",
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

/// `_docs_actions`: links resolve + docs reachable from AGENTS.md.
pub fn docs_findings(repo: &Path) -> Vec<Finding> {
    let mut mds: Vec<PathBuf> = Vec::new();
    if repo.join("docs").exists() {
        if let Ok(entries) = std::fs::read_dir(repo.join("docs")) {
            let mut all: Vec<PathBuf> = Vec::new();
            collect_md(repo.join("docs").as_path(), &mut all);
            all.sort();
            mds.extend(all);
        }
    }
    for root in ["README.md", "AGENTS.md"] {
        let p = repo.join(root);
        if p.exists() {
            mds.push(p);
        }
    }
    let mut out = Vec::new();
    for md in &mds {
        let Ok(text) = std::fs::read_to_string(md) else { continue };
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
                    message: format!("link to '{target}' from {rel} does not resolve — a doc that links nowhere is a finding"),
                });
            }
        }
        for path in md_backtick_paths(&text) {
            // backtick paths resolve against the REPO root; a bare name
            // (coding-standards.md) is a reference, not a path
            if !path.starts_with("docs/")
                && !path.starts_with("standards/")
                && !path.starts_with("./")
                && !path.starts_with("../")
            {
                continue;
            }
            if !repo.join(&path).exists() {
                out.push(Finding {
                    file: rel.clone(),
                    line: 0,
                    function: String::new(),
                    kind: "docs-link".into(),
                    severity: "fail".into(),
                    message: format!("backtick path '{path}' from {rel} does not resolve — a doc that links nowhere is a finding"),
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
        let Ok(text) = std::fs::read_to_string(md) else { continue };
        let src = md
            .strip_prefix(repo)
            .unwrap_or(md)
            .to_string_lossy()
            .replace('\\', "/");
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
