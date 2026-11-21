//! The graph contract's path vocabulary: a repo-relative name, a directory
//! key, a dotted module resolved to a file, and the test-file predicate.

use std::path::Path;

/// The path vocabulary every graph family reads the contract's paths through.
pub(crate) struct GraphPaths;

impl GraphPaths {
    /// First two directory segments inside the repo root — `_module_key`.
    pub(super) fn module_key(repo: &Path, file_path: &str) -> String {
        let rel = file_path
            .strip_prefix(&format!("{}/", repo.to_string_lossy()))
            .unwrap_or(file_path);
        let parts: Vec<&str> = rel.split('/').collect();
        if parts.len() <= 1 {
            rel.to_string()
        } else if parts.len() == 2 {
            parts[0].to_string()
        } else {
            format!("{}/{}", parts[0], parts[1])
        }
    }

    pub(super) fn base_name(qn: &str) -> &str {
        qn.rsplit("::").next().unwrap_or(qn).rsplit('.').next().unwrap_or(qn)
    }

    /// A dotted module name to a repo file rel — `_module_to_file`.
    pub(super) fn module_to_file(repo: &Path, dotted: &str) -> Option<String> {
        // The exporter emits IMPORTS_FROM targets as RESOLVED ABSOLUTE PATHS
        // for local modules — treat those directly as file paths.
        let path = Path::new(dotted);
        if path.is_absolute() {
            return Some(Self::repo_rel(repo, dotted));
        }
        // Falling back to dotted module name resolution for unresolved imports
        // (third-party / unresolvable by the exporter).
        let base = dotted.replace('.', "/");
        [format!("{base}.py"), format!("{base}/__init__.py")]
            .iter()
            .find(|c| repo.join(c).exists())
            .cloned()
    }

    pub(super) fn repo_rel(repo: &Path, path: &str) -> String {
        path.strip_prefix(&format!("{}/", repo.to_string_lossy()))
            .map(str::to_string)
            .unwrap_or_else(|| path.to_string())
    }

    pub(super) fn is_test_rel(rel: &str) -> bool {
        rel.contains("/test") || rel.starts_with("test")
    }
}
