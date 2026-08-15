//! The repo-wide families computed from the code-review-graph contract
//! (a versioned, schema-neutral JSON emitted by code_health_graph_export.py
//! through the graph tool's own public API — the gate never touches the
//! SQLite schema or the DB location). Plus hotspot (git churn + max CC).
//!
//! Finding identity (kind/file/line/function) matches the Python reference
//! exactly; the concern-mix wording in messages is the honest core fact.

use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use crate::Finding;

#[derive(Deserialize)]
pub struct GNode {
    pub kind: String,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub line_start: Option<i64>,
    pub line_end: Option<i64>,
    pub community_id: Option<i64>,
}

#[derive(Deserialize)]
pub struct GEdge {
    pub kind: String,
    pub source: String,
    pub target: String,
    pub file_path: String,
}

#[derive(Deserialize)]
pub struct GraphContract {
    pub contract_version: i64,
    pub nodes: Vec<GNode>,
    pub edges: Vec<GEdge>,
    #[serde(default)]
    pub communities: HashMap<String, String>,
}

pub const BUILTIN_NAMES: &[&str] = &[
    "ArithmeticError", "AssertionError", "AttributeError", "BaseException", "BaseExceptionGroup",
    "BlockingIOError", "BrokenPipeError", "BufferError", "BytesWarning", "ChildProcessError",
    "ConnectionAbortedError", "ConnectionError", "ConnectionRefusedError", "ConnectionResetError",
    "DeprecationWarning", "EOFError", "Ellipsis", "EncodingWarning", "EnvironmentError", "Exception",
    "False", "FileExistsError", "FileNotFoundError", "FloatingPointError", "FutureWarning",
    "GeneratorExit", "IOError", "ImportError", "ImportWarning", "IndentationError", "IndexError",
    "InterruptedError", "IsADirectoryError", "KeyError", "KeyboardInterrupt", "LookupError",
    "MemoryError", "ModuleNotFoundError", "NameError", "None", "NotADirectoryError",
    "NotImplemented", "NotImplementedError", "OSError", "OverflowError", "PendingDeprecationWarning",
    "PermissionError", "ProcessLookupError", "RecursionError", "ReferenceError", "ResourceWarning",
    "RuntimeError", "RuntimeWarning", "StopAsyncIteration", "StopIteration", "SyntaxError",
    "SyntaxWarning", "SystemError", "SystemExit", "TabError", "TimeoutError", "True", "TypeError",
    "UnboundLocalError", "UnicodeDecodeError", "UnicodeEncodeError", "UnicodeError",
    "UnicodeTranslateError", "UnicodeWarning", "UserWarning", "ValueError", "Warning",
    "ZeroDivisionError", "__build_class__", "__debug__", "__doc__", "__import__", "__loader__",
    "__name__", "__package__", "__spec__", "abs", "aiter", "all", "anext", "any", "ascii", "bin",
    "bool", "breakpoint", "bytearray", "bytes", "callable", "chr", "classmethod", "compile",
    "complex", "copyright", "credits", "delattr", "dict", "dir", "divmod", "enumerate", "eval",
    "exec", "exit", "filter", "float", "format", "frozenset", "getattr", "globals", "hasattr",
    "hash", "help", "hex", "id", "input", "int", "isinstance", "issubclass", "iter", "len", "license",
    "list", "locals", "map", "max", "memoryview", "min", "next", "object", "oct", "open", "ord",
    "pow", "print", "property", "quit", "range", "repr", "reversed", "round", "set", "setattr",
    "slice", "sorted", "staticmethod", "str", "sum", "super", "tuple", "type", "vars", "zip",
];

/// First two directory segments inside the repo root — `_module_key`.
fn module_key(repo: &Path, file_path: &str) -> String {
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

fn base_name(qn: &str) -> &str {
    qn.rsplit("::").next().unwrap_or(qn).rsplit('.').next().unwrap_or(qn)
}

/// A dotted module name to a repo file rel — `_module_to_file`.
fn module_to_file(repo: &Path, dotted: &str) -> Option<String> {
    let base = dotted.replace('.', "/");
    for candidate in [format!("{base}.py"), format!("{base}/__init__.py")] {
        if repo.join(&candidate).exists() {
            return Some(candidate);
        }
    }
    None
}

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
        while let Some((w, mut i, edges)) = work.pop() {
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
                    loop {
                        let x = stack.pop().unwrap();
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
    let start = comp.iter().min().cloned().unwrap();
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
pub fn cycle_findings(repo: &Path, contract: &GraphContract) -> Vec<Finding> {
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut files: HashSet<String> = HashSet::new();
    for e in &contract.edges {
        if e.kind != "IMPORTS_FROM" {
            continue;
        }
        let src_rel = repo_rel(repo, &e.source);
        if !src_rel.ends_with(".py") {
            continue;
        }
        files.insert(src_rel.clone());
        if let Some(target_rel) = module_to_file(repo, &e.target) {
            if target_rel != src_rel {
                graph.entry(src_rel).or_default().push(target_rel);
            }
        }
    }
    let node_list: Vec<String> = files.iter().cloned().collect();
    let mut out = Vec::new();
    for comp in strongly_connected_components(&graph, &node_list) {
        if comp.len() < 2 {
            continue;
        }
        let chain = find_cycle(&graph, &comp);
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
        out.push(Finding {
            file: anchor,
            line: 0,
            function: String::new(),
            kind: "import-cycle".into(),
            severity: "fail".into(),
            message: format!(
                "import cycle: {cycle_text} — circular imports are fixed by restructuring modules, never bodged with lazy imports: hoist the shared interface into its own module"
            ),
        });
    }
    out
}

fn repo_rel(repo: &Path, path: &str) -> String {
    path.strip_prefix(&format!("{}/", repo.to_string_lossy()))
        .map(str::to_string)
        .unwrap_or_else(|| path.to_string())
}

/// Large functions: node span >= threshold — `_large_function_actions`.
pub fn large_function_findings(repo: &Path, contract: &GraphContract, max_lines: usize, include_tests: bool) -> Vec<Finding> {
    let mut out = Vec::new();
    for n in &contract.nodes {
        if !matches!(n.kind.as_str(), "Function" | "Method") {
            continue;
        }
        if !include_tests && n.kind == "Test" {
            continue;
        }
        if !n.file_path.ends_with(".py") {
            continue;
        }
        let (Some(ls), Some(le)) = (n.line_start, n.line_end) else { continue };
        let span = le - ls + 1;
        if le - ls < max_lines as i64 {
            continue;
        }
        let rel = repo_rel(repo, &n.file_path);
        if !include_tests && is_test_rel(&rel) {
            continue;
        }
        out.push(Finding {
            file: rel,
            line: ls as usize,
            function: n.name.clone(),
            kind: "large-function".into(),
            severity: "fail".into(),
            message: format!("function spans {span} lines (>= {max_lines})"),
        });
    }
    out
}

fn is_test_rel(rel: &str) -> bool {
    rel.contains("/test") || rel.starts_with("test")
}

/// Per-file coupling edge counts — `_hub_edge_counts` (builtin CALLS skipped).
fn hub_edge_counts(contract: &GraphContract) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for e in &contract.edges {
        if !matches!(e.kind.as_str(), "CALLS" | "IMPORTS_FROM" | "INHERITS" | "REFERENCES") {
            continue;
        }
        if !e.file_path.ends_with(".py") {
            continue;
        }
        if e.kind == "CALLS" && BUILTIN_NAMES.contains(&base_name(&e.target)) {
            continue;
        }
        *counts.entry(e.file_path.clone()).or_default() += 1;
    }
    counts
}

/// Hub files: heavy coupling — `_hub_file_actions`.
pub fn hub_file_findings(
    repo: &Path,
    contract: &GraphContract,
    max_edges: usize,
    include_tests: bool,
    max_cc_by_file: &HashMap<String, (usize, usize, String)>, // rel -> (line, cc, name) of the fattest
) -> Vec<Finding> {
    let counts = hub_edge_counts(contract);
    let mut out = Vec::new();
    let mut entries: Vec<(&String, &usize)> = counts.iter().collect();
    entries.sort_by_key(|(f, c)| (*c, f.clone()));
    entries.reverse();
    for (file_path, edge_count) in entries {
        if *edge_count < max_edges {
            continue;
        }
        let rel = repo_rel(repo, file_path);
        if !include_tests && is_test_rel(&rel) {
            continue;
        }
        let first = contract
            .nodes
            .iter()
            .filter(|n| n.file_path == *file_path && matches!(n.kind.as_str(), "Function" | "Method"))
            .filter_map(|n| n.line_start)
            .min()
            .unwrap_or(1);
        let (anchor, mut message) = match max_cc_by_file.get(&rel) {
            Some((line, cc, name)) => {
                (
                    *line,
                    format!(
                        "{edge_count} call/import edges (>= {max_edges}) fattest: {name}:{line} (CC {cc})"
                    ),
                )
            }
            None => (first as usize, format!("{edge_count} call/import edges (>= {max_edges})")),
        };
        if message.is_empty() {
            message = format!("{edge_count} call/import edges (>= {max_edges})");
        }
        out.push(Finding {
            file: rel,
            line: anchor,
            function: String::new(),
            kind: "hub-file".into(),
            severity: "fail".into(),
            message,
        });
    }
    out
}

/// The graph tool's own risk formula — recomputed from CALLS/TESTED_BY.
fn risk_for(
    contract: &GraphContract,
    node: &GNode,
    caller_counts: &HashMap<String, usize>,
    tested_counts: &HashMap<String, usize>,
) -> f64 {
    let caller_count = caller_counts.get(&node.qualified_name).copied().unwrap_or(0);
    let tested = tested_counts.get(&node.qualified_name).copied().unwrap_or(0) > 0;
    let mut risk: f64 = 0.0;
    if caller_count > 10 {
        risk += 0.3;
    } else if caller_count > 3 {
        risk += 0.15;
    }
    if !tested {
        risk += 0.3;
    }
    let name_lower = node.name.to_lowercase();
    let sec_kw = [
        "auth", "login", "password", "token", "session", "crypt", "secret", "credential",
        "permission", "sql", "execute",
    ];
    if sec_kw.iter().any(|kw| name_lower.contains(kw)) {
        risk += 0.4;
    }
    risk.min(1.0)
}

/// High-risk nodes — `_high_risk_actions` (risk recomputed, order/limit kept).
pub fn high_risk_findings(repo: &Path, contract: &GraphContract, max_risk: f64, include_tests: bool) -> Vec<Finding> {
    let mut caller_counts: HashMap<String, usize> = HashMap::new();
    let mut tested_counts: HashMap<String, usize> = HashMap::new();
    for e in &contract.edges {
        if e.kind == "CALLS" {
            *caller_counts.entry(e.target.clone()).or_default() += 1;
        } else if e.kind == "TESTED_BY" {
            *tested_counts.entry(e.source.clone()).or_default() += 1;
        }
    }
    let mut scored: Vec<(&GNode, f64, usize, bool)> = Vec::new();
    for n in &contract.nodes {
        if !matches!(n.kind.as_str(), "Function" | "Class" | "Test") {
            continue;
        }
        if !n.file_path.ends_with(".py") {
            continue;
        }
        let risk = risk_for(contract, n, &caller_counts, &tested_counts);
        if risk < max_risk {
            continue;
        }
        let rel = repo_rel(repo, &n.file_path);
        if !include_tests && is_test_rel(&rel) {
            continue;
        }
        let caller_count = caller_counts.get(&n.qualified_name).copied().unwrap_or(0);
        let tested = tested_counts.get(&n.qualified_name).copied().unwrap_or(0) > 0;
        scored.push((n, risk, caller_count, tested));
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    scored.truncate(50);
    let mut out = Vec::new();
    for (n, risk, caller_count, _tested) in scored {
        let line = n.line_start.unwrap_or(1) as usize;
        out.push(Finding {
            file: repo_rel(repo, &n.file_path),
            line,
            function: n.name.clone(),
            kind: "high-risk".into(),
            severity: "fail".into(),
            message: format!(
                "graph risk {risk:.2} (>= {max_risk}), {caller_count} call site(s)"
            ),
        });
    }
    out
}

/// The most-called external subsystem of a function — `_dominant_callee`.
fn dominant_callee(
    contract: &GraphContract,
    repo: &Path,
    qn: &str,
    own_rel: &str,
    node_by_qn: &HashMap<String, &GNode>,
) -> Option<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let own_mod = module_key(repo, own_rel);
    for e in &contract.edges {
        if e.kind != "CALLS" || e.source != qn {
            continue;
        }
        let callee_mod = resolve_callee_module(contract, repo, &e.target, node_by_qn);
        if let Some(m) = callee_mod {
            if m != own_mod {
                *counts.entry(m).or_default() += 1;
            }
        }
    }
    counts.into_iter().max_by_key(|(_, c)| *c).map(|(m, _)| m)
}

fn resolve_callee_module(
    contract: &GraphContract,
    repo: &Path,
    target: &str,
    node_by_qn: &HashMap<String, &GNode>,
) -> Option<String> {
    if let Some(n) = node_by_qn.get(target) {
        return Some(module_key(repo, &n.file_path));
    }
    if target.contains("::") {
        let name = base_name(target);
        if let Some(n) = contract.nodes.iter().find(|n| n.name == name) {
            return Some(module_key(repo, &n.file_path));
        }
    }
    None
}

/// File mixes layers — `_layer_mix_actions` / `_layer_mix_for_file`.
pub fn layer_mix_findings(repo: &Path, contract: &GraphContract, files: &[String]) -> Vec<Finding> {
    let node_by_qn: HashMap<String, &GNode> = contract
        .nodes
        .iter()
        .map(|n| (n.qualified_name.clone(), n))
        .collect();
    let mut out = Vec::new();
    for rel in files {
        if is_test_rel(rel) {
            continue;
        }
        let abs = format!("{}/{}", repo.to_string_lossy(), rel);
        let fns: Vec<&GNode> = contract
            .nodes
            .iter()
            .filter(|n| n.file_path == abs && matches!(n.kind.as_str(), "Function" | "Method"))
            .collect();
        if fns.len() < 6 {
            continue;
        }
        let mut layers: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for n in fns {
            if let Some(layer) = dominant_callee(contract, repo, &n.qualified_name, rel, &node_by_qn) {
                layers.entry(layer).or_default().push(base_name(&n.qualified_name).to_string());
            }
        }
        let big: Vec<(String, Vec<String>)> = layers
            .into_iter()
            .filter(|(_, names)| names.len() >= 2)
            .collect();
        if big.len() < 2 {
            continue;
        }
        let groups = big
            .iter()
            .take(3)
            .map(|(m, names)| format!("{m} ({})", names.iter().take(4).cloned().collect::<Vec<_>>().join(", ")))
            .collect::<Vec<_>>()
            .join(", ");
        let metric: usize = big.iter().map(|(_, n)| n.len()).sum();
        let _ = metric;
        out.push(Finding {
            file: rel.clone(),
            line: 0,
            function: String::new(),
            kind: "layer-mix".into(),
            severity: "fail".into(),
            message: format!("file '{rel}' mixes layers: {groups}"),
        });
    }
    out
}

/// Folder grab bags — `_folder_mix_actions` / `_folder_mix_for_dir`.
pub fn folder_mix_findings(repo: &Path, contract: &GraphContract) -> Vec<Finding> {
    // best community per file (max count)
    let mut best: HashMap<String, i64> = HashMap::new();
    let mut best_count: HashMap<String, usize> = HashMap::new();
    for n in &contract.nodes {
        let Some(cid) = n.community_id else { continue };
        if !n.file_path.ends_with(".py") {
            continue;
        }
        let file = repo_rel(repo, &n.file_path);
        let c = best_count.entry(file.clone()).or_insert(0);
        *c += 1;
        if *c == 1 || *c > best_count[&file] {
            // keep the FIRST community seen when counts tie; Python keeps the
            // max count, first-wins on tie (dict iteration order)
            best.entry(file).or_insert(cid);
        }
    }
    let mut dirs: BTreeMap<String, Vec<(String, i64)>> = BTreeMap::new();
    for (fp, cid) in best {
        let parts: Vec<&str> = fp.split('/').collect();
        if parts.len() < 2 {
            continue;
        }
        let dir = parts[..parts.len() - 1].join("/");
        dirs.entry(dir).or_default().push((parts.last().unwrap().to_string(), cid));
    }
    let mut out = Vec::new();
    for (d, files) in dirs {
        let rel = repo_rel(repo, &d);
        if files.len() < 5 || rel.starts_with("tests") || rel.is_empty() || rel == "." {
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
            .take(3)
            .map(|(cid, fns)| {
                let name = contract.communities.get(&cid.to_string()).cloned().unwrap_or_else(|| cid.to_string());
                format!("{name} ({})", fns.iter().take(4).cloned().collect::<Vec<_>>().join(", "))
            })
            .collect::<Vec<_>>()
            .join(", ");
        out.push(Finding {
            file: rel.clone(),
            line: 0,
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

/// Hotspot: files that change often AND are complex (churn + max CC).
pub fn hotspot_findings(
    churn: &HashMap<String, usize>,
    cc_by_file: &HashMap<String, u32>,
    top_frac: f64,
    min_cc: u32,
    last_modified: &HashMap<String, String>,
) -> Vec<Finding> {
    let cutoff = ((churn.len() as f64) * top_frac).max(1.0) as usize;
    let mut entries: Vec<(&String, &usize)> = churn.iter().collect();
    entries.sort_by_key(|(f, c)| (*c, f.clone()));
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
            file: rel.clone(),
            line: 1,
            function: String::new(),
            kind: "hotspot".into(),
            severity: "fail".into(),
            message: format!(
                "changed {count}x (top {cutoff} by churn) — volatile part: max CC {max_cc} in {rel}"
            ),
        });
        let _ = last_modified;
    }
    out
}
