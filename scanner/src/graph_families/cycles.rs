//! `import-cycle`: a cycle among local modules, named by one concrete chain.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use crate::finding::Finding;

use super::contract::GraphContract;
use super::graph_paths::GraphPaths;

pub(crate) struct Cycles;

impl Cycles {
    /// Iterative Tarjan SCC — `_strongly_connected_components`.
    fn strongly_connected_components(graph: &BTreeMap<String, Vec<String>>, nodes: &[String]) -> Vec<Vec<String>> {
        let mut index = 0usize;
        let mut indices: HashMap<String, usize> = HashMap::new();
        let mut low: HashMap<String, usize> = HashMap::new();
        let mut stack: Vec<String> = Vec::new();
        let mut on_stack: HashSet<String> = HashSet::new();
        let mut comps: Vec<Vec<String>> = Vec::new();
        let mut work: Vec<(String, usize, Vec<String>)> = Vec::new();
        for v in nodes {
            if indices.contains_key(v) {
                continue;
            }
            indices.insert(v.clone(), index);
            low.insert(v.clone(), index);
            index += 1;
            stack.push(v.clone());
            on_stack.insert(v.clone());
            work.push((v.clone(), 0, graph.get(v).cloned().unwrap_or_default()));
            while let Some((w, i, edges)) = work.pop() {
                if i < edges.len() {
                    work.push((w.clone(), i + 1, edges.clone()));
                    let next = &edges[i];
                    if !indices.contains_key(next) {
                        indices.insert(next.clone(), index);
                        low.insert(next.clone(), index);
                        index += 1;
                        stack.push(next.clone());
                        on_stack.insert(next.clone());
                        work.push((next.clone(), 0, graph.get(next).cloned().unwrap_or_default()));
                    } else if on_stack.contains(next) {
                        let low_w = low[&w].min(indices[next]);
                        low.insert(w.clone(), low_w);
                    }
                } else {
                    if low[&w] == indices[&w] {
                        let mut comp = Vec::new();
                        while let Some(x) = stack.pop() {
                            on_stack.remove(&x);
                            comp.push(x.clone());
                            if x == w {
                                break;
                            }
                        }
                        comps.push(comp);
                    }
                    if let Some(parent) = work.last() {
                        let low_parent = low[&parent.0].min(low[&w]);
                        low.insert(parent.0.clone(), low_parent);
                    }
                }
            }
        }
        comps
    }

    /// One concrete cycle in an SCC — `_find_cycle`.
    fn find_cycle(graph: &BTreeMap<String, Vec<String>>, comp: &[String]) -> Option<Vec<String>> {
        let members: HashSet<&String> = comp.iter().collect();
        let start = comp.iter().min().cloned()?;
        let mut stack = vec![(start.clone(), vec![start.clone()], HashSet::new())];
        while let Some((node, path, seen)) = stack.pop() {
            for w in graph.get(&node).cloned().unwrap_or_default() {
                if !members.contains(&w) {
                    continue;
                }
                if w == start {
                    let mut p = path.clone();
                    p.push(w);
                    return Some(p);
                }
                if !seen.contains(&w) {
                    let mut p = path.clone();
                    p.push(w.clone());
                    let mut s = seen.clone();
                    s.insert(w.clone());
                    stack.push((w, p, s));
                }
            }
        }
        None
    }

    /// Import cycles between local modules — `_cycle_actions`.
    pub fn findings(repo: &Path, contract: &GraphContract) -> Vec<Finding> {
        let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut files: HashSet<String> = HashSet::new();
        for e in &contract.edges {
            if e.kind != "IMPORTS_FROM" {
                continue;
            }
            let src_rel = GraphPaths::repo_rel(repo, &e.source);
            if !src_rel.ends_with(".py") {
                continue;
            }
            files.insert(src_rel.clone());
            if let Some(target_rel) = GraphPaths::module_to_file(repo, &e.target) {
                if target_rel != src_rel {
                    graph.entry(src_rel).or_default().push(target_rel);
                }
            }
        }
        Self::findings_for(&graph)
    }

    /// The shared cycle family over a resolved file->file adjacency — the Python
    /// graph contract builds it from IMPORTS_FROM edges, the Rust layer from its
    /// own mod/use tree. SCC + the finding shape are language-neutral.
    pub fn findings_for(graph: &BTreeMap<String, Vec<String>>) -> Vec<Finding> {
        let node_list: Vec<String> = graph.keys().cloned().collect();
        let mut out = Vec::new();
        for comp in Self::strongly_connected_components(graph, &node_list) {
            if comp.len() < 2 {
                continue;
            }
            let chain = Self::find_cycle(graph, &comp);
            let cycle_text = match &chain {
                Some(c) => format!("{} -> {}", c.join(" -> "), c[0]),
                None => {
                    let mut sorted = comp.clone();
                    sorted.sort();
                    sorted.join(", ")
                }
            };
            let anchor = chain.and_then(|c| c.first().cloned()).unwrap_or_else(|| {
                let mut sorted = comp.clone();
                sorted.sort();
                sorted[0].clone()
            });
            out.push(Finding { logical_start: None, seam_members: Vec::new(), file: anchor, line: 0, col: 0, function: String::new(), kind: "import-cycle".into(), severity: "fail".into(), message: format!(
            "import cycle: {cycle_text} — circular imports are fixed by restructuring modules, never hidden behind lazy imports: hoist the shared interface into its own module"
        ) });
        }
        out
    }
}
