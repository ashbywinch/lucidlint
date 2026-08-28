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

/// A path's repo-relative form ("/"-joined), for the gitignored membership
/// check — the orchestrator passes git-ignored rels (review-log R3).
fn repo_rel<'a>(repo: &'a Path, p: &'a Path) -> Option<String> {
    p.strip_prefix(repo)
        .ok()
        .map(|r| r.to_string_lossy().replace('\\', "/"))
}

/// Is a link/backtick target one git deliberately keeps out of the repo (a
/// private doc)? Such a target is intentionally absent — not a broken link.
fn is_gitignored_target(repo: &Path, abs: &Path, gitignored: &HashSet<String>) -> bool {
    repo_rel(repo, abs)
        .map(|rel| gitignored.contains(&rel))
        .unwrap_or(false)
}

/// A link/backtick target is a finding when it does not resolve — unless git
/// deliberately excludes it (a private doc). One place for both spellings so
/// docs_findings stays under the CC gate (review-log R1 self-discipline).
// lucidlint: ignore long-param-list one shared link checker — a ctx object for loose values is ceremony
fn check_target(
    repo: &Path,
    gitignored: &HashSet<String>,
    out: &mut Vec<Finding>,
    abs: &Path,
    target: &str,
    rel: &str,
    what: &str,
) {
    if is_gitignored_target(repo, abs, gitignored) {
        return; // intentionally private — not a broken link
    }
    if !abs.exists() {
        out.push(Finding {
            file: rel.to_string(),
            line: 0,
            col: 0,
            function: String::new(),
            kind: "docs-link".into(),
            severity: "fail".into(),
            message: format!("{what} '{target}' from {rel} does not resolve — a doc that links nowhere is a finding"),
        });
    }
}

/// `_docs_actions`: links resolve + docs reachable from AGENTS.md.
/// `gitignored` carries the repo-relative paths git ignores — a gitignored
/// doc is intentionally private (not shipped), so it neither breaks a link
/// nor counts as an undiscoverable doc.
pub fn docs_findings(repo: &Path, gitignored: &HashSet<String>) -> Vec<Finding> {
    let mut mds: Vec<PathBuf> = Vec::new();
    if repo.join("docs").exists() {
        let mut all: Vec<PathBuf> = Vec::new();
        collect_md(repo.join("docs").as_path(), &mut all, repo, gitignored);
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
            check_target(
                repo,
                gitignored,
                &mut out,
                &parent.join(&target),
                &target,
                &rel,
                "link to",
            );
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
            check_target(
                repo,
                gitignored,
                &mut out,
                &_backtick_base(repo, parent, &path).join(&path),
                &path,
                &rel,
                "backtick path",
            );
        }
    }
    out.extend(docs_reachability(repo, gitignored));
    out
}

/// Recursively collect the .md files under `dir`, skipping files git ignores
/// (private docs are not part of the shipped doc set).
fn collect_md(dir: &Path, out: &mut Vec<PathBuf>, repo: &Path, gitignored: &HashSet<String>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_md(&p, out, repo, gitignored);
            } else if p.extension().is_some_and(|x| x == "md") {
                let skip = repo_rel(repo, &p).map(|rel| gitignored.contains(&rel)).unwrap_or(false);
                if !skip {
                    out.push(p);
                }
            }
        }
    }
}

/// Resolve one target string against its base and add it to `targets` when
/// it names a doc in the set — the reachability edge.
fn reachability_edge(
    repo: &Path,
    doc_set: &HashSet<String>,
    targets: &mut HashSet<String>,
    cand_abs: std::path::PathBuf,
) {
    if let Ok(rel) = cand_abs.canonicalize() {
        if let Ok(rel) = rel.strip_prefix(repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf())) {
            let cand = rel.to_string_lossy().replace('\\', "/");
            if doc_set.contains(&cand) {
                targets.insert(cand);
            }
        }
    }
}

/// `_docs_reachability_actions`: docs/ files reachable from AGENTS.md.
fn docs_reachability(repo: &Path, gitignored: &HashSet<String>) -> Vec<Finding> {
    let agents = repo.join("AGENTS.md");
    if !agents.exists() || !repo.join("docs").exists() {
        return Vec::new();
    }
    let mut doc_set: HashSet<String> = HashSet::new();
    let mut docs: Vec<PathBuf> = Vec::new();
    collect_md(&repo.join("docs"), &mut docs, repo, gitignored);
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
        for target in md_link_targets(&text) {
            reachability_edge(repo, &doc_set, &mut targets, parent.join(&target));
        }
        for path in md_backtick_paths(&text) {
            // backtick targets resolve like docs_findings: a `docs/`/
            // `standards/` form against the REPO root, anything else (a
            // parent-relative or bare name) against the doc's own directory.
            // The bare-name case is a prose reference — the doc it names is
            // discoverable from here even though docs_findings does not
            // link-check it.
            let cand_abs = if path.starts_with("docs/") || path.starts_with("standards/") {
                repo.join(&path)
            } else {
                parent.join(&path)
            };
            reachability_edge(repo, &doc_set, &mut targets, cand_abs);
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
    let mut unreachable: Vec<String> = Vec::new();
    for d in &docs {
        let rel = d.strip_prefix(repo).unwrap_or(d).to_string_lossy().replace('\\', "/");
        if !reachable.contains(&rel) {
            unreachable.push(rel);
        }
    }
    if unreachable.is_empty() {
        return Vec::new();
    }
    // ONE finding naming them all — the agent fixes one finding per run, so
    // per-doc findings would make it re-run once per unreachable doc and
    // never see the tail (each re-run hides one behind a suppression).
    let mut message = format!(
        "{} doc(s) not reachable from AGENTS.md at any hop — a doc the reader cannot reach from where everyone starts does not exist: ",
        unreachable.len()
    );
    message.push_str(&unreachable.join(", "));
    message.push_str(". Link each from its group's index");
    out.push(Finding {
        file: "AGENTS.md".into(),
        line: 0,
        col: 0,
        function: String::new(),
        kind: "docs-undiscoverable".into(),
        severity: "fail".into(),
        message,
    });
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
        let f = docs_findings(&dir, &std::collections::HashSet::new());
        assert!(f.iter().any(|x| x.kind == "docs-link" && x.message.contains("nope.md")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gitignored_link_target_is_intentionally_absent() {
        // R3 (review log §3.6): a doc target git ignores is intentionally
        // private — a link to it is NOT a broken-link finding.
        let dir = std::env::temp_dir().join(format!("docs_test_{}_link", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("guide.md"), "see [private](docs/PRIVATE.md)\n").unwrap();
        let ignored: std::collections::HashSet<String> =
            std::collections::HashSet::from(["docs/PRIVATE.md".to_string()]);
        let f = docs_findings(&dir, &ignored);
        assert!(!f.iter().any(|x| x.kind == "docs-link"), "{f:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gitignored_private_doc_is_not_undiscoverable() {
        // R3: a gitignored doc present on disk but unreachable from AGENTS.md
        // is private material — it must not be flagged as an undiscoverable
        // shipped doc (without the gitignored set it IS, which was the bug).
        let dir = std::env::temp_dir().join(format!("docs_test_{}_priv", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "# agents\n").unwrap();
        std::fs::write(dir.join("docs/PRIVATE.md"), "private\n").unwrap();
        let ignored: std::collections::HashSet<String> =
            std::collections::HashSet::from(["docs/PRIVATE.md".to_string()]);
        let f = docs_findings(&dir, &ignored);
        assert!(!f.iter().any(|x| x.kind == "docs-undiscoverable"), "{f:?}");
        // control: without the gitignore knowledge the finding fires
        let f2 = docs_findings(&dir, &std::collections::HashSet::new());
        assert!(f2.iter().any(|x| x.kind == "docs-undiscoverable"), "{f2:?}");
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
        let f = docs_findings(&dir, &std::collections::HashSet::new());
        assert!(!f.iter().any(|x| x.kind == "docs-link"), "{f:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parent_relative_backtick_resolves_against_doc_parent() {
        // a doc at docs/plans/guide.md backticks `../prd/PRD.md` — the
        // parent-relative path must resolve against docs/plans/.. (== docs/),
        // exactly like a markdown link. A repo-root-relative resolution
        // (`repo/../prd/PRD.md`) misses the existing docs/prd/PRD.md and
        // fires a false positive.
        let dir = std::env::temp_dir().join(format!("docs_paren_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs/plans")).unwrap();
        std::fs::create_dir_all(dir.join("docs/prd")).unwrap();
        std::fs::write(dir.join("docs/prd/PRD.md"), "").unwrap();
        std::fs::write(dir.join("docs/plans/guide.md"), "see `../prd/PRD.md`\n").unwrap();
        let f = docs_findings(&dir, &std::collections::HashSet::new());
        assert!(
            !f.iter().any(|x| x.kind == "docs-link"),
            "parent-relative backtick path must resolve: {f:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn all_unreachable_docs_are_enumerated_in_one_finding() {
        // the agent fixes one finding at a time; per-doc findings would make
        // it re-run once per unreachable doc — one finding names them all
        let dir = std::env::temp_dir().join(format!("docs_enum_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "# agents\n").unwrap();
        std::fs::write(dir.join("docs/ALPHA.md"), "a\n").unwrap();
        std::fs::write(dir.join("docs/BETA.md"), "b\n").unwrap();
        std::fs::write(dir.join("docs/GAMMA.md"), "c\n").unwrap();
        let f = docs_findings(&dir, &std::collections::HashSet::new());
        let u: Vec<&Finding> = f.iter().filter(|x| x.kind == "docs-undiscoverable").collect();
        assert_eq!(u.len(), 1, "{f:?}");
        assert!(
            u[0].message.contains("docs/ALPHA.md")
                && u[0].message.contains("docs/BETA.md")
                && u[0].message.contains("docs/GAMMA.md"),
            "one finding must enumerate every unreachable doc: {}",
            u[0].message
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backtick_edge_reaches_doc_from_nested_dir() {
        // B1 residual: the reachability BFS used parent.join for EVERY target,
        // so a `docs/`-prefixed backtick from a nested doc resolved against
        // the doc's dir (docs/plans/docs/prd/PRD.md), the edge was dropped,
        // and a doc referenced by a RESOLVING backtick was flagged
        // undiscoverable. The backtick edge must use the same base as
        // docs_findings: `docs/`-prefixed -> repo root.
        let dir = std::env::temp_dir().join(format!("docs_bt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs/plans")).unwrap();
        std::fs::create_dir_all(dir.join("docs/prd")).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "# agent\n- [guide](docs/guide.md)\n").unwrap();
        std::fs::write(dir.join("docs/guide.md"), "see `docs/prd/PRD.md`\n").unwrap();
        std::fs::write(dir.join("docs/prd/PRD.md"), "prd\n").unwrap();
        let f = docs_findings(&dir, &std::collections::HashSet::new());
        assert!(
            !f.iter().any(|x| x.kind == "docs-undiscoverable"),
            "backtick edge must reach PRD.md: {f:?}"
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
        let f = docs_findings(&dir, &std::collections::HashSet::new());
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
        let f = docs_findings(&dir, &std::collections::HashSet::new());
        assert!(!f.iter().any(|x| x.kind == "docs-undiscoverable"), "{f:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
