//! The latent-class state families: a shared parameter pair, a method that
//! reads another object's fields, undeclared members, and duplicated fields.
// lucidlint: ignore-file boolean-arg the walker's flag is its API contract (descend),
// not an unnamed mode

use std::collections::HashSet;

use ruff_python_ast::{Expr, Stmt, StmtClassDef, StmtFunctionDef};
use ruff_text_size::Ranged;

use crate::finding::Finding;

use super::class_facts::class_field_names;
use super::expr_walk::{push_stmt_children, stmt_exprs, walk_expr_deep};
use super::function_facts::FunctionFacts;
use super::record_shape::{class_key, module_type_bindings, resolve_alias, TypeBindings};
use crate::{line_of, ScanState};

/// One anchor's clump: the functions sharing it and the pairs they share.
#[derive(Default)]
struct AnchorClump {
    functions: Vec<String>,
    /// Each pair's NAME plus the annotation key attached to each name, so
    /// two clumps that share names but not types stay separate.
    pairs: Vec<PairKey>,
    /// Each pair's OWN function set — the union is not shared by every pair
    /// (review-bot, PR #15).
    pair_fns: Vec<Vec<String>>,
}

/// One parameter's grouping identity: its name and its annotation's
/// whitespace-normalized, alias-resolved source text. `""` is the
/// unannotated key and matches only other unannotated parameters — an
/// absent annotation is not evidence that two parameters share a type.
fn param_annotation_key(p: &ruff_python_ast::ParameterWithDefault, bindings: &TypeBindings, source: &str) -> ParamKey {
    let name = p.parameter.name.to_string();
    let annotation = p
        .parameter
        .annotation
        .as_deref()
        .map(|a| {
            let resolved = resolve_alias(a, bindings);
            source[resolved.range()]
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect()
        })
        .unwrap_or_default();
    (name, annotation)
}

/// Three or more module functions sharing the same unordered parameter
/// pair — the pair travels together, so it is a data clump; introduce a
/// parameter object.
pub fn data_clump_findings(state: &mut ScanState, body: &[Stmt]) {
    let bindings = module_type_bindings(body);
    let mut funcs: Vec<(&StmtFunctionDef, Vec<ParamKey>)> = Vec::new();
    for s in body {
        let Stmt::FunctionDef(f) = s else { continue };
        let mut params = Vec::new();
        for a in f.parameters.posonlyargs.iter().chain(&f.parameters.args) {
            params.push(param_annotation_key(a, &bindings, state.source));
        }
        funcs.push((f, params));
    }
    let mut pairs: std::collections::HashMap<PairKey, Vec<&StmtFunctionDef>> = std::collections::HashMap::new();
    for (f, params) in &funcs {
        for i in 0..params.len() {
            for j in (i + 1)..params.len() {
                let (a, b) = if params[i].0 < params[j].0 {
                    (params[i].clone(), params[j].clone())
                } else {
                    (params[j].clone(), params[i].clone())
                };
                pairs.entry((a, b)).or_default().push(f);
            }
        }
    }
    // ONE finding per anchor naming every pair that shares it: per-pair
    // findings stack at the same def line and one marker consumes one
    // finding, so an anchor with >3 pairs could never be per-site
    // suppressed — the whack-a-mole the houses sweep hit (suppress the
    // visible one, the next pair surfaces).
    let mut by_anchor: std::collections::HashMap<(usize, String), AnchorClump> = std::collections::HashMap::new();
    let mut reported: HashSet<PairKey> = HashSet::new();
    for ((a, b), fs) in &pairs {
        if fs.len() < 3 || !reported.insert((a.clone(), b.clone())) {
            continue;
        }
        let line = line_of(state.source, fs[0].name.range().start());
        let entry = by_anchor.entry((line, fs[0].name.to_string())).or_default();
        for f in fs {
            let name = f.name.to_string();
            if !entry.functions.contains(&name) {
                entry.functions.push(name);
            }
        }
        entry.pairs.push((a.clone(), b.clone()));
        // each pair's OWN >= 3 functions — the message names these, never a
        // union some pair does not share (review-bot, PR #15)
        let mut fns: Vec<String> = fs.iter().map(|f| f.name.to_string()).collect();
        fns.sort();
        entry.pair_fns.push(fns);
    }
    let mut anchors: Vec<_> = by_anchor.into_iter().collect();
    anchors.sort_by_key(|((line, _), _)| *line);
    for ((line, anchor), clump) in anchors {
        // the pairs travel with their own function sets through the sort
        let mut paired: Vec<(PairKey, Vec<String>)> = clump.pairs.into_iter().zip(clump.pair_fns).collect();
        paired.sort();
        let Some((((pa, _), (pb, _)), pair_names)) = paired.first().cloned() else {
            continue;
        };
        let pair_text = format!("({pa}, {pb})");
        // seam grouping keeps every clump parameter (schema-3 disambiguation)
        let mut params: Vec<String> = Vec::new();
        for (((a, _), (b, _)), _) in &paired {
            params.push(a.clone());
            params.push(b.clone());
        }
        params.sort();
        params.dedup();
        state.findings.push(Finding { logical_start: None, seam_members: params.clone(),
        col: 0,
        file: state.file.to_string(),
        line,
        function: anchor,
        kind: "data-clump".into(),
        severity: "fail".into(),
        // A3: the message names the FIRST qualifying pair and only ITS
        // functions — each pair has its OWN >= 3 functions (PR #15)
        message: format!(
            "{names_count} functions near here ({names}) pass the same parameter pair {pair_text} together. The pair is one thing in the domain, and passing it as two parameters never says what that thing is. Make a class whose name states that thing, and pass one instance. — fix: extract-class",
            names_count = pair_names.len(),
            names = pair_names.join(", "),
        ), });
    }
}

/// The root name of an attribute chain (self.graph.em -> self; graph.em ->
/// graph); None for other shapes.
fn chain_root_name(e: &Expr) -> Option<&str> {
    let mut v = e;
    loop {
        match v {
            Expr::Attribute(a) => v = a.value.as_ref(),
            Expr::Name(n) => return Some(n.id.as_str()),
            _ => return None,
        }
    }
}

/// Attribute READ counts by receiver root name — method calls (Call funcs)
/// are not field reads and do not count.
fn count_receiver_reads(body: &[Stmt], counts: &mut std::collections::HashMap<String, usize>) {
    fn visit(e: &Expr, counts: &mut std::collections::HashMap<String, usize>) {
        walk_expr_deep(e, false, &mut |x, in_call_func| {
            if let Expr::Attribute(a) = x {
                if in_call_func {
                    return; // method calls are not field reads
                }
                if let Some(base) = chain_root_name(&a.value) {
                    *counts.entry(base.to_string()).or_insert(0) += 1;
                }
            }
        });
    }

    let mut stack: Vec<&Stmt> = Vec::new();
    for s in body {
        stack.push(s);
    }
    while let Some(s) = stack.pop() {
        if matches!(s, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
            continue;
        }
        for e in stmt_exprs(s) {
            visit(e, counts);
        }
        push_stmt_children(s, &mut stack);
    }
}

/// The locals aliased from the class's own state: `graph = self.graph`.
/// Feature envy reads THESE — the collaborator the method reaches into.
fn collaborator_aliases(body: &[Stmt], out: &mut HashSet<String>) {
    let mut stack: Vec<&Stmt> = Vec::new();
    for s in body {
        stack.push(s);
    }
    while let Some(s) = stack.pop() {
        if matches!(s, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
            continue;
        }
        if let Stmt::Assign(a) = s {
            if a.targets.len() == 1 {
                if let (Expr::Name(n), Expr::Attribute(at)) = (&a.targets[0], a.value.as_ref()) {
                    if matches!(at.value.as_ref(), Expr::Name(inner) if inner.id.as_str() == "self") {
                        out.insert(n.id.to_string());
                    }
                }
            }
        }
        push_stmt_children(s, &mut stack);
    }
}

/// The names bound as loop targets anywhere in a function's body (for x in
/// ...) — elements the function iterates are its own inputs.
fn loop_target_names(body: &[Stmt], out: &mut HashSet<String>) {
    let mut stack: Vec<&Stmt> = Vec::new();
    for s in body {
        stack.push(s);
    }
    while let Some(s) = stack.pop() {
        if matches!(s, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
            continue;
        }
        if let Stmt::For(f) = s {
            let mut tstack: Vec<&Expr> = Vec::new();
            tstack.push(&f.target);
            while let Some(t) = tstack.pop() {
                match t {
                    Expr::Name(n) => {
                        out.insert(n.id.to_string());
                    }
                    Expr::Tuple(tp) => {
                        for el in &tp.elts {
                            tstack.push(el);
                        }
                    }
                    Expr::List(l) => {
                        for el in &l.elts {
                            tstack.push(el);
                        }
                    }
                    Expr::Starred(st) => tstack.push(&st.value),
                    _ => {}
                }
            }
        }
        push_stmt_children(s, &mut stack);
    }
}

/// A method that reads another object's fields more than its own state —
/// feature envy: the computation belongs on the envied object.
pub fn feature_envy_findings(state: &mut ScanState, body: &[Stmt]) {
    for s in body {
        let Stmt::ClassDef(c) = s else { continue };
        for m in &c.body {
            let Stmt::FunctionDef(f) = m else { continue };
            let mut params = HashSet::new();
            FunctionFacts::function_param_names(&f.parameters, &mut params);
            let mut loop_targets = HashSet::new();
            loop_target_names(&f.body, &mut loop_targets);
            let mut collaborators = HashSet::new();
            collaborator_aliases(&f.body, &mut collaborators);
            let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
            count_receiver_reads(&f.body, &mut counts);
            let self_reads = counts.get("self").copied().unwrap_or(0);
            if self_reads < 1 {
                continue;
            }
            for (receiver, n) in &counts {
                // envy is reaching into a COLLABORATOR (a local aliased from
                // self.<attr>) — not the method's inputs (params, loop
                // targets) and not computed values (pos = self.get_metadata())
                let reads = ReceiverReads {
                    receiver,
                    reads: *n,
                    self_reads,
                    params: &params,
                    loop_targets: &loop_targets,
                    collaborators: &collaborators,
                };
                if !reads.is_envy() {
                    continue;
                }
                let line = line_of(state.source, f.name.range().start());
                state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                                    file: state.file.to_string(),
                                    line,
                                    function: f.name.to_string(),
                                    kind: "feature-envy".into(),
                                    severity: "fail".into(),
                                    message: format!(
                                        "'{}' reads '{}' {} times vs its own state {} — feature envy: the logic belongs on the envied object; move the computation onto '{}' as a method named with a domain verb (what it does) — fix: feature-envy",
                                        f.name.as_str(),
                                        receiver,
                                        n,
                                        self_reads,
                                        receiver
                                    ), });
            }
        }
    }
}

/// Collect every `self.<attr> = ...` site in a top-level class — the
/// undeclared-attribute family's raw material. Whether a site is a QUIET
/// assignment is decided at the repo-wide merge (`undeclared_findings`):
/// a base class in another file may declare the member, and a per-file
/// scan cannot see it.
pub fn collect_self_assigns(state: &mut ScanState, body: &[Stmt]) {
    for s in body {
        let Stmt::ClassDef(c) = s else { continue };
        for m in &c.body {
            let Stmt::FunctionDef(f) = m else { continue };
            let is_init = f.name.as_str() == "__init__";
            let mut stack: Vec<&Stmt> = Vec::new();
            for b in &f.body {
                stack.push(b);
            }
            while let Some(st) = stack.pop() {
                if matches!(st, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
                    continue;
                }
                let mut targets: Vec<&Expr> = Vec::new();
                match st {
                    Stmt::Assign(a) => {
                        for t in &a.targets {
                            targets.push(t);
                        }
                    }
                    Stmt::AnnAssign(a) => targets.push(a.target.as_ref()),
                    Stmt::AugAssign(a) => targets.push(&a.target),
                    _ => {}
                }
                for t in targets {
                    let Expr::Attribute(at) = t else { continue };
                    if !matches!(at.value.as_ref(), Expr::Name(n) if n.id.as_str() == "self") {
                        continue;
                    }
                    state.self_assigns.push(crate::SelfAssign {
                        class: c.name.to_string(),
                        attr: at.attr.to_string(),
                        line: line_of(state.source, at.range().start()),
                        function: f.name.to_string(),
                        is_init,
                    });
                }
                push_stmt_children(st, &mut stack);
            }
        }
    }
}

/// The repo-wide undeclared-attribute pass: a `self.<attr> = v` site is a
/// finding only when neither the class NOR ANY ANCESTOR declares the member
/// (annotated in the class body, in __slots__, or in __init__) — the houses
/// case was `Node.display_name` declared in dag/node.py, flagged on every
/// subclass until they re-declared it. Base resolution follows
/// `abstraction_findings`: a `Name:` base resolves same-file first, then
/// through the file's imports; an `Attr:` base through the alias's module.
pub fn undeclared_findings(scans: &[crate::UndeclScan]) -> Vec<Finding> {
    use std::collections::HashMap;
    let mut classes: HashMap<(String, String), &crate::ClassInfo> = HashMap::new();
    for s in scans {
        for c in &s.classes {
            classes.insert((s.rel.clone(), c.name.clone()), c);
        }
    }
    // each class's bases resolve against ITS OWN file's imports
    let import_maps: HashMap<&String, HashMap<&str, (&str, &str)>> = scans
        .iter()
        .map(|s| {
            (
                &s.rel,
                s.imports
                    .iter()
                    .map(|i| (i.alias.as_str(), (i.module.as_str(), i.imported.as_str())))
                    .collect(),
            )
        })
        .collect();
    /// True when `key`'s class or any ANCESTOR declares `attr`; visited
    /// guards against base-class cycles (Python tolerates them at definition
    /// time). A `Name:` base resolves same-file first, then through the
    /// class's own file's imports; an `Attr:` base through the alias's
    /// module. Each hop uses the ancestor's file imports, so multi-level
    /// chains across files resolve correctly.
    fn declares(
        classes: &HashMap<(String, String), &crate::ClassInfo>,
        import_maps: &HashMap<&String, HashMap<&str, (&str, &str)>>,
        key: &(String, String),
        attr: &str,
        visited: &mut Vec<(String, String)>,
    ) -> bool {
        if visited.contains(key) {
            return false;
        }
        visited.push(key.clone());
        let Some(c) = classes.get(key) else {
            return false;
        };
        if c.declared.iter().any(|d| d == attr) {
            return true;
        }
        let empty = HashMap::new();
        let imports_of = import_maps.get(&key.0).unwrap_or(&empty);
        for base in &c.bases {
            let candidates: Vec<(String, String)> = if let Some(rest) = base.strip_prefix("Name:") {
                let mut cands = vec![(key.0.clone(), rest.to_string())];
                if let Some((module, imported)) = imports_of.get(rest) {
                    // `from base import Node as N`: the candidate class is the
                    // IMPORTED name, not the local alias (review-bot, PR #15)
                    cands.push(((*module).to_string(), (*imported).to_string()));
                }
                cands
            } else if let Some(rest) = base.strip_prefix("Attr:") {
                let mut parts = rest.splitn(2, ':');
                let alias = parts.next().unwrap_or("");
                let attr_name = parts.next().unwrap_or("");
                let mut cands = Vec::new();
                if let Some((module, _name)) = imports_of.get(alias) {
                    cands.push(((*module).to_string(), attr_name.to_string()));
                }
                cands
            } else {
                Vec::new()
            };
            for anc in candidates {
                let Some(anc_key) = class_key(classes, &anc.0, &anc.1) else {
                    continue;
                };
                if declares(classes, import_maps, &anc_key, attr, visited) {
                    return true;
                }
            }
        }
        false
    }
    let mut out = Vec::new();
    for s in scans {
        for a in &s.self_assigns {
            let mut visited: Vec<(String, String)> = Vec::new();
            let key = (s.rel.clone(), a.class.clone());
            if declares(&classes, &import_maps, &key, &a.attr, &mut visited) {
                continue;
            }
            let msg = if a.is_init {
                format!(
                    "'{}' assigns member '{}' in __init__ without a declaration — declare it: self.{}: <type> = ... — fix: undeclared-attribute",
                    a.class, a.attr, a.attr
                )
            } else {
                format!(
                    "'{}' assigns member '{}' in '{}' without a declaration — declare it in __init__ (annotated) or the class body",
                    a.class, a.attr, a.function
                )
            };
            out.push(Finding {
                logical_start: None,
                seam_members: Vec::new(),
                col: 0,
                file: s.rel.clone(),
                line: a.line,
                function: a.function.clone(),
                kind: "undeclared-attribute".into(),
                severity: "fail".into(),
                message: msg,
            });
        }
    }
    out.sort_by(|x, y| (&x.file, x.line, &x.message).cmp(&(&y.file, y.line, &y.message)));
    out
}

/// The names in a __slots__ literal (tuple or list of string literals).
pub(super) fn slot_names(e: &Expr) -> Option<Vec<String>> {
    let elts: Vec<&Expr> = match e {
        Expr::Tuple(t) => t.elts.iter().collect(),
        Expr::List(l) => l.elts.iter().collect(),
        _ => return None,
    };
    let mut names = Vec::new();
    for el in elts {
        if let Expr::StringLiteral(s) = el {
            names.push(s.value.to_string());
        } else {
            return None;
        }
    }
    Some(names)
}

// ------------------------------------------------------------- latent-class
// the duplication counterweight: domain state duplicated across a containment
// edge.
//
// The size counterweight that used to live here (`god-class`: >=20 methods, or
// >=12 methods over >=250 lines) is GONE: its own message said "split it ONLY
// where the partition rule finds field-disjoint method groups", so at a
// cohesive class it ordered nothing, and the reader's only way to clear it was
// a suppression the bulk-suppression census then chased (issue #47). Size
// alone is not a split order; the seam case keeps its own finding, `partition`.

/// The same domain state on a class and on a class it CONTAINS — the
/// duplicate lives across a containment edge; one source of truth should
/// own it.
pub fn duplicate_field_findings(state: &mut ScanState, body: &[Stmt]) {
    let classes: Vec<&StmtClassDef> = body
        .iter()
        .filter_map(|s| match s {
            Stmt::ClassDef(c) => Some(c),
            _ => None,
        })
        .collect();
    for c in &classes {
        let fields = class_field_names(c);
        for ty in fields.iter().map(|(_, t)| t) {
            let Some(ty) = ty else { continue };
            let Some(contained) = classes.iter().find(|b| b.name.as_str() == ty.as_str()) else {
                continue;
            };
            if std::ptr::eq(*c, *contained) {
                continue;
            }
            // the shared field set across the containment edge — >=2 shared
            // names is duplicated domain state (one generic name like `name`
            // is coincidence)
            let contained_fields = class_field_names(contained);
            let shared: Vec<&str> = fields
                .iter()
                .filter(|(n, _)| contained_fields.iter().any(|(cn, _)| cn == n))
                .map(|(n, _)| n.as_str())
                .collect();
            if shared.len() < 2 {
                continue;
            }
            let line = line_of(state.source, c.name.range().start());
            state.findings.push(Finding { logical_start: None, seam_members: Vec::new(), col: 0,
                            file: state.file.to_string(),
                            line,
                            function: c.name.to_string(),
                            kind: "duplicate-field".into(),
                            severity: "fail".into(),
                            message: format!(
                                "'{}' and the '{}' it contains both hold {} — the same data lives in two places; one of them should own it",
                                c.name.as_str(),
                                contained.name.as_str(),
                                shared.join(", ")
                            ), });
        }
    }
}

// ------------------------------------------- sibling value classes (#61)

/// The read reaches into a collaborator: not the receiver, not a parameter or
/// loop target, and often enough to be a habit.
struct ReceiverReads<'a> {
    receiver: &'a str,
    reads: usize,
    self_reads: usize,
    params: &'a HashSet<String>,
    loop_targets: &'a HashSet<String>,
    collaborators: &'a HashSet<String>,
}

impl ReceiverReads<'_> {
    /// Envy is reaching into a COLLABORATOR (a local aliased from
    /// self.<attr>) — not the method's inputs (params, loop targets) and not
    /// computed values (pos = self.get_metadata()) — and out-reading the
    /// method's own state by three.
    fn is_envy(&self) -> bool {
        self.receiver != "self"
            && !self.params.contains(self.receiver)
            && !self.loop_targets.contains(self.receiver)
            && self.collaborators.contains(self.receiver)
            && self.reads >= 4
            && self.reads >= self.self_reads + 3
    }
}

/// One parameter's grouping identity: its name and annotation key.
pub(super) type ParamKey = (String, String);

/// The unordered parameter pair; each annotation travels with its name.
pub(super) type PairKey = (ParamKey, ParamKey);
