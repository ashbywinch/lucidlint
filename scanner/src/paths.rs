//! Path vocabulary: the repo root the scan resolves against, repo-relative
//! names, and what counts as test or vendored (out of scan).

use std::path::Path;

/// The scan's path rules: the repo root, the scanned root, test and vendored paths, and repo-relative names.
pub struct Paths;

pub const VENDOR_DIRS: &[&str] = &["vendor", "third_party", "thirdparty", "node_modules", "site-packages"];

/// Directories a repo-wide scan never enters (venvs, caches, build output —
/// the orchestrator's git-ls-files equivalent without git).
/// The repo-wide merge for the LSP's save path: every Python file under
/// `root` scanned with the repo-wide flag, then the families that need the
/// whole repo (duplicate, unused — reconciled through each file's
/// suppressions, review-log B3). Returns the ADDITIONS per repo-relative
/// file; the per-file findings travel via the buffer scan so nothing is
/// double-reported. Lives here (the composition root) so lsp stays a
/// standalone scan-core module (layers test).
pub(crate) const SCAN_SKIP_DIRS: &[&str] = &[
    ".git",
    ".venv",
    "venv",
    "node_modules",
    "__pycache__",
    ".lucidlint-cache",
    ".ruff_cache",
    ".pytest_cache",
    ".mypy_cache",
    ".pyrefly-cache",
    "htmlcov",
    "dist",
    "build",
    "target",
    ".code-review-graph",
    ".tox",
    ".eggs",
];

impl Paths {
    pub fn is_vendored(rel: &str) -> bool {
        rel.split('/').any(|part| VENDOR_DIRS.contains(&part))
    }

    /// The path is outside this scan: test code (unless tests are included) or
    /// vendored.
    pub(crate) fn is_out_of_scan(include_tests: bool, rel: &str) -> bool {
        (!include_tests && Self::is_test_path(rel)) || Self::is_vendored(rel)
    }

    /// Longest common prefix of the passed paths — the repo root for a
    /// full-repo run; the file itself in per-file mode.
    ///
    /// The char-level prefix can stop mid-component (tests/ vs tools/ share a
    /// trailing 't'), so it backs off to the last path separator — rels are
    /// always clean directory boundaries.
    pub(crate) fn repo_root(paths: &[String]) -> String {
        let cwd = std::env::current_dir().unwrap_or_default();
        Self::repo_root_impl(paths, &cwd)
    }

    /// The longest common prefix of the passed paths, backed off to the last
    /// path separator. Paths are resolved against `cwd` FIRST: the orchestrator
    /// passes REPO-RELATIVE paths (the binary runs with the repo as its cwd),
    /// and the raw common prefix of `tests/a.py` + `tools/b.py` is `"t"` — which
    /// backed off to `""` and silently broke every graph-family rel resolution
    /// (repo_rel on the graph's ABSOLUTE node paths stripped nothing), so
    /// large-function/high-risk/layer-mix/folder-mix never fired
    /// through the orchestrator. Joining the cwd makes the root the absolute
    /// repo path for relative inputs and leaves absolute inputs unchanged.
    pub(crate) fn repo_root_impl(paths: &[String], cwd: &Path) -> String {
        if paths.is_empty() {
            return String::new();
        }
        // A relative input is defined relative to the cwd, and the orchestrator
        // passes REPO-RELATIVE paths with its cwd AT the repo root — so the cwd IS
        // the scan root whenever ANY input is relative. The common prefix cannot
        // know that: it loses a package segment shared by every file (pkg/a.py +
        // pkg/b.py -> .../pkg), which silently breaks every repo-wide rel mapping
        // — duplicate, unused, duplicate-module, forwarding-chain and the graph
        // families all vanished for the common src/ or lib/ layout. Pure absolute
        // input (an external caller, the LSP) keeps the prefix logic; a mixed
        // caller is anchored at the cwd because that is what its relative half
        // means.
        if paths.iter().any(|p| !Path::new(p).is_absolute()) {
            return cwd.to_string_lossy().to_string();
        }
        let abs: Vec<String> = paths
            .iter()
            .map(|p| {
                let pb = Path::new(p);
                if pb.is_absolute() {
                    p.clone()
                } else {
                    cwd.join(pb).to_string_lossy().to_string()
                }
            })
            .collect();
        let mut prefix = abs[0].clone();
        for p in &abs[1..] {
            while !p.starts_with(&prefix) {
                prefix.truncate(prefix.len().saturating_sub(1));
            }
        }
        match prefix.rfind('/') {
            Some(idx) => prefix.truncate(idx),
            None => prefix.clear(),
        }
        prefix
    }

    pub(crate) fn rel_of(path: &str, root: &str) -> String {
        // a relative path is resolved against the cwd (the binary runs with the
        // repo as its cwd) — stripping the root then yields the true rel instead
        // of falling back to the bare file name (which left per-file rels to be
        // re-resolved by the orchestrator's cwd-dependent guess)
        let abs = if Path::new(path).is_absolute() {
            path.to_string()
        } else {
            std::env::current_dir()
                .unwrap_or_default()
                .join(path)
                .to_string_lossy()
                .to_string()
        };
        if let Some(rel) = abs.strip_prefix(root).and_then(|r| r.strip_prefix('/')) {
            return rel.to_string();
        }
        std::path::Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string())
    }

    /// The scan root: the explicit `--root` the orchestrator passes, else the
    /// inferred one. Explicit wins so the caller's contract does not depend on
    /// the file layout it happens to pass.
    pub(crate) fn scan_root(paths: &[String], root_override: Option<&str>) -> String {
        match root_override {
            Some(r) => r.to_string(),
            None => Self::repo_root(paths),
        }
    }

    pub(crate) fn is_test_path(name: &str) -> bool {
        name.contains("/test") || name.starts_with("test")
    }
}
