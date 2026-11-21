//! The module graph: `mod` declarations and `use` paths resolved to file edges,
//! so the import-cycle and layering families can read a crate's shape.

use std::collections::HashMap;

use syn::{ItemUse, UseTree};

/// The module graph: `mod` declarations and `use` paths resolved to file edges.
pub struct ModuleGraph;

impl ModuleGraph {
    /// The module->file map and the file->[module files] adjacency for import
    /// cycles. `files` is the repo-relative scan set; `mod_decls` maps each file
    /// to its top-level `mod` declarations. Crate roots are files named main.rs
    /// or lib.rs (a workspace's roots are all covered).
    pub fn module_graph(
        files: &[String],
        mod_decls: &HashMap<String, Vec<(String, bool)>>,
        uses: &HashMap<String, Vec<String>>,
    ) -> std::collections::BTreeMap<String, Vec<String>> {
        let (module_map, top_level, module_files) = Self::module_index(files, mod_decls);
        let mut graph: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
        for file in module_files {
            graph.entry(file).or_default();
        }
        for file in files {
            let Some(paths) = uses.get(file) else { continue };
            let edges = Self::use_edges(paths, file, &module_map, &top_level);
            if !edges.is_empty() {
                graph.insert(file.clone(), edges);
            }
        }
        graph
    }

    /// Walk every crate root's `mod` declarations into the module-path -> file map,
    /// the set of top-level module names (for bare `use a::b`), and the files that
    /// are modules at all.
    ///
    /// Paths are scoped per root to avoid workspace collisions: two crates with a
    /// module named `a` would collide as unqualified paths, so keys carry the
    /// root's directory (e.g. `crate_a/src::a`).
    fn module_index(
        files: &[String],
        mod_decls: &HashMap<String, Vec<(String, bool)>>,
    ) -> (HashMap<String, String>, std::collections::HashSet<String>, Vec<String>) {
        let scan_set: std::collections::HashSet<String> = files.iter().cloned().collect();
        let mut module_map: HashMap<String, String> = HashMap::new();
        // Per-root top-level names for bare `use a::b` resolution
        let mut top_level_by_root: HashMap<String, Vec<String>> = HashMap::new();
        let mut module_files: Vec<String> = Vec::new();
        for root in files {
            if !(root.ends_with("main.rs") || root.ends_with("lib.rs")) {
                continue;
            }
            let dir = root
                .rsplit('/')
                .next()
                .map(|_| root[..root.rfind('/').map_or(0, |i| i)].to_string());
            let dir = dir.unwrap_or_default();
            let prefix = dir.clone(); // scope prefix for this root
            let root_decls = top_level_by_root.entry(root.clone()).or_default();
            let mut stack: Vec<(String, String)> = Vec::new(); // (module path, dir)
            if let Some(decls) = mod_decls.get(root) {
                for (name, _inline) in decls {
                    root_decls.push(name.clone());
                    let child = if dir.is_empty() {
                        name.clone()
                    } else {
                        format!("{dir}/{name}")
                    };
                    stack.push((child, name.clone()));
                }
            }
            while let Some((child_dir, path)) = stack.pop() {
                let candidates = [format!("{child_dir}.rs"), format!("{child_dir}/mod.rs")];
                let found = candidates.into_iter().find(|c| scan_set.contains(c));
                if let Some(file) = found {
                    // Qualify the key with the root's directory prefix
                    let key = if prefix.is_empty() {
                        path.clone()
                    } else {
                        format!("{prefix}::{path}")
                    };
                    module_map.insert(key, file.clone());
                    module_files.push(file.clone());
                    if let Some(decls) = mod_decls.get(&file) {
                        for (name, _) in decls {
                            stack.push((format!("{child_dir}/{name}"), format!("{path}::{name}")));
                        }
                    }
                }
            }
        }
        // top-level module names for bare `use a::b` resolution (qualified by root)
        let top_level: std::collections::HashSet<String> = files
            .iter()
            .filter(|f| f.ends_with("main.rs") || f.ends_with("lib.rs"))
            .filter_map(|f| top_level_by_root.get(f))
            .flatten()
            .cloned()
            .collect();
        (module_map, top_level, module_files)
    }

    /// The scanned files this file's `use` paths point at, sorted and deduped.
    fn use_edges(
        paths: &[String],
        file: &str,
        module_map: &HashMap<String, String>,
        top_level: &std::collections::HashSet<String>,
    ) -> Vec<String> {
        let mut out_edges: Vec<String> = Vec::new();
        for path in paths {
            if let Some(t) = Self::resolve_use(path, file, module_map, top_level) {
                if t != *file {
                    out_edges.push(t);
                }
            }
        }
        out_edges.sort();
        out_edges.dedup();
        out_edges
    }

    /// Resolve one raw use path to a scanned file rel (the module that owns the
    /// imported name), or None when it is external or unresolvable.
    fn resolve_use(
        path: &str,
        file: &str,
        module_map: &HashMap<String, String>,
        top_level: &std::collections::HashSet<String>,
    ) -> Option<String> {
        let (rest, is_crate) = if let Some(r) = path.strip_prefix("crate::") {
            (r.to_string(), true)
        } else if path.starts_with("::") {
            (path.trim_start_matches(':').to_string(), true)
        } else {
            (path.to_string(), false)
        };
        if !is_crate {
            if let Some(r) = rest.strip_prefix("super::") {
                // the file's own module path, up one level, plus the rest
                let own = Self::file_module_path(file, module_map)?;
                let parent = match own.rfind("::") {
                    Some(i) => &own[..i],
                    None => "",
                };
                let joined = if parent.is_empty() {
                    r.to_string()
                } else {
                    format!("{parent}::{r}")
                };
                return Self::resolve_dotted(&joined, module_map);
            }
            if let Some(r) = rest.strip_prefix("self::") {
                let own = Self::file_module_path(file, module_map)?;
                let joined = if own.is_empty() {
                    r.to_string()
                } else {
                    format!("{own}::{r}")
                };
                return Self::resolve_dotted(&joined, module_map);
            }
            // bare path: edition-2018 absolute — a top-level local module, else external
            let first = rest.split("::").next().unwrap_or("");
            if first.is_empty() || !top_level.contains(first) {
                return None;
            }
        }
        Self::resolve_dotted(&rest, module_map)
    }

    fn resolve_dotted(path: &str, module_map: &HashMap<String, String>) -> Option<String> {
        let parts: Vec<&str> = path.split("::").collect();
        for cut in (1..parts.len()).rev() {
            let candidate = parts[..cut].join("::");
            // Try the exact key (which may include a root-scope prefix)
            if let Some(f) = module_map.get(&candidate) {
                return Some(f.clone());
            }
        }
        None
    }

    fn file_module_path(file: &str, module_map: &HashMap<String, String>) -> Option<String> {
        let _ = file;
        module_map.iter().find(|(_, f)| *f == file).map(|(p, _)| p.clone())
    }

    /// `use` paths referenced by one item, raw ("crate::a::b", "super::x", ...).
    /// The last segment of each path is the imported NAME — the module edge is
    /// the path minus that segment, resolved cross-file.
    pub(super) fn use_paths(u: &ItemUse) -> Vec<String> {
        fn walk(tree: &UseTree, prefix: &mut Vec<String>, out: &mut Vec<String>) {
            match tree {
                UseTree::Path(p) => {
                    prefix.push(p.ident.to_string());
                    walk(&p.tree, prefix, out);
                    prefix.pop();
                }
                UseTree::Name(n) => {
                    prefix.push(n.ident.to_string());
                    out.push(prefix.join("::"));
                    prefix.pop();
                }
                UseTree::Rename(r) => {
                    prefix.push(r.ident.to_string());
                    out.push(prefix.join("::"));
                    prefix.pop();
                }
                UseTree::Glob(_) => {
                    out.push(prefix.join("::"));
                }
                UseTree::Group(g) => {
                    for item in &g.items {
                        walk(item, prefix, out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        let mut prefix: Vec<String> = Vec::new();
        if u.leading_colon.is_some() {
            prefix.push(String::new()); // ::a::b — absolute
        }
        walk(&u.tree, &mut prefix, &mut out);
        out
    }
}
