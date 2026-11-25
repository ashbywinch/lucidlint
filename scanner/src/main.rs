// lucidlint: ignore-file complexity the ruff visitor walkers are parity-locked dispatch tables —
// match-arm count, not branching; keep NEW functions under cc 15

//! lucidlint — the Rust scan core for the deterministic lucidlint gate.
//!
//! Phase 1 of the port: parsing + the pure-walk checks. The finding schema is
//! deliberately language-neutral (file/line/function/kind/severity/message) —
//! JS and other languages plug in later with their own parsers behind the
//! same schema.
//!
//! Architecture: EVERY function (methods and nested included) becomes a scan
//! scope for the walk-based checks (unreachable, imports, magic numbers,
//! no-op statements) with innermost-function attribution; only module-level
//! functions report cyclomatic complexity, exactly as radon does (nested
//! functions and class bodies contribute no decisions to their parent).

// lucidlint: ignore-file large-function scan_source_impl is the CLI's one-pass pipeline — extracting
// helpers would thread six collections through a parameter object for no reader gain
use rayon::prelude::*;
use ruff_python_ast::visitor::source_order::SourceOrderVisitor;
use ruff_python_ast::{AnyNodeRef, ModModule};
use ruff_python_parser::{parse_module, Parsed};
use ruff_text_size::Ranged;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

mod checks;
mod code_text;
mod config;
mod docs;
mod file_scan;
mod finding;
mod fix;
mod fix_render;
mod forwarding;
mod graph_families;
mod lsp;
mod magic;
mod naming;
mod paths;
mod python_shape;
mod repo_scan;
mod report;
mod rules_gen;
mod rustloops;
mod rustscan;
mod skeleton;
mod source;
mod state;
mod suppressions;
use checks::Q;
use checks::*;
use file_scan::{ClassInfo, Definition, FileScan, ImportInfo, SelfAssign, Setter, UndeclScan};
use finding::{Finding, FnCc, FnScope};
use paths::{Paths, SCAN_SKIP_DIRS};
use repo_scan::RepoScan;
use source::Source;

/// The gate's own bars: a large function's span, the bulk-suppression site
/// count, the graph families' cuts (high-risk floor, cohesion span, the top
/// fraction for hotspot and churn-untested), and the complexity gate.
const LARGE_FUNCTION_LINES: usize = 120;
const BULK_SUPPRESSION_SITES: usize = 10;
const HIGH_RISK_MIN: f64 = 0.8;
const COHESION_SPAN: usize = 150;
const TOP_FRACTION: f64 = 0.1;
const COMPLEXITY_GATE: u32 = 15;

impl Finding {
    /// The finding's logical line: its own line when no construct spans
    /// several source lines, else the construct's first source line.
    pub(crate) fn logical_line(&self) -> usize {
        self.logical_start.unwrap_or(self.line)
    }
}

#[derive(Default)]
struct ScanState<'a> {
    file: &'a str,
    source: &'a str,
    findings: Vec<Finding>,
    cc: Vec<FnCc>,
    /// (name, start line) of the innermost function — attribution.
    current_fn: Option<(String, usize)>,
    /// Open function scopes (innermost last): decisions + the enclosing
    /// function's returned names (for the swallow analysis).
    fn_stack: Vec<FnScope>,
    /// Class nesting: class bodies contribute no decisions (radon sub-visitor).
    in_class: u32,
    /// Chains the latent-visitor rule claimed — conditional-polymorphism
    /// must not double-flag them (one ruling per chain, or the agent thrashes
    /// between implementing the visitor and the polymorphic methods).
    claimed_dispatch: std::collections::HashSet<usize>,
    /// Parent chain for the magic-number position check — exprs plus the
    /// non-expr layers (stmt, keyword) that break the direct-parent link.
    parent_stack: Vec<state::ParentEntry<'a>>,
    /// Module-level function definitions (name, line) — non-test files.
    defs: Vec<(String, usize)>,
    /// Every function/method span (name, first line, last line) — the
    /// large-function rule's own evidence, computed from this file's AST so
    /// the fact never depends on the optional graph contract.
    fn_spans: Vec<(String, usize, usize)>,
    /// set_* methods and property setters (name, line) — repo-wide pass.
    setters: Vec<(String, usize)>,
    /// `self.<attr> = ...` sites in top-level classes — undeclared-
    /// attribute's repo-wide raw material.
    self_assigns: Vec<SelfAssign>,
    /// Module-level CLASS definitions (name, line) — the unused scan's
    /// dead-class arm (#28).
    class_defs: Vec<(String, usize)>,
    /// Module-level `NAME = SomeClass(...)` bindings — the scattered-instance
    /// pass's raw material, resolved against the repo's class names.
    scattered: Vec<checks::ScatteredInstance>,
    /// Module-level path expressions — the shared-path family's raw material.
    path_expressions: Vec<checks::PathExpression>,
    /// Substantive expressions — the duplicate-expression family's raw material.
    duplicate_expressions: Vec<checks::ExpressionOccurrence>,
    /// Small constructor calls — the shared-construction family's raw material.
    shared_constructions: Vec<checks::ConstructionOccurrence>,
    /// Duplicate-module identities (#26) — modules + top-level classes with
    /// their structural skeletons and constant tokens.
    skeleton_modules: Vec<checks::SkeletonModule>,
    /// Forwarding-chain raw material (#30) — see collect_forwarders.
    forwarder_fns: Vec<checks::ForwarderFn>,
    forwarder_methods: Vec<checks::ForwarderMethod>,
    repo_methods: Vec<checks::RepoMethod>,
    /// Middle-man raw material (#44): the file's classes (the repo-wide index
    /// is their union) and its single-statement delegations, which the
    /// repo-wide pass resolves — one class per file, so the target is usually
    /// in another file.
    middle_man_classes: Vec<checks::MiddleManClass>,
    middle_man_candidates: Vec<checks::MiddleManCandidate>,
    magic_table_exempts: HashSet<usize>,
    magic_const_rhs_exempts: HashSet<usize>,
    magic_len_guard_exempts: HashSet<usize>,
    /// (P2) Literals exempted by a unit-stating name (assignment target or
    /// enclosing parameter).
    magic_unit_named_exempts: HashSet<usize>,
    /// Every referenced name (Name nodes + import aliases) in this file.
    refs: HashSet<String>,
    /// Bound name -> the module it was imported from: the module alias for
    /// `import a.b [as c]`, the bound name for `from a import b [as c]`.
    /// `private-code-leakage` resolves a private attribute's receiver here, and
    /// a package `__init__.py` may reassemble its own package's names.
    import_sources: std::collections::HashMap<String, String>,
    /// Underscore-attribute reaches found during the walk, resolved after it:
    /// (receiver root, dotted target, line, enclosing function).
    private_reaches: Vec<(String, String, usize, String)>,
    /// String literal values (prod files only).
    strings: Vec<String>,
    /// Module-level function names with decorators (framework-registered).
    decorated: HashSet<String>,
    /// Duplicate candidates with their structural skeletons.
    skeletons: Vec<SkeletonFn>,
    /// Per-source-line logical-line start (1-based): the first source line of
    /// the construct a line belongs to (a multi-line `def`, a wrapped call, a
    /// split literal). Index = physical line; value = logical start.
    logical_starts: Vec<usize>,
    /// File is under a test path — reference-scan split + skeleton skip.
    is_test: bool,
    /// The module-level constants this file binds, in source order — the
    /// `global-state` constant message offers one class for the set when
    /// there are several.
    module_constants: Vec<String>,
}

// The last line start per `line_of`/`col_of` source — a thread-local cache
// keyed by (ptr, len): `line_of` used to scan from offset 0 for every call,
// so a scan with F findings and C comments cost O((F+C) * file_len) — a
impl FileScan {
    /// The repo-relative path — the scan's identity (the orchestrator's
    /// `rel`); the repo-wide merge keys everything by it.
    pub fn rel_of(&self, root_s: &str) -> String {
        Paths::rel_of(&self.file_name, root_s)
    }

    /// This file's function and class definitions, as the repo-wide `unused`
    /// rule reads them.
    pub fn definition_rows<'a>(&'a self, rel: &'a str) -> impl Iterator<Item = Definition> + 'a {
        let plain = self.defs.iter().map(move |(name, line)| Definition {
            rel: rel.to_string(),
            name: name.clone(),
            line: *line,
            is_class: false,
        });
        let classes = self.class_defs.iter().map(move |(name, line)| Definition {
            rel: rel.to_string(),
            name: name.clone(),
            line: *line,
            is_class: true,
        });
        plain.chain(classes)
    }

    /// This file's setter methods, as the repo-wide `unused` rule reads them.
    pub fn setter_rows<'a>(&'a self, rel: &'a str) -> impl Iterator<Item = Setter> + 'a {
        self.setters.iter().map(move |(name, line)| Setter {
            rel: rel.to_string(),
            name: name.clone(),
            line: *line,
        })
    }

    /// This file's function shapes, tagged with the file they came from — the
    /// `duplicate` rule's pool.
    pub fn skeleton_rows<'a>(&'a self, rel: &'a str) -> impl Iterator<Item = SkeletonFn> + 'a {
        self.skeletons.iter().map(move |shape| SkeletonFn {
            rel: rel.to_string(),
            name: shape.name.clone(),
            line: shape.line,
            end_line: shape.end_line,
            skeleton: shape.skeleton.clone(),
        })
    }
}

pub use rules_gen::{final_kind, FAMILY_KINDS, STANDARD_KINDS};

/// The scan pipeline — the composition root's one pass over a file or repo.
pub struct Pipeline;

impl Pipeline {
    /// The full scan (CLI): per-file families + repo-wide collections.
    fn scan_source(source: &str, name: &str) -> FileScan {
        let repo_wide = true;
        Self::scan(source, name, repo_wide)
    }

    /// The LSP scan: per-file families only — the duplicate-candidate BFS and
    /// its per-function skeleton walks are pure waste for a single buffer.
    /// Dispatches on the buffer's extension: .rs -> the Rust layer, else Python.
    fn scan_source_lsp(source: &str, name: &str) -> FileScan {
        if name.ends_with(".rs") {
            let repo_wide = false; // LSP per-buffer scans are single-file
            let rs = rustscan::RustScan::scan(source, name, repo_wide);
            return Self::rustscan_to_filescan_ref(&rs, name);
        }
        let repo_wide = false; // LSP per-buffer scans are single-file
        Self::scan(source, name, repo_wide)
    }

    /// Per-buffer scans never run the repo-wide families (unused, unused-setter,
    /// duplicate) — their findings are evaluated only in repo_wide_merge — so
    /// their markers cannot bind here and a stale finding for one is a false
    /// fail artifact. The gate path keeps stale reporting: its merge consumes
    /// those markers and drops exactly the stale findings that survive it.
    fn drop_repo_wide_stale_artifacts(findings: &mut Vec<Finding>) {
        findings.retain(|f| {
            if f.kind != "stale-suppression" {
                return true;
            }
            // every family the corpus answers on save: the per-buffer scan's
            // "nothing fires here" is a missing scan, not a missing finding
            let signal = crate::suppressions::Suppressions::signal_of_stale(&f.message);
            !crate::repo_scan::RepoScan::FAMILIES
                .iter()
                .any(|kind| crate::suppressions::Suppressions::signal_matches(&signal, kind))
        });
    }

    fn scan(source: &str, name: &str, repo_wide: bool) -> FileScan {
        Source::reset_line_starts();
        let parsed: Parsed<ModModule> = match parse_module(source) {
            Ok(p) => p,
            Err(_) => {
                return FileScan {
                    file_name: name.to_string(),
                    errors: 1,
                    ..Default::default()
                };
            }
        };
        let errors = parsed.errors().len();
        let body = parsed.syntax().body.clone(); // ThinVec<Stmt> — derefs to &[Stmt]
        let mut state = ScanState {
            file: name,
            source,
            logical_starts: checks::Suppressions::logical_line_starts(source, parsed.tokens()),
            magic_table_exempts: checks::MagicNumber::magic_table_exempt_offsets(&body),
            magic_const_rhs_exempts: checks::MagicNumber::magic_const_rhs_offsets(&body),
            magic_len_guard_exempts: checks::MagicNumber::magic_len_guard_offsets(&body),
            magic_unit_named_exempts: checks::NamingUnits::magic_unit_named_offsets(&body),
            is_test: Paths::is_test_path(name),
            module_constants: checks::GlobalState::module_constant_names(&body),
            ..Default::default()
        };
        for stmt in &body {
            state.visit_stmt(stmt);
        }
        state.module_post_passes(&body, name, source, repo_wide);
        code_in_literal_findings(&mut state, &body, source);
        if state.is_test {
            // the rules that live in tests — scanned for alone or with --include-tests
            Monkeypatch::findings(&mut state, &body, source);
            skipif_findings(&mut state, &body, source);
            fakefs_findings(&mut state, &body, source);
            no_assert_test_findings(&mut state, &body, source);
            test_asserts_source_findings(&mut state, &body, source);
        }
        // large-function: the span is a fact of THIS file's AST, computed here so
        // the LSP and a graph-less CI run see it too (the graph contract only
        // adds evidence about other files). Test files keep it out of the gate via
        // the action layer's test-only filter, unless --include-tests is passed.
        for (fn_name, start, end) in &state.fn_spans {
            let span = end.saturating_sub(*start) + 1;
            if span <= LARGE_FUNCTION_LINES {
                continue;
            }
            state.findings.push(Finding {
            logical_start: None,
            seam_members: Vec::new(),
            col: 0,
            file: name.to_string(),
            line: *start,
            function: fn_name.clone(),
            kind: "large-function".into(),
            severity: "fail".into(),
            message: format!(
                "'{fn_name}' spans {span} lines (>= 120) — the steps it performs are not named, so a reader cannot hold the whole job: extract each domain step into a named function"
            ),
        });
        }
        // duplicate candidates in ast.walk BFS order — module-level functions
        // (any depth) come before class methods regardless of source line;
        // skipped for a single buffer (repo-wide families need the whole repo)
        if repo_wide && !state.is_test {
            let mut queue: Vec<Q> = body.iter().map(|s| Q::N(AnyNodeRef::from(s))).collect();
            let mut qi = 0usize;
            while qi < queue.len() {
                if let Q::N(n) = queue[qi] {
                    if let AnyNodeRef::StmtFunctionDef(f) = n {
                        let skel = fn_skeleton(f);
                        if Similarity::is_duplicate_candidate(f, skel.len()) {
                            let def_line = Source::line_of(source, f.name.range().start());
                            state.skeletons.push(SkeletonFn {
                                rel: name.to_string(),
                                name: f.name.to_string(),
                                line: def_line,
                                end_line: Source::line_of(source, f.range().end()),
                                skeleton: skel,
                            });
                        }
                    }
                    skel_children(n, &mut queue);
                }
                qi += 1;
            }
        }
        // Phase 4a: label every emitted finding with its logical line — the first
        // source line of the construct it sits in (a multi-line `def`/`async def`,
        // a wrapped call, a split literal). The binder then admits a marker on the
        // logical line or the line before it in addition to the finding's own line
        // pair. Single-line constructs keep `None` (the finding's own line).
        {
            let starts = &state.logical_starts;
            for f in &mut state.findings {
                let line = f.line;
                if f.logical_start.is_none() {
                    f.logical_start = starts.get(line).copied().filter(|&start| start < line);
                }
            }
        }
        let tokens = parsed.tokens();
        let supps = checks::Suppressions::parse_suppressions(source, tokens);
        // complexity findings are generated from the cc array after this pass —
        // honor complexity suppressions here so both paths agree (radon parity:
        // a fn whose complexity is suppressed must not resurface via cc)
        let mut pre_used = crate::suppressions::PreUsedSuppressions::default();
        if !state.cc.is_empty() {
            state.cc.retain(|e| {
                // record what the retention's own decision honors — the SAME
                // widened window and family-aware matching that suppressed()
                // uses (a suppression used by the cc path must not be re-flagged
                // stale by apply_suppressions_impl)
                for ln in crate::suppressions::Suppressions::window_lines(e.line) {
                    if let Some(entries) = supps.line.get(&ln) {
                        for (sig, why) in entries {
                            if crate::suppressions::Suppressions::signal_matches(sig, "complexity") && !why.is_empty() {
                                pre_used.lines.insert((ln, sig.clone()));
                            }
                        }
                    }
                }
                let line_suppressed = crate::suppressions::Suppressions::suppressed("complexity", e.line, &supps);
                let file_suppressed = supps.file.get("complexity").is_some_and(|w| !w.is_empty());
                if file_suppressed {
                    pre_used.files.insert("complexity".into());
                }
                !line_suppressed && !file_suppressed
            });
        }
        let mut supps_spent = HashSet::new();
        // G4: the exemption-kind labels among the file's still-present
        // magic-number candidates — threaded into the stale message so it names
        // exactly the exemptions that apply (never unit/constant/table for
        // indices).
        let magic_exempt_labels = checks::MagicNumber::magic_exemption_labels(
            &body,
            source,
            &state.magic_table_exempts,
            &state.magic_const_rhs_exempts,
            &state.magic_len_guard_exempts,
            &state.magic_unit_named_exempts,
        );
        let mut books = crate::suppressions::SuppressionBooks {
            pre_used: &pre_used,
            spent: &mut supps_spent,
        };
        let findings =
            checks::Suppressions::apply_source(state.findings, source, name, tokens, &mut books, &magic_exempt_labels);
        let mut all = type_ignore_findings(source, name, tokens);
        all.extend(noqa_findings(source, name, tokens));
        all.extend(findings);
        if !repo_wide {
            Self::drop_repo_wide_stale_artifacts(&mut all);
        }
        let classes = OverAbstraction::collect_classes(&body, source);
        let imports = OverAbstraction::collect_imports(&body, source);
        let sibling_classes = checks::SiblingValueClass::collect_sibling_classes(&body, source);
        FileScan {
            file_name: name.to_string(),
            findings: all,
            cc: state.cc,
            errors,
            defs: state.defs,
            class_defs: state.class_defs,
            scattered: state.scattered,
            duplicate_expressions: state.duplicate_expressions,
            shared_constructions: state.shared_constructions,
            path_expressions: state.path_expressions,
            module_shape: checks::ModuleShape::python(source, &body),
            setters: state.setters,
            refs: state.refs,
            strings: state.strings,
            decorated: state.decorated,
            skeletons: state.skeletons,
            classes,
            imports,
            self_assigns: state.self_assigns,
            supps,
            supps_spent,
            skeleton_modules: state.skeleton_modules,
            forwarder_fns: state.forwarder_fns,
            forwarder_methods: state.forwarder_methods,
            repo_methods: state.repo_methods,
            middle_man_classes: state.middle_man_classes,
            middle_man_candidates: state.middle_man_candidates,
            sibling_classes,
        }
    }

    fn scan_file(path: &Path) -> FileScan {
        let source = std::fs::read_to_string(path).unwrap_or_default();
        let name = path.to_str().unwrap_or("<file>").to_string();
        if name.ends_with(".rs") {
            let repo_wide = true;
            let rs = rustscan::RustScan::scan(&source, &name, repo_wide);
            return Self::rustscan_to_filescan_ref(&rs, &name);
        }
        let mut scan = Self::scan_source(&source, &name);
        scan.file_name = name;
        scan
    }

    /// Every Python file under `root` (the scan walk; `SCAN_SKIP_DIRS` prunes
    /// venvs/caches/build output). Shared by the full gate scan and the LSP's
    /// incremental save path — the LSP stats these paths instead of re-scanning.
    fn repo_files(root: &Path) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if !SCAN_SKIP_DIRS.contains(&name) {
                        stack.push(p);
                    }
                } else if p.extension().is_some_and(|x| x == "py") {
                    paths.push(p);
                }
            }
        }
        paths.sort();
        paths
    }

    /// The repo-wide merge (every scan-derived family, reconciled through each
    /// file's suppressions) over a set of scans — the expensive part of a
    /// repo-wide scan is the per-file scan, so the LSP reuses cached scans and
    /// re-runs only this pure in-memory aggregation on save.
    fn repo_wide_merge(scans: &[FileScan], root_s: &str) -> std::collections::HashMap<String, Vec<Finding>> {
        crate::repo_scan::RepoScan::of(scans, root_s).by_file()
    }

    /// The Rust layer's scan into the shared FileScan shape. The Python-specific
    /// collections (defs/refs/strings/decorated/classes/imports) stay empty:
    /// `unused` is rustc dead_code's job and the graph families need the Rust
    /// exporter — the rustscan data (mod/use/structs) travels via `RustScan`.
    fn rustscan_to_filescan_ref(rs: &rustscan::RustScan, name: &str) -> FileScan {
        let supps = rs.supps.clone();
        FileScan {
            file_name: name.to_string(),
            findings: rs.findings.clone(),
            cc: rs.cc.clone(),
            errors: rs.errors,
            defs: Vec::new(),
            class_defs: Vec::new(),
            scattered: Vec::new(),
            duplicate_expressions: Vec::new(),
            shared_constructions: Vec::new(),
            path_expressions: Vec::new(),
            module_shape: rs.module_shape.clone(),
            setters: Vec::new(),
            refs: HashSet::new(),
            strings: Vec::new(),
            decorated: HashSet::new(),
            skeletons: rs.skeletons.clone(),
            skeleton_modules: Vec::new(),
            forwarder_fns: Vec::new(),
            forwarder_methods: Vec::new(),
            repo_methods: Vec::new(),
            middle_man_classes: Vec::new(),
            middle_man_candidates: Vec::new(),
            sibling_classes: Vec::new(),
            classes: Vec::new(),
            imports: Vec::new(),
            self_assigns: Vec::new(),
            supps,
            supps_spent: rs.supps_spent.clone(),
        }
    }
    /// Suppression census: how many `lucidlint: ignore` sites each signal has
    /// across the scanned set (line-level AND file-level). The census feeds the
    /// report's `suppressions` field and the bulk-suppression warning - repeated
    /// identical whys are a policy decision, not per-site judgment.
    fn suppression_counts<'a>(
        supps_list: impl IntoIterator<Item = &'a crate::suppressions::Suppressions>,
    ) -> std::collections::HashMap<String, usize> {
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for supps in supps_list {
            for entries in supps.line.values() {
                for (sig, _) in entries {
                    *counts.entry(sig.clone()).or_insert(0) += 1;
                }
            }
            for sig in supps.file.keys() {
                *counts.entry(sig.clone()).or_insert(0) += 1;
            }
        }
        counts
    }

    /// One warn finding per signal suppressed at >= threshold sites, attached to
    /// the file carrying the most of them. Visibility, not enforcement: the
    /// pattern itself (mass per-site suppression) is what a reviewer must see.
    fn bulk_suppression_findings(
        counts: &std::collections::HashMap<String, usize>,
        supps_by_rel: &std::collections::HashMap<String, crate::suppressions::Suppressions>,
        threshold: usize,
    ) -> Vec<Finding> {
        let mut out = Vec::new();
        for (kind, n) in counts {
            if *n < threshold {
                continue;
            }
            let mut top: Option<(&String, usize)> = None;
            for (rel, supps) in supps_by_rel {
                let sites = supps.line.values().flatten().filter(|(sig, _)| sig == kind).count()
                    + supps.file.keys().filter(|k| k.as_str() == kind).count();
                if sites > 0 && top.is_none_or(|(_, c)| sites > c) {
                    top = Some((rel, sites));
                }
            }
            out.push(Finding { logical_start: None, seam_members: Vec::new(), file: top.map(|(r, _)| r.clone()).unwrap_or_default(), line: 1, col: 0, function: String::new(), kind: "bulk-suppression".into(), severity: "warn".into(), message: format!(
            "{kind} suppressed at {n} sites - repeated identical whys are POLICY, not per-site judgment: \
        move the rule into [lucidlint.guidance] config guidance or a documented config ignore, or fix the recurring cause"
        ) });
        }
        out.sort_by(|a, b| a.kind.cmp(&b.kind));
        out
    }
}

// lucidlint: ignore large-function the CLI orchestrates every repo-wide family in one flow — extracting helpers would thread six collections
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--version") {
        // the release pipeline stamps CODE_HEALTH_VERSION at build time; the
        // crate version is the local/dev fallback
        let version = option_env!("CODE_HEALTH_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
        println!("lucidlint {version}");
        return;
    }
    if args.first().map(String::as_str) == Some("--lsp") {
        // stdio JSON-RPC language server — in-process scans, no spawns
        lsp::LspState::run();
        return;
    }
    if args.first().map(String::as_str) == Some("--fix") {
        // the Rust fix surface the orchestrator dispatches to for `.rs`
        // targets: `--fix '{"kind":...,"file":...,"line":N,"name":...}'`.
        // Reads the file, applies the fix IN PLACE, prints a status line.
        if let Some(spec) = args.get(1) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(spec) {
                let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                let file = v.get("file").and_then(|k| k.as_str()).unwrap_or("");
                let line = v.get("line").and_then(|k| k.as_u64()).unwrap_or(0) as usize;
                let name = v.get("name").and_then(|k| k.as_str()).unwrap_or("");
                if !RUST_FIX_KINDS.contains(&kind) {
                    // the request names a kind with no engine — the honest
                    // refusal names the gap (every shipped family runs
                    // through this gate; an unlisted kind is a typo or a
                    // future family)
                    println!("fix: {kind} has no Rust auto-fix - the Rust engine fixes extract-method, dispatch-registry, rule-table, loop-pipeline, loop-sequence, loop-hoist only; the report line for it says what to change");
                    std::process::exit(2);
                }
                if RUST_FIX_KINDS.contains(&kind) {
                    let src = match std::fs::read_to_string(file) {
                        Ok(s) => s,
                        Err(e) => {
                            // R29: an unexpected error surfaces the ACTUAL
                            // error — a missing/unreadable file is NOT
                            // "nothing to change" (which would fabricate a
                            // silent success for a finding that may exist)
                            eprintln!("fix: cannot read {file}: {e}");
                            std::process::exit(1);
                        }
                    };
                    let result = if kind == "dispatch-registry" {
                        fix::fix_dispatch_registry(&src, line)
                    } else if kind == "rule-table" {
                        fix::RuleTable::fix_rule_table(&src, line)
                    } else if kind == "loop-pipeline" {
                        fix::LoopPipeline::fix(&src, line)
                    } else if kind == "loop-sequence" {
                        fix::fix_loop_sequence(&src, line)
                    } else if kind == "loop-hoist" {
                        fix::LoopHoist::fix(&src, line, name)
                    } else {
                        fix::fix_extract_method(&src, line, name)
                    };
                    match result {
                        Ok(out) => {
                            if std::fs::write(file, out).is_err() {
                                println!("fix: could not write the fixed file — {file}:{line}");
                                return;
                            }
                            let what = if kind == "dispatch-registry" {
                                "converted the dispatch chain into a match"
                            } else if kind == "rule-table" {
                                "hoisted the if/append checks into a (condition, violation) table"
                            } else if kind == "loop-pipeline" {
                                "rewrote the loop as a combinator"
                            } else if kind == "loop-sequence" {
                                "concatenated the loop chain into one combinator"
                            } else if kind == "loop-hoist" {
                                "hoisted the loop body into a helper"
                            } else {
                                "extracted seam into a named function"
                            };
                            println!("fix: {what} — {file}:{line} ({kind})");
                            return;
                        }
                        Err(_) => {
                            // R28: never explain an absent fix — the
                            // silence is the signal
                            println!("fix: nothing to change for {kind} at {file}:{line}");
                            return;
                        }
                    }
                }
            }
        }
        println!("fix: unknown or malformed --fix request");
        std::process::exit(2);
    }
    // flag parsing: --graph <json> --churn <json> --docs <root> --include-tests
    let mut graph_path: Option<String> = None;
    let mut churn_path: Option<String> = None;
    let mut coverage_path: Option<String> = None;
    let mut docs_root: Option<String> = None;
    let mut gitignored_json: Option<String> = None;
    let mut root_override: Option<String> = None;
    let mut include_tests = false;
    let mut i = 0usize;
    let mut paths: Vec<String> = Vec::new();
    while i < args.len() {
        match args[i].as_str() {
            "--graph" => {
                i += 1;
                graph_path = args.get(i).cloned();
            }
            "--churn" => {
                i += 1;
                churn_path = args.get(i).cloned();
            }
            "--coverage" => {
                i += 1;
                coverage_path = args.get(i).cloned();
            }
            "--docs" => {
                i += 1;
                docs_root = args.get(i).cloned();
            }
            "--gitignored" => {
                i += 1;
                gitignored_json = args.get(i).cloned();
            }
            "--root" => {
                i += 1;
                root_override = args.get(i).cloned();
            }
            "--include-tests" => include_tests = true,
            _ => paths.push(args[i].clone()),
        }
        i += 1;
    }
    let root = Paths::scan_root(&paths, root_override.as_deref());
    let mut scans = Vec::new();
    // per-file scans are independent — parse in parallel (rayon), then the
    // repo-wide passes consume the scans in ORDER (duplicate first-seen,
    // unused refs, cycles all depend on the deterministic file order)
    let paired: Vec<(Option<rustscan::RustScan>, FileScan)> = paths
        .par_iter()
        .map(|path| {
            if path.ends_with(".rs") {
                let source = std::fs::read_to_string(path).unwrap_or_default();
                let repo_wide = true;
                let rs = rustscan::RustScan::scan(&source, path, repo_wide);
                let rel = path.clone();
                let fs = Pipeline::rustscan_to_filescan_ref(&rs, &rel);
                (Some(rs), fs)
            } else {
                (None, Pipeline::scan_file(Path::new(path)))
            }
        })
        .collect();
    let mut rust_scans: Vec<rustscan::RustScan> = Vec::new();
    for (rs, fs) in paired {
        if let Some(r) = rs {
            rust_scans.push(r);
        }
        scans.push(fs);
    }
    // The repo-wide families read the whole scan set: the corpus gathers the
    // pools once and reconciles each family through the files' suppressions —
    // an `ignore unused <why>` suppresses the finding and is not stale.
    let corpus = RepoScan::of(&scans, &root);
    let mut all_findings = Vec::new();
    let mut all_cc = Vec::new();
    let mut total_errors = 0usize;
    for scan in &scans {
        all_findings.extend(scan.findings.clone());
        all_cc.extend(scan.cc.clone());
        total_errors += scan.errors;
    }
    // the suppression census feeds the report's ledger and the bulk warning —
    // a report family over what the scan set carries, not a per-file rule
    let supps_by_rel: std::collections::HashMap<String, crate::suppressions::Suppressions> = scans
        .iter()
        .map(|s| (Paths::rel_of(&s.file_name, &root), s.supps.clone()))
        .collect();
    let suppression_census = Pipeline::suppression_counts(scans.iter().map(|s| &s.supps));
    let bulk_suppressions =
        Pipeline::bulk_suppression_findings(&suppression_census, &supps_by_rel, BULK_SUPPRESSION_SITES);
    all_findings.extend(bulk_suppressions);
    corpus.reconcile(&mut all_findings);
    // flat-folder: what a reader browses per directory — modules + docs,
    // counted from the scan set and the docs root (no graph needed)
    let mut module_rels: Vec<String> = Vec::new();
    let mut doc_rels: Vec<String> = Vec::new();
    for p in &paths {
        let rel = Paths::rel_of(p, &root);
        // count what the scan browses: production code unless the caller asked
        // for tests, and never upstream's vendored tree
        if Paths::is_out_of_scan(include_tests, &rel) {
            continue;
        }
        if p.ends_with(".md") {
            doc_rels.push(rel);
        } else {
            module_rels.push(rel);
        }
    }
    all_findings.extend(checks::FlatFolder::findings(&module_rels, &doc_rels));
    // import cycles for Rust crates: the local mod/use graph (the
    // code-review-graph contract is Python-only; Rust resolves itself)
    if !rust_scans.is_empty() {
        let mut mod_decls: std::collections::HashMap<String, Vec<(String, bool)>> = std::collections::HashMap::new();
        let mut uses: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        for rs in &rust_scans {
            let rel = Paths::rel_of(&rs.file_name(), &root);
            mod_decls.insert(rel.clone(), rs.mod_decls.clone());
            uses.insert(rel, rs.uses.clone());
        }
        let rels: Vec<String> = paths.iter().map(|p| Paths::rel_of(p, &root)).collect();
        let graph = rustscan::ModuleGraph::module_graph(&rels, &mod_decls, &uses);
        // the module graph keys are rels to the binary's computed root — the
        // orchestrator's scan set is repo-relative, so re-anchor the findings
        // to absolute paths (resolution-neutral, like the per-file findings)
        let mut cycle_findings = graph_families::Cycles::findings_for(&graph);
        for f in &mut cycle_findings {
            if !f.file.starts_with('/') {
                f.file = format!("{}/{}", root, f.file);
            }
        }
        all_findings.extend(cycle_findings);
    }
    // repo-wide families from the graph contract + git churn (orchestrator
    // gathers both; the findings compute here)
    let contract = graph_path
        .as_deref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<graph_families::GraphContract>(&s).ok());
    // the contract version is part of the interface — a mismatched version
    // means the exporter and the scan core disagree; degrade to the
    // non-graph families rather than trusting unknown shapes
    if contract.as_ref().is_some_and(|c| c.contract_version != 1) {
        eprintln!(
            "graph contract version {} — expected 1; graph families skipped",
            contract.as_ref().unwrap().contract_version
        );
        // drop the contract so the graph families are skipped
    }
    let contract = if contract.as_ref().is_some_and(|c| c.contract_version == 1) {
        contract
    } else {
        None
    };
    let rels: Vec<String> = paths.iter().map(|p| Paths::rel_of(p, &root)).collect();
    if let Some(c) = &contract {
        let repo_root = Path::new(&root);
        all_findings.extend(graph_families::HighRisk::findings(
            repo_root,
            c,
            HIGH_RISK_MIN,
            include_tests,
        ));
        all_findings.extend(graph_families::Cycles::findings(repo_root, c));
        all_findings.extend(graph_families::LayerMix::findings(repo_root, c, &rels));
        all_findings.extend(graph_families::FolderMix::findings(repo_root, c));
        all_findings.extend(graph_families::Cohesion::findings(repo_root, c, COHESION_SPAN));
    }
    if let Some(d) = &docs_root {
        let gitignored: std::collections::HashSet<String> = gitignored_json
            .as_ref()
            .and_then(|j| serde_json::from_str::<Vec<String>>(j).ok())
            .map(|v| v.into_iter().collect())
            .unwrap_or_default();
        all_findings.extend(docs::Docs::docs_findings(Path::new(d), &gitignored));
    }
    if let Some(p) = &churn_path {
        if let Ok(s) = std::fs::read_to_string(p) {
            if let Ok(churn) = serde_json::from_str::<std::collections::HashMap<String, usize>>(&s) {
                // max CC per file from the scan itself — recompute without the graph
                let mut cc_any: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
                for e in &all_cc {
                    let rel = Paths::rel_of(&e.file, &root);
                    let slot = cc_any.entry(rel).or_insert(0);
                    *slot = (*slot).max(e.cc);
                }
                all_findings.extend(graph_families::Hotspot::findings(
                    &churn,
                    &cc_any,
                    TOP_FRACTION,
                    COMPLEXITY_GATE,
                ));
                // churn-untested: the repo's OWN coverage snapshot decides
                // (coverage.xml / .coverage, R6) — never the graph's TESTED_BY
                // approximation, which misses body imports and cannot speak
                // for Rust at all
                let coverage = coverage_path
                    .as_deref()
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .and_then(|s| serde_json::from_str::<graph_families::CoverageSnapshot>(&s).ok());
                all_findings.extend(graph_families::ChurnUntested::findings(
                    &churn,
                    coverage.as_ref(),
                    TOP_FRACTION,
                ));
            }
        }
    }
    // complexity findings from the scan's own CC array (the gate threshold)
    let contract_complexity: Vec<serde_json::Value> = all_cc
        .iter()
        .filter(|e| e.cc >= COMPLEXITY_GATE)
        .map(|e| {
            serde_json::json!({
                "file": e.file,
                "line": e.line,
                "function": e.function,
                "kind": "complexity",
                "severity": "fail",
                "message": crate::fix_render::FixRender::full_fix_command(
                    &e.file,
                    e.line,
                    &crate::fix_render::FixRender::complexity_message(e.cc, e.shape, &e.shape_detail),
                ),
                "fix_kind": checks::FixRender::fix_kind_of_message(&crate::fix_render::FixRender::complexity_message(e.cc, e.shape, &e.shape_detail))
                    .unwrap_or("extract-method"),
            })
        })
        .collect();
    // The families the gate composed itself — flat-folder, the Rust mod graph,
    // the graph contract, docs, churn, the bulk warning — were appended after
    // the per-file pass, so their markers were never consumed: filter them
    // through each file's suppressions now. A marker this pass uses clears the
    // stale-suppression finding the per-file pass left behind (PRD R18,
    // review-log B3).
    all_findings = corpus.apply_suppressions(all_findings);

    let contract_findings: Vec<serde_json::Value> = all_findings
        .iter()
        .map(|f| {
            serde_json::json!({
                "file": f.file,
                "line": f.line,
                "col": f.col,
                "function": f.function,
                "kind": final_kind(&f.kind),
                "signal": f.kind,
                "severity": f.severity,
                "message": crate::fix_render::FixRender::full_fix_command(&f.file, f.line, &f.message),
                "seam_members": f.seam_members,
                "fix_kind": FixRender::fix_kind_of_message(&f.message).unwrap_or_else(|| FixRender::kind_fix_default(&f.kind)),
            })
        })
        .collect();
    let out = serde_json::json!({
        "schema_version": 4,
        "header": crate::report::REPORT_HEADER,
        "suppressions": suppression_census,
        "files": paths.len(),
        "parse_errors": total_errors,
        "findings": contract_findings,
        "cc": all_cc,
        "complexity": contract_complexity,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

/// The fix kinds the Rust engine implements.
const RUST_FIX_KINDS: &[&str] = &[
    "extract-method",
    "dispatch-registry",
    "rule-table",
    "loop-pipeline",
    "loop-sequence",
    "loop-hoist",
];

#[cfg(test)]
mod tests {
    use super::*;

    use crate::python_shape::FnShape;
    use ruff_python_ast::Stmt;

    use crate::paths::Paths;

    #[test]
    fn full_fix_command_never_fabricates_name_from_prose() {
        // the plain complexity directive is machine form (kind only) — the
        // rewritten command must carry NO fabricated --name (the regression:
        // the prose "--fix-name <name>" slot started a token with a
        // semicolon and produced "--name apply" for every plain-shape
        // complexity finding)
        let msg = crate::fix_render::FixRender::complexity_message(16, "plain", "x");
        let cmd = crate::fix_render::FixRender::full_fix_command("src/a.py", 5, &msg);
        assert!(cmd.contains("--kind extract-method"), "{cmd}");
        assert!(!cmd.contains("--name"), "{cmd}");
        assert!(!cmd.contains("apply"), "{cmd}");
        // name-bearing directives keep their exact placeholder slot
        let m2 = crate::fix_render::FixRender::full_fix_command(
            "src/a.py",
            5,
            "magic number 3 \u{2014} name it as a constant \u{2014} fix: magic-number --fix-name <CONST>",
        );
        assert!(m2.contains("--name <CONST>"), "{m2}");
        // a prose token merely STARTING with --fix-name is ignored (exact match)
        let m3 = crate::fix_render::FixRender::full_fix_command(
            "src/a.py",
            5,
            "x \u{2014} fix: extract-method (preview; --fix-name; prose)",
        );
        assert!(!m3.contains("--name"), "{m3}");
    }
    #[test]
    fn repo_root_resolves_relative_paths_against_cwd() {
        // the orchestrator passes REPO-RELATIVE paths with the binary's cwd
        // at the repo root: the raw common prefix of tests/a.py + tools/b.py
        // is "t" — which used to back off to "" and silently broke every
        // graph-family rel resolution (large-function/high-risk/
        // layer-mix/folder-mix never fired through the orchestrator)
        let cwd = Path::new("/work/repo");
        let root = Paths::repo_root_impl(
            &[
                "tests/a.py".to_string(),
                "tests/b.py".to_string(),
                "tools/c.py".to_string(),
            ],
            cwd,
        );
        assert_eq!(root, "/work/repo");
    }

    #[test]
    fn repo_root_relative_paths_in_one_directory_is_the_cwd() {
        // every scanned source in ONE package: the common prefix of
        // pkg/a.py + pkg/b.py is .../pkg, which is not the repo root. The
        // repo-wide families then report "a.py", the orchestrator cannot map
        // it back into the scan set, and duplicate/unused silently vanish
        // (the single-package src/ or lib/ layout — the common case).
        // Relative inputs are resolved against the cwd FIRST, so the cwd IS
        // the scan root the caller passed them from.
        let cwd = Path::new("/work/repo");
        let root = Paths::repo_root_impl(&["pkg/a.py".to_string(), "pkg/b.py".to_string()], cwd);
        assert_eq!(root, "/work/repo");
        let single = Paths::repo_root_impl(&["pkg/only.py".to_string()], cwd);
        assert_eq!(single, "/work/repo");
    }

    #[test]
    fn repo_root_mixed_inputs_anchor_at_the_cwd() {
        // a relative half is defined relative to the cwd — the prefix of a
        // mixed pair cannot be trusted
        let cwd = Path::new("/work/repo");
        let root = Paths::repo_root_impl(&["/elsewhere/core.py".to_string(), "pkg/a.py".to_string()], cwd);
        assert_eq!(root, "/work/repo");
    }

    #[test]
    fn explicit_root_beats_inference() {
        // the orchestrator names the scan root; the file layout must not
        // decide it
        let paths = vec!["pkg/a.py".to_string(), "pkg/b.py".to_string()];
        assert_eq!(Paths::scan_root(&paths, Some("/work/repo")), "/work/repo");
        assert_eq!(Paths::scan_root(&paths, None), Paths::repo_root(&paths));
    }

    #[test]
    fn repo_root_absolute_paths_unchanged() {
        let cwd = Path::new("/work/repo");
        let root = Paths::repo_root_impl(
            &[
                "/home/u/proj/tests/a.py".to_string(),
                "/home/u/proj/tools/b.py".to_string(),
            ],
            cwd,
        );
        assert_eq!(root, "/home/u/proj");
    }

    #[test]
    fn fix_command_carries_params_slot() {
        // the extract-module directive names the seam's members via --params —
        // the rewrite must carry them or the agent gets a command the fix
        let msg = "module x holds 2 domains — fix: extract-module --fix-name text --params tokenize,words";

        let cmd = crate::fix_render::FixRender::full_fix_command("tools/layout.py", 0, msg);
        assert!(cmd.contains("--kind extract-module"), "{cmd}");
        assert!(cmd.contains("--name text"), "{cmd}");
        assert!(cmd.contains("--params tokenize,words"), "{cmd}");
    }

    fn scan_src(src: &str) -> Vec<Finding> {
        scan_corpus(&[("prod_mod.py", src)])
    }

    /// The first top-level function's body — for shape-classifier tests.
    fn first_py_fn_body(src: &str) -> Vec<Stmt> {
        let parsed = parse_module(src).unwrap();
        let module = parsed.syntax();
        match module.body.first().unwrap() {
            Stmt::FunctionDef(f) => f.body.iter().cloned().collect(),
            _ => panic!("the first statement is not a function"),
        }
    }

    /// The test-only families (monkeypatch/skipif/fakefs) run for test paths.
    fn scan_src_test(src: &str) -> Vec<Finding> {
        scan_corpus(&[("tests/test_mod.py", src)])
    }

    fn scan_cc(src: &str) -> (Vec<Finding>, Vec<FnCc>, usize) {
        let scan = Pipeline::scan_source(src, "prod_mod.py");
        (scan.findings, scan.cc, scan.errors)
    }

    fn kinds(findings: &[Finding]) -> Vec<&str> {
        let mut v: Vec<&str> = findings.iter().map(|f| f.kind.as_str()).collect();
        v.sort_unstable();
        v
    }

    // ------------------------------------------------------------- CC
    #[test]
    fn cc_if_elif_else_counts_elifs_not_else() {
        let src = include_str!("../../tests/fixtures/code/sample_cc_if_elif_else_counts_elifs_not_else__041fad56.py");
        let (_, cc, _) = scan_cc(src);
        assert_eq!(cc[0].cc, 3); // if + elif; trailing else does NOT count
    }

    #[test]
    fn cc_loops_try_assert_match_boolop() {
        let src = include_str!("../../tests/fixtures/rust/cc_loops_try_assert_match_boolop__01.py");
        let (_, cc, _) = scan_cc(src);
        assert_eq!(cc[0].cc, 8); // for+else(2) if(1) try+handler+else(3) assert(1) match-case(1) + base 1 — the boolop under assert does NOT count (radon's visit_Assert never recurses)
    }

    #[test]
    fn cc_nested_and_class_excluded() {
        let src = include_str!("../../tests/fixtures/rust/cc_nested_and_class_excluded__01.py");
        let (_, cc, _) = scan_cc(src);
        assert_eq!(cc.len(), 1);
        assert_eq!(cc[0].cc, 2); // only the outer if counts
    }

    #[test]
    fn cc_assert_does_not_recurse() {
        // radon's visit_Assert short-circuits: boolop/ternary/comp inside
        // an assert contribute nothing — only the assert itself counts
        let src = include_str!("../../tests/fixtures/rust/cc_assert_does_not_recurse__01.py");
        let (_, cc, _) = scan_cc(src);
        assert_eq!(cc[0].cc, 3); // base + 2 asserts; the boolop/comp/ifs do not count
    }

    #[test]
    fn cc_lambda_zero_but_body_walks() {
        let src = include_str!("../../tests/fixtures/rust/cc_lambda_zero_but_body_walks__01.py");
        let (_, cc, _) = scan_cc(src);
        assert_eq!(cc[0].cc, 2); // lambda +0, inner ternary +1
    }

    #[test]
    fn cc_comprehension_counts_each_generator() {
        let src = include_str!("../../tests/fixtures/rust/cc_comprehension_counts_each_generator__01.py");
        let (_, cc, _) = scan_cc(src);
        assert_eq!(cc[0].cc, 4); // 2 generators + 1 if + base
    }

    // ------------------------------------------------------------- magic
    #[test]
    fn magic_skips_lookup_table_and_small_literals() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/magic_skips_lookup_table_and_small_literals__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "magic-number")); // 10/20 in a dict literal, 1 and 0 skipped
    }

    #[test]
    fn magic_operand_found_index_exempt() {
        // B8: a plain integer subscript index is a position code, not domain
        // magic — cols[7] does not fire; the operand 60 still does, and the
        // message names the function (A5)
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/magic_operand_and_index_found__01.py"
        ));
        let m: Vec<&Finding> = f.iter().filter(|x| x.kind == "magic-number").collect();
        assert_eq!(m.len(), 1, "{f:?}");
        assert_eq!(m[0].function, "f");
        assert!(
            m[0].message
                .contains("magic number 60 in f — nothing states what it means or why this magnitude"),
            "{}",
            m[0].message
        );
        assert!(
            m[0].message
                .contains("Write it as a named constant where the computation uses it"),
            "{}",
            m[0].message
        );
    }

    #[test]
    fn magic_octal_hex_binary_literals_are_intent_bearing() {
        // B8: 0o600/0xFF/0b1010 are intent-bearing spellings — the meaning is
        // in the literal (the old code rendered 0o600 as "magic number 384")
        let f = scan_src(include_str!(
            "../../tests/fixtures/code/sample_magic_octal_hex_binary_literals_are_intent_bearing__fe6d7f9f.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "magic-number"), "{f:?}");
        // the same VALUES in decimal form are not intent-bearing — still flagged
        let dec = scan_src(include_str!(
            "../../tests/fixtures/code/sample_magic_octal_hex_binary_literals_are_intent_bearing__9092f3e6.py"
        ));
        assert!(dec.iter().any(|x| x.kind == "magic-number"), "{dec:?}");
    }

    #[test]
    fn magic_named_constant_rhs_is_exempt() {
        // B8: the RHS of a module-level named constant IS the why — the whole
        // value is exempt (`RATE = 2 * 60`, `MARGIN = -12`); the same literal
        // inside a function still fires
        let f = scan_src(include_str!(
            "../../tests/fixtures/code/sample_magic_named_constant_rhs_is_exempt__707f1b5a.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "magic-number"), "{f:?}");
        let neg = scan_src(include_str!(
            "../../tests/fixtures/code/sample_magic_named_constant_rhs_is_exempt__5cec87ad.py"
        ));
        assert!(!neg.iter().any(|x| x.kind == "magic-number"), "{neg:?}");
        let g = scan_src(include_str!(
            "../../tests/fixtures/code/sample_magic_named_constant_rhs_is_exempt__f66f74f4.py"
        ));
        assert!(g.iter().any(|x| x.kind == "magic-number"), "{g:?}");
    }

    #[test]
    fn magic_length_guards_and_subscript_indexes_are_not_findings() {
        // B8: a tuple index and a len() length guard are structural, not
        // domain magic
        let f = scan_src(include_str!(
            "../../tests/fixtures/code/sample_magic_length_guards_and_subscript_indexes_are_not_findings__6678bb90.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "magic-number"), "{f:?}");
        // an operand in the same function still fires
        let g = scan_src(include_str!(
            "../../tests/fixtures/code/sample_magic_length_guards_and_subscript_indexes_are_not_findings__a612fad2.py"
        ));
        let m: Vec<&Finding> = g.iter().filter(|x| x.kind == "magic-number").collect();
        assert_eq!(m.len(), 1, "{g:?}");
        assert!(m[0].message.contains("60"), "{}", m[0].message);
    }

    #[test]
    fn magic_data_table_pair_in_dict_initializer_clears_the_bar() {
        // B8: the 4x12 pair in one dict initializer is table data — the
        // >= 3 same-kind siblings exemption counts by numeric kind (int vs
        // float), so a third sibling clears the bar
        let f = scan_src(include_str!(
            "../../tests/fixtures/code/sample_magic_data_table_pair_in_dict_initializer_clears_the_bar__bafaed56.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "magic-number"), "{f:?}");
        // two siblings alone do NOT clear the >= 3 bar — still flagged
        let g = scan_src(include_str!(
            "../../tests/fixtures/code/sample_magic_data_table_pair_in_dict_initializer_clears_the_bar__d6c19b47.py"
        ));
        assert!(g.iter().filter(|x| x.kind == "magic-number").count() >= 2, "{g:?}");
    }

    #[test]
    fn magic_keyword_value_is_not_a_finding() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/magic_keyword_value_is_not_a_finding__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "magic-number"));
    }
    #[test]
    fn magic_unit_constructor_call_is_exempt() {
        // P2: the unit is stated at the call site — Quantity(20, 'minute')
        // and timedelta(minutes=20) name their unit (constructor callee or a
        // unit-naming keyword); the literal is named there, no finding.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/magic_unit_constructor_exempt__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "magic-number"), "{f:?}");
    }

    #[test]
    fn magic_unit_named_assignment_and_param_are_exempt() {
        // P2: a literal whose nearest assignment target — or an enclosing
        // function's parameter — names the unit (max_walk_km = 2 * 30,
        // distance_m * 30) is named by that name, not magic.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/magic_unit_named_assignment_exempt__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "magic-number"), "{f:?}");
    }

    #[test]
    fn magic_number_message_names_both_mechanisms_and_never_the_family() {
        // Phase 2: the Duration/Pint/Generic split is removed — the same
        // message renders at every firing site, naming both mechanisms as
        // glossed examples, never claiming which applies, and never naming a
        // unit. The 60-division conversion factor and the retry cap render
        // identical text; "is a duration" appears nowhere.
        let general = "nothing states what it means or why this magnitude";
        let mechanisms = "If the value carries units, express it in a type that understands them — Python's timedelta for durations, or a Quantity from the pint units library for physical measures";
        for (fx, src) in [
            (
                "divisor-60",
                include_str!("../../tests/fixtures/rust/magic_number_divisor_60_general__01.py"),
            ),
            (
                "retry-cap",
                include_str!("../../tests/fixtures/rust/magic_number_retry_cap_general__01.py"),
            ),
        ] {
            let f = scan_src(src);
            let m: Vec<&Finding> = f.iter().filter(|x| x.kind == "magic-number").collect();
            assert_eq!(m.len(), 1, "{fx}: {f:?}");
            assert!(m[0].message.contains(general), "{fx}: {}", m[0].message);
            assert!(m[0].message.contains(mechanisms), "{fx}: {}", m[0].message);
            assert!(!m[0].message.contains("is a duration"), "{fx}: {}", m[0].message);
            assert!(!m[0].message.contains("Quantity("), "{fx}: {}", m[0].message);
            assert!(!m[0].message.contains("timedelta("), "{fx}: {}", m[0].message);
            assert!(m[0].message.contains("--fix-name <CONST>"), "{fx}: {}", m[0].message);
        }
        // a unit-stating trailing comment no longer selects a family — the
        // same message, and no unit is named
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/magic_unit_pint_suggestion__01.py"
        ));
        let m: Vec<&Finding> = f.iter().filter(|x| x.kind == "magic-number").collect();
        assert_eq!(m.len(), 2, "{f:?}");
        for x in &m {
            assert!(x.message.contains(general), "{}", x.message);
            assert!(x.message.contains(mechanisms), "{}", x.message);
            assert!(!x.message.contains("kilometer"), "{}", x.message);
        }
    }

    #[test]
    fn duplicate_fast_path_matches_reference_dice() {
        // the sorted-index-bigram fast path must agree with the reference
        // multiset dice on the pinned repetition cases (2026-08-17 review-log)
        use crate::skeleton::Similarity;
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let rep = s(&["a", "b", "a", "b"]);
        let uniq = s(&["a", "b", "a"]);
        let interner = crate::skeleton::BigramInterner::new(&[&rep, &uniq]);
        let fa = interner.bigrams_of(0);
        let fb = interner.bigrams_of(1);
        assert_eq!(
            crate::skeleton::Similarity::dice_from_bigrams(fa, fb),
            Similarity::dice_similarity(&rep, &uniq)
        );
        assert_eq!(crate::skeleton::Similarity::dice_from_bigrams(fa, fa), 1.0);
        // empty skeletons: never a duplicate, matches the reference
        let e = Vec::<String>::new();
        let interner2 = crate::skeleton::BigramInterner::new(&[&e, &rep]);
        assert_eq!(
            crate::skeleton::Similarity::dice_from_bigrams(interner2.bigrams_of(0), interner2.bigrams_of(1)),
            Similarity::dice_similarity(&e, &rep)
        );
    }

    #[test]
    fn name_required_kinds_carries_fix_kinds() {
        // the LSP matches the directive's FIX kind — record-shape's fix is
        // extract-record-class, module-cohesion's is extract-module; rule
        // kinds alone never matched, so needsName never fired (review bot)
        assert!(crate::rules_gen::NAME_REQUIRED_KINDS.contains(&"extract-record-class"));
        assert!(crate::rules_gen::NAME_REQUIRED_KINDS.contains(&"extract-module"));
        assert!(crate::rules_gen::NAME_REQUIRED_KINDS.contains(&"extract-class"));
    }

    #[test]
    fn comment_lines_tracks_lines_across_multiline_tokens() {
        // starts; a multi-line string between comments must not lose count
        let src = "# one\nx = (\n    1 +\n    \"\"\"\nmulti\nline\n\"\"\"\n)\n# two\ny = 2  # three\n";
        let parsed = parse_module(src).unwrap();
        let lines = crate::suppressions::Suppressions::comment_lines(src, parsed.tokens());
        let got: Vec<(usize, &str)> = lines.iter().map(|(l, t)| (*l, t.as_str())).collect();
        assert_eq!(got, vec![(1, "# one"), (9, "# two"), (10, "# three")], "{got:?}");
    }
    #[test]

    fn magic_table_carve_out_exempts_data_tables_keeps_operands() {
        // statutory band ratios: same-kind ints in Div ops inside one dict —
        // the literal IS the data, naming each would destroy the table
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/magic_table_carve_out_exempts_data_tables_keeps_operands__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "magic-number"), "{f:?}");
        // postcode bounds: negative floats in UnaryOps inside tuples inside a dict
        let f2 = scan_src(include_str!(
            "../../tests/fixtures/rust/magic_table_carve_out_exempts_data_tables_keeps_operands__02.py"
        ));
        assert!(!f2.iter().any(|x| x.kind == "magic-number"), "{f2:?}");
        // two same-kind operands in a collection are NOT a table — still flagged
        let f3 = scan_src(include_str!(
            "../../tests/fixtures/rust/magic_table_carve_out_exempts_data_tables_keeps_operands__03.py"
        ));
        let m: Vec<&Finding> = f3.iter().filter(|x| x.kind == "magic-number").collect();
        assert_eq!(m.len(), 2, "{f3:?}");
        // ordinary operands outside any collection stay flagged
        let f4 = scan_src(include_str!(
            "../../tests/fixtures/rust/magic_table_carve_out_exempts_data_tables_keeps_operands__04.py"
        ));
        assert_eq!(f4.iter().filter(|x| x.kind == "magic-number").count(), 1);
        // the message names the data-table exemption so agents know the bar
        let f5 = scan_src(include_str!(
            "../../tests/fixtures/rust/magic_table_carve_out_exempts_data_tables_keeps_operands__05.py"
        ));
        let m5: Vec<&Finding> = f5.iter().filter(|x| x.kind == "magic-number").collect();
        assert!(m5[0].message.contains("data table"), "{}", m5[0].message);
    }

    #[test]
    fn positional_boolean_literals_are_found_keyword_exempt() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/positional_boolean_literals_are_found_keyword_exempt__01.py"
        ));
        let b: Vec<&Finding> = f.iter().filter(|x| x.kind == "boolean-arg").collect();
        assert_eq!(b.len(), 2); // g(True) positional and outer False; retry=True keyword exempt
        assert!(b.iter().all(|x| x.message.contains("name it")));
    }

    #[test]
    fn boolean_name_and_keyword_args_pass() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/boolean_name_and_keyword_args_pass__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "boolean-arg"));
    }

    #[test]
    fn boolean_lookup_defaults_are_exempt_other_calls_stay_flagged() {
        // the boolean is the DEFAULT for a missing key/attribute — the
        // "name it: retry=True" prescription is impossible for these
        // positional-only params, so the finding is unactionable noise
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/boolean_lookup_defaults_are_exempt_other_calls_stay_flagged__01.py"
        ));
        assert!(
            !f.iter().any(|x| x.kind == "boolean-arg"),
            "lookup defaults exempt: {:?}",
            f
        );
        // setattr is NOT a lookup-with-default: the trailing boolean is the
        // VALUE being assigned (nameable), so it stays a finding — the
        // exemption is for missing-key/attribute defaults only (review bot)
        let f3 = scan_src(include_str!(
            "../../tests/fixtures/rust/boolean_lookup_defaults_are_exempt_other_calls_stay_flagged__02.py"
        ));
        let b3: Vec<&Finding> = f3.iter().filter(|x| x.kind == "boolean-arg").collect();
        assert_eq!(b3.len(), 1, "setattr value must stay flagged: {:?}", f3);
        // a non-default boolean in a lookup call is still a finding
        let f2 = scan_src(include_str!(
            "../../tests/fixtures/rust/boolean_lookup_defaults_are_exempt_other_calls_stay_flagged__03.py"
        ));
        assert!(f2.iter().any(|x| x.kind == "boolean-arg"), "{:?}", f2);
        // a user function literally named `get`/`setdefault`/`pop` is NOT a
        // lookup-with-default — only ATTRIBUTE receivers and the bare
        // getattr builtin are exempt; a bare `get(...)` must stay flagged
        // (review bot)
        let f4 = scan_src(include_str!(
            "../../tests/fixtures/rust/boolean_lookup_defaults_are_exempt_other_calls_stay_flagged__04.py"
        ));
        assert!(f4.iter().any(|x| x.kind == "boolean-arg"), "{:?}", f4);
        let f5 = scan_src(include_str!(
            "../../tests/fixtures/rust/boolean_lookup_defaults_are_exempt_other_calls_stay_flagged__05.py"
        ));
        assert!(f5.iter().any(|x| x.kind == "boolean-arg"), "{:?}", f5);
        // the bare getattr builtin stays exempt (it IS a lookup)
        let f6 = scan_src(include_str!(
            "../../tests/fixtures/rust/boolean_lookup_defaults_are_exempt_other_calls_stay_flagged__06.py"
        ));
        assert!(!f6.iter().any(|x| x.kind == "boolean-arg"), "{:?}", f6);
    }

    #[test]
    fn magic_in_nested_function_attributes_to_innermost() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/magic_in_nested_function_attributes_to_innermost__01.py"
        ));
        let m: Vec<&Finding> = f.iter().filter(|x| x.kind == "magic-number").collect();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].function, "inner");
    }

    // ------------------------------------------------------------- noop
    #[test]
    fn noop_ternary_is_a_finding() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/noop_ternary_is_a_finding__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "noop-statement"));
    }

    #[test]
    fn noop_calls_and_docstrings_pass() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/noop_calls_and_docstrings_pass__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "noop-statement"));
    }

    // ------------------------------------------------------------- imports
    #[test]
    fn inline_import_in_function_and_method() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/inline_import_in_function_and_method__01.py"
        ));
        let inline: Vec<&Finding> = f.iter().filter(|x| x.kind == "inline-import").collect();
        assert_eq!(inline.len(), 2);
    }

    #[test]
    fn private_code_leakage_imports_and_module_attribute() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/private_code_leakage_imports_and_module_attribute__01.py"
        ));
        let leaks: Vec<&Finding> = f.iter().filter(|x| x.kind == "private-code-leakage").collect();
        assert_eq!(leaks.len(), 5); // two private imports + three module-attribute reaches
        assert!(
            leaks
                .iter()
                .any(|x| x.line == 11 && x.message.contains("houses.school._try_geopoint")),
            "the module-path reach is named at its line: {leaks:?}"
        );
        assert!(
            !leaks.iter().any(|x| x.message.contains("self._value")),
            "a class reaching its own private attribute is not leakage: {leaks:?}"
        );
        assert!(
            !leaks.iter().any(|x| x.message.contains("__future__")),
            "__future__ is skipped: {leaks:?}"
        );
    }

    #[test]
    fn private_code_leakage_package_facade_reassembles_its_own_names() {
        let own = include_str!("../../tests/fixtures/rust/private_code_leakage_package_facade__01.py");
        let facade = scan_corpus(&[("pkg/__init__.py", own)]);
        assert!(
            !facade.iter().any(|x| x.kind == "private-code-leakage"),
            "a package __init__ may reassemble its own package's names: {facade:?}"
        );
        let peer = scan_corpus(&[("pkg/peer.py", own)]);
        assert!(
            peer.iter().any(|x| x.kind == "private-code-leakage"),
            "a peer module may not reach the same private name: {peer:?}"
        );
        let foreign = include_str!("../../tests/fixtures/rust/private_code_leakage_foreign_package__01.py");
        let facade = scan_corpus(&[("pkg/__init__.py", foreign)]);
        assert!(
            facade.iter().any(|x| x.kind == "private-code-leakage"),
            "another package's private name is leakage even in a facade: {facade:?}"
        );
        // a package whose name only shares a prefix is not the same package
        let prefixed = include_str!("../../tests/fixtures/rust/private_code_leakage_prefixed_package__01.py");
        let facade = scan_corpus(&[("pkg/__init__.py", prefixed)]);
        assert!(
            facade.iter().any(|x| x.kind == "private-code-leakage"),
            "pkg_extra is not pkg: {facade:?}"
        );
    }

    // ------------------------------------------------------------- unreachable
    #[test]
    fn unreachable_after_return() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/unreachable_after_return__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "unreachable" && x.line == 3));
    }

    #[test]
    fn unreachable_inside_nested_if_not_flagged() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/unreachable_inside_nested_if_not_flagged__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "unreachable"));
    }

    // ------------------------------------------------------------- suppressions
    #[test]
    fn suppression_with_why_exempts() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/suppression_with_why_exempts__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn suppression_without_why_is_a_finding() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/suppression_without_why_is_a_finding__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "suppression"));
        assert!(f.iter().any(|x| x.kind == "swallow")); // not actually exempted
    }

    #[test]
    fn suppression_on_line_above_exempts() {
        // the comment must sit on the finding's line or line-1 — the except
        // handler is line 5, so line 4 (the try) is the line-1 position
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/suppression_on_line_above_exempts__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn ignore_file_exempts() {
        let src = include_str!("../../tests/fixtures/rust/ignore_file_exempts__01.py");
        let f = scan_src(src);
        assert!(!f.iter().any(|x| x.kind == "class-module"));
    }

    // ------------------------------------------------------------- type-ignore
    #[test]
    fn type_ignore_without_why_is_a_finding() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/type_ignore_without_why_is_a_finding__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "type-ignore" && x.line == 1));
    }

    #[test]
    fn type_ignore_with_why_passes() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/type_ignore_with_why_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "type-ignore"));
    }

    #[test]
    fn noqa_without_why_is_a_finding() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/noqa_without_why_is_a_finding__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "noqa" && x.line == 1));
    }

    #[test]
    fn pragma_no_cover_without_why_is_a_finding() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/pragma_no_cover_without_why_is_a_finding__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "noqa"));
    }

    #[test]
    fn noqa_with_why_passes() {
        let f = scan_src(include_str!("../../tests/fixtures/rust/noqa_with_why_passes__01.py"));
        assert!(!f.iter().any(|x| x.kind == "noqa"));
    }

    #[test]
    fn noqa_reason_on_the_same_comment_line_passes() {
        // the repo writes `# noqa: BLE001 — the callback must never 500` —
        // one comment with a real reason. The rule's requirement of a SECOND
        // `#` after the marker (`# noqa: X  # reason`) rejects that natural
        // format: a reason is a reason whether or not it follows a `#`.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/noqa_reason_on_the_same_comment_line_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "noqa"), "{f:?}");
        // and the no-reason form still fails
        let g = scan_src(include_str!(
            "../../tests/fixtures/rust/noqa_reason_on_the_same_comment_line_passes__02.py"
        ));
        assert!(g.iter().any(|x| x.kind == "noqa"));
    }

    // ------------------------------------------------------------- global-state
    #[test]
    fn module_literal_assign_and_annassign_are_findings() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/module_literal_assign_and_annassign_are_findings__01.py"
        ));
        let gs: Vec<&Finding> = f.iter().filter(|x| x.kind == "global-state").collect();
        assert_eq!(gs.len(), 2);
        // Both fixtures are mutable containers, so the AST already says which
        // action applies: the owning class's attribute, never a parameter
        // default (a mutable default argument is a bug of its own).
        // #79: the old message said "keep it here" for a constant the
        // module's own functions read — an exemption the rule does not have,
        // so following it never cleared the finding and led to a suppression
        // wall. The message must not offer a shape the rule still fails, and
        // must not ask the reader to classify a value the AST already knows.
        for g in &gs {
            assert!(g.message.contains("mutable container"), "{}", g.message);
            assert!(
                g.message.contains("attribute of the class that owns the work"),
                "{}",
                g.message
            );
            assert!(!g.message.contains("parameter default"), "{}", g.message);
            assert!(!g.message.contains("If it is"), "{}", g.message);
            assert!(!g.message.contains("keep it here"), "{}", g.message);
        }
    }

    #[test]
    fn global_state_messages_name_the_value_kind() {
        // The message branches on the value the rule already parsed: a
        // mutable container gets the class-attribute action; a constant gets
        // the parameter-default/class-attribute action; a re-binding gets the
        // one-home/import action. No conditional menu.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/global_state_messages_name_the_value_kind__01.py"
        ));
        let gs: Vec<&Finding> = f.iter().filter(|x| x.kind == "global-state").collect();
        assert_eq!(gs.len(), 5, "{f:?}");
        let msg = |name: &str| {
            gs.iter()
                .find(|g| g.message.starts_with(name))
                .map(|g| g.message.as_str())
                .unwrap_or_else(|| panic!("no global-state message for {name}: {gs:?}"))
        };

        for name in ["STATE", "TABLE"] {
            let m = msg(name);
            assert!(m.contains("mutable container"), "{m}");
            assert!(m.contains("attribute of the class"), "{m}");
            assert!(!m.contains("parameter default"), "{m}");
        }
        for name in ["PAIR", "PROMPT"] {
            let m = msg(name);
            assert!(m.contains("constant"), "{m}");
            assert!(m.contains("parameter default"), "{m}");
            assert!(!m.contains("mutable container"), "{m}");
        }
        let alias = msg("ALIAS");
        assert!(alias.contains("bound to another value"), "{alias}");
        assert!(alias.contains("one home"), "{alias}");
        assert!(!alias.contains("parameter default"), "{alias}");

        for g in &gs {
            assert!(g.message.contains("at import time"), "{}", g.message);
            assert!(!g.message.contains("If it is"), "{}", g.message);
        }
    }

    #[test]
    fn several_module_constants_offer_one_class() {
        // #80: the per-value action is not the only reading when the module
        // holds a set of constants. The message names the set and offers the
        // one-class decision; the reader judges whether the constants are one
        // concept.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/several_module_constants_offer_one_class__01.py"
        ));
        let gs: Vec<&Finding> = f.iter().filter(|x| x.kind == "global-state").collect();
        assert_eq!(gs.len(), 3, "{f:?}");
        assert!(gs[0].message.starts_with("HOST"), "{}", gs[0].message);
        assert!(gs[0].message.contains("the module's 3 constants"), "{}", gs[0].message);
        assert!(gs[0].message.contains("the module's own class"), "{}", gs[0].message);
        assert!(gs[0].message.contains("parameter default"), "{}", gs[0].message);
        for g in &gs[1..] {
            // one module-level observation, not one per constant
            assert!(!g.message.contains("the module's"), "{}", g.message);
        }
    }

    #[test]
    fn two_module_constants_keep_the_single_value_action() {
        // "Several" is three or more: a pair does not carry the class nudge.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/several_module_constants_offer_one_class__02.py"
        ));
        let gs: Vec<&Finding> = f.iter().filter(|x| x.kind == "global-state").collect();
        assert_eq!(gs.len(), 2, "{f:?}");
        for g in &gs {
            assert!(!g.message.contains("the module's"), "{}", g.message);
            assert!(!g.message.contains("the module's own class"), "{}", g.message);
        }
    }

    #[test]
    fn module_constant_count_ignores_other_module_bindings() {
        // The count in the message is the number of constant findings, not
        // every module-level binding: the mutable container, re-binding,
        // call-valued service and dunder name do not belong to the set.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/several_module_constants_offer_one_class__03.py"
        ));
        let gs: Vec<&Finding> = f.iter().filter(|x| x.kind == "global-state").collect();
        assert_eq!(gs.len(), 5, "{f:?}");
        let constants: Vec<&Finding> = gs
            .iter()
            .filter(|g| g.message.contains("is a constant bound at import time"))
            .copied()
            .collect();
        assert_eq!(constants.len(), 3, "{gs:?}");
        let nudged: Vec<&Finding> = gs
            .iter()
            .filter(|g| g.message.contains("the module's 3 constants"))
            .copied()
            .collect();
        assert_eq!(nudged.len(), 1, "{gs:?}");
        assert!(nudged[0].message.starts_with("HOST"), "{}", nudged[0].message);
        for g in gs.iter().filter(|g| !g.message.contains("is a constant")) {
            assert!(!g.message.contains("the module's"), "{}", g.message);
        }
    }

    #[test]
    fn constant_table_is_a_module_level_variable() {
        // third-pass ruling 3: ANY module-level variable assignment fires —
        // a constant table is still a module-level variable; the negative
        // literals INSIDE it stay magic-number-exempt
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/negative_literals_stay_constant_tables__01.py"
        ));
        let gs: Vec<&Finding> = f.iter().filter(|x| x.kind == "global-state").collect();
        assert_eq!(gs.len(), 1, "{f:?}");
        assert_eq!(gs[0].line, 1);
    }

    #[test]
    fn mutation_of_flagged_literal_not_duplicated() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/mutation_of_flagged_literal_not_duplicated__01.py"
        ));
        let gs: Vec<&Finding> = f.iter().filter(|x| x.kind == "global-state").collect();
        assert_eq!(gs.len(), 1); // the literal, not the mutation
    }

    // ------------------------------------------------------------- builtin-shadow
    #[test]
    fn shadow_params_and_locals() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/shadow_params_and_locals__01.py"
        ));
        let sh: Vec<&Finding> = f.iter().filter(|x| x.kind == "builtin-shadow").collect();
        assert_eq!(sh.len(), 3);
    }

    #[test]
    fn shadow_bindings_include_loop_with_and_except_targets() {
        // a for target, a with-as name and an except-as name are bindings too
        let f = scan_src(include_str!(
            "../../tests/fixtures/code/sample_shadow_bindings_include_loop_with_and_except_targets__9fdbd91a.py"
        ));
        let sh: Vec<&Finding> = f.iter().filter(|x| x.kind == "builtin-shadow").collect();
        assert_eq!(sh.len(), 3, "{f:?}");
        assert!(sh.iter().all(|s| s.message.starts_with("variable '")), "{f:?}");
    }

    // ------------------------------------------------------------- signature hygiene
    #[test]
    fn long_param_list_is_found() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/long_param_list_is_found__01.py"
        ));
        let lp: Vec<&Finding> = f.iter().filter(|x| x.kind == "long-param-list").collect();
        assert_eq!(lp.len(), 1);
        assert!(lp[0].message.contains("7 parameters"));
        // A7: the message names the call-site cost and the grouping action
        assert!(
            lp[0].message.contains("the call site cannot see what each value means"),
            "{}",
            lp[0].message
        );
        assert!(
            lp[0]
                .message
                .contains("Group the parameters that belong together into one object"),
            "{}",
            lp[0].message
        );
    }
    #[test]
    fn long_param_list_counts_leading_self_out() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/long_param_list_counts_leading_self_out__01.py"
        ));
        let lp: Vec<&Finding> = f.iter().filter(|x| x.kind == "long-param-list").collect();
        assert_eq!(lp.len(), 1);
        assert!(lp[0].message.contains("6 parameters"));
    }

    #[test]
    fn short_param_list_and_five_plus_self_pass() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/short_param_list_and_five_plus_self_pass__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "long-param-list"));
    }
    #[test]
    fn trailing_marker_on_multiline_except_header_binds() {
        // Phase 3: a marker riding the except header's closing line (the
        // server.py:704 shape — `except (…):  # lucidlint: ignore …`) binds
        // and clears: the finding anchors at the header's colon line, where
        // a trailing marker sits — the single-line trailing-marker case
        // (marker on the `except` line) already bound; this is its
        // multi-line twin.
        let src = "try:\n    work()\nexcept (\n    ValueError,\n    TypeError,\n):  # lucidlint: ignore swallow boundary is logged upstream\n    log('blip')\n";
        let f = scan_src(src);
        assert!(!f.iter().any(|x| x.kind == "swallow"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{f:?}");

        // control: the same header without a marker still fires, anchored
        // at the header's colon line (where a trailing marker would sit)
        let src2 = "try:\n    work()\nexcept (\n    ValueError,\n    TypeError,\n):\n    log('blip')\n";
        let f2 = scan_src(src2);
        let s: Vec<&Finding> = f2.iter().filter(|x| x.kind == "swallow").collect();
        assert_eq!(s.len(), 1, "{f2:?}");
        assert_eq!(s[0].line, 6, "{f2:?}");
        assert!(!f2.iter().any(|x| x.kind == "stale-suppression"), "{f2:?}");
    }

    // ------------------------------------------------------------- except family
    #[test]
    fn bare_and_log_only_swallows() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/bare_and_log_only_swallows__01.py"
        ));
        assert_eq!(f.iter().filter(|x| x.kind == "swallow").count(), 2);
    }

    #[test]
    fn surfaced_return_is_not_a_swallow() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/surfaced_return_is_not_a_swallow__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn accumulator_surfacing_via_returned_name() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/accumulator_surfacing_via_returned_name__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn sys_exit_surfaces() {
        let f = scan_src(include_str!("../../tests/fixtures/rust/sys_exit_surfaces__01.py"));
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn swallow_message_names_the_antipattern_and_the_bar() {
        // H6: the message states the mechanism (a caller exists that needs
        // to decide) and the surface-by list, and carries NO suppression
        // pointer — the renderer owns that (writing-messages.md).
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/swallow_message_names_the_antipattern_and_the_bar__01.py"
        ));
        let s = f.iter().find(|x| x.kind == "swallow").expect("swallow fires");
        assert!(
            s.message.contains("a caller exists that needs to decide"),
            "message must name the invisible-audience problem: {}",
            s.message
        );
        assert!(
            s.message
                .contains("surface by return, raise, break, continue, sys.exit"),
            "{}",
            s.message
        );
        assert!(
            !s.message.contains("lucidlint: ignore"),
            "suppression pointers are renderer data: {}",
            s.message
        );
        assert!(
            !s.message.contains("no caller exists"),
            "the terminal-boundary escape is no longer in the message: {}",
            s.message
        );
    }

    #[test]
    fn real_tfl_build_cost_groups_counts_two() {
        // faithful slice of houses/tfl_client.py:583 — nested def + one
        // keyword-position lambda must count 2 (Python ast.walk parity)
        let src = include_str!("../../tests/fixtures/rust/real_tfl_build_cost_groups_counts_two__01.py");
        let parsed = ruff_python_parser::parse_module(src).unwrap();
        let body = parsed.syntax().body.clone();
        let inner = checks::FunctionFacts::inner_function_count(&body[0]);
        assert_eq!(inner, 2, "real fn inner count: {inner}");
    }

    #[test]
    fn keyword_lambdas_count_towards_closures() {
        // METHOD: cc gate = 0 (fn_map semantics); span >= 60 via a padding
        // comment block; two keyword-position lambdas → closure fires
        let pad = "    # padding\n".repeat(58);
        let f = scan_src(&format!(
            "class C:\n    def _build_cost_groups(self, data):\n        journeys = data.get(\"journeys\", [])\n        if not journeys:\n            return []\n        best = min(journeys, key=lambda j: j.get(\"duration\", 9999))\n        groups = sorted(journeys, key=lambda j: j.get(\"mode\", \"\"))\n{pad}        return best, groups\n"
        ));
        let closures: Vec<(usize, &str)> = f
            .iter()
            .filter(|x| x.kind == "closures")
            .map(|x| (x.line, x.message.as_str()))
            .collect();
        assert_eq!(closures.len(), 1, "expected one closure, got {closures:?}");
    }

    #[test]
    fn subscript_store_on_returned_name_surfaces() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/subscript_store_on_returned_name_surfaces__01.py"
        ));
        assert!(
            !f.iter().any(|x| x.kind == "swallow"),
            "expected no swallow, got {:?}",
            f.iter().map(|x| (x.kind.as_str(), x.line)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn swallow_mutated_name_inside_returned_dict_surfaces() {
        // H4: server.py:704 shape — the except mutates db and the RETURNED
        // dict literal carries it (`return {"status": ..., "db": db}`); the
        // value surfaces, so no swallow.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/swallow_return_dict_carries_mutated_db__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "swallow"), "{f:?}");
    }

    #[test]
    fn swallow_mutated_name_inside_returned_fstring_surfaces() {
        // H4: the mutated name inside a returned f-string field surfaces too.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/swallow_return_fstring_carries_mutated_db__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "swallow"), "{f:?}");
    }

    #[test]
    fn swallow_mutated_name_not_referenced_in_return_fires() {
        // H4 guard: mutating db with NO reference anywhere in the return
        // expression tree still swallows the error.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/swallow_mutated_name_not_in_return_fires__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "swallow"), "{f:?}");
    }

    #[test]
    fn broad_except_is_a_warn() {
        let f = scan_src(include_str!("../../tests/fixtures/rust/broad_except_is_a_warn__01.py"));
        let b: Vec<&Finding> = f.iter().filter(|x| x.kind == "broad-except").collect();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].severity, "warn");
    }

    // ------------------------------------------------------------- closures
    #[test]
    fn closures_two_inner_functions_with_cc() {
        // cc must reach the >= 15 gate: 14 ifs + 1 base
        let ifs: Vec<String> = (0..14).map(|i| format!("    if a{i}:\n        x = {i}\n")).collect();
        let src = format!(
            "def big(a):\n    def inner_a(x):\n        return x\n    def inner_b(x):\n        return x\n    x = 0\n{}\n    return x\n",
            ifs.concat()
        );
        let f = scan_src(&src);
        assert!(
            f.iter().any(|x| x.kind == "closures"),
            "expected closures, got {:?}",
            kinds(&f)
        );
    }

    #[test]
    fn closures_mutating_accumulator_fires_without_cc_or_span() {
        // the _render shape: two small walkers that both append to the
        // enclosing function's line buffer — shared-state mutation is the
        // class-in-disguise tell even with cc < 15 and span < 60
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/closures_mutating_accumulator_fires_without_cc_or_span__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "closures").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert!(r[0].message.contains("accumulator pattern"), "{}", r[0].message);
    }

    #[test]
    fn closures_pure_handler_factory_passes() {
        // a factory of handlers that only READ the captured config — the
        // legit closure idiom, not a class in disguise
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/closures_pure_handler_factory_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "closures"), "{f:?}");
    }

    #[test]
    fn closures_single_mutating_closure_passes() {
        // one closure over an accumulator is a closure, not a class
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/closures_single_mutating_closure_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "closures"), "{f:?}");
    }

    #[test]
    fn closures_attribute_chain_mutation_fires() {
        // the accumulator reached through an attribute of a captured name
        // (writer.lines.append) — the same latent class, a harder receiver
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/closures_attribute_chain_mutation_fires__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "closures").collect();
        assert_eq!(r.len(), 1, "{f:?}");
    }

    #[test]
    fn closures_collaborator_write_drops_the_accumulator_claim() {
        // Phase 7a: a mutating call on an attribute of a captured local
        // (`prop.payers.push`) still makes the closures finding fire — the
        // object is shared state — but the LOCAL is not being accumulated, so
        // the message states only that the closures close over the enclosing
        // locals and makes no accumulator claim.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/closures_collaborator_write_no_accumulator__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "closures").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert!(r[0].message.contains("closing over its state"), "{}", r[0].message);
        assert!(!r[0].message.contains("accumulator"), "{}", r[0].message);
        assert!(!r[0].message.contains("captured local"), "{}", r[0].message);
    }

    // ------------------------------------ misplaced-method / tuple-record / assembly
    #[test]
    fn misplaced_method_fires_when_class_state_is_passed() {
        // _superseded_lines(em, vm) called from a method holding em/vm as
        // instance state — the function is the class's method in exile
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/misplaced_method_fires_when_class_state_is_passed__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "misplaced-method").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert!(r[0].message.contains("R.render"), "{}", r[0].message);
    }

    #[test]
    fn misplaced_method_ignores_helper_called_with_plain_locals() {
        // a helper called with locals that are NOT class attributes — no
        // instance state in exile
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/misplaced_method_ignores_helper_called_with_plain_locals__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "misplaced-method"), "{f:?}");
    }

    #[test]
    fn misplaced_method_direct_form_reads_the_class_it_takes() {
        // a module function whose first parameter is the module's own class,
        // read only through its fields, is that class's method
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/misplaced_method_direct_form_reads_the_class_it_takes__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "misplaced-method").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert!(r[0].message.contains("School"), "{}", r[0].message);
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/misplaced_method_direct_form_negatives__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "misplaced-method"), "{f:?}");
    }

    #[test]
    fn tuple_record_fires_on_positional_reads() {
        // (props, name) tuples built into dicts, read by constant index and
        // destructure — an anonymous record
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/tuple_record_fires_on_positional_reads__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "tuple-record").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert!(r[0].message.contains("em"), "{}", r[0].message);
    }

    #[test]
    fn tuple_record_ignores_single_read() {
        // a dict of tuples read once is not a record-shaped pattern yet
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/tuple_record_ignores_single_read__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "tuple-record"), "{f:?}");
    }

    #[test]
    fn assembly_class_fires_on_threaded_build() {
        // __init__ threading em/vm through functions whose outputs feed each
        // other, storing every result — a class in waiting
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/assembly_class_fires_on_threaded_build__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "assembly-class").collect();
        assert_eq!(r.len(), 1, "{f:?}");
    }

    #[test]
    fn assembly_class_ignores_plain_pipeline() {
        // a linear a = f(x); b = g(a) chain has no shared base — the
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/assembly_class_ignores_plain_pipeline__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "assembly-class"), "{f:?}");
    }

    // ------------------------------------------------------------- class-module
    #[test]
    fn class_module_name_mismatch() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/class_module_name_mismatch__01.py"
        ));
        let cm: Vec<&Finding> = f.iter().filter(|x| x.kind == "class-module").collect();
        assert_eq!(cm.len(), 1, "{f:?}");
        // A7: the message names the class, the file-structure mechanism, and
        // the rename target (lowercase stem)
        assert!(
            cm[0].message.contains("holds one class named 'Config'"),
            "{}",
            cm[0].message
        );
        assert!(
            cm[0].message.contains("Rename the file to 'config.py'"),
            "{}",
            cm[0].message
        );
        assert!(cm[0].message.contains("closely related models"), "{}", cm[0].message);
    }

    // ------------------------------------------------------------- strewing
    #[test]
    fn strewing_three_functions_shared_param() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/strewing_three_functions_shared_param__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "strewing"));
    }

    // ------------------------------------------------------------- duplicate-def
    #[test]
    fn duplicate_module_scope_def_is_found() {
        // the review-log guess_pages shadow: two defs, same name, module scope
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/duplicate_module_scope_def_is_found__01.py"
        ));
        let d: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate-def").collect();
        assert_eq!(d.len(), 1, "{f:?}");
        assert_eq!(d[0].line, 4, "the SECOND binding is the finding");
    }

    #[test]
    fn tuple_record_counts_reads_inside_comprehensions() {
        // em[x][0] in a comprehension if, em[k][1] in a dictcomp value, and
        // a lambda sort key all count — the reads hidden inside expressions
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/tuple_record_counts_reads_inside_comprehensions__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "tuple-record").collect();
        assert_eq!(r.len(), 1, "{f:?}");
    }

    #[test]
    fn wide_tuple_fails_on_four_element_annotations() {
        // the smoosh: a long parameter list packed into one anonymous tuple
        // — the positions carry meaning the call site cannot see
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/wide_tuple_fails_on_four_element_annotations__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "wide-tuple").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert_eq!(r[0].severity, "fail", "{f:?}");
        assert!(r[0].message.contains("4-tuple"), "{}", r[0].message);
        assert!(r[0].message.contains("limits"), "{}", r[0].message);
    }

    #[test]
    fn wide_tuple_warns_on_three_elements() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/wide_tuple_warns_on_three_elements__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "wide-tuple").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert_eq!(r[0].severity, "warn", "{f:?}");
    }

    #[test]
    fn wide_tuple_exempts_pairs_variadic_and_nested_inner_tuples() {
        // 2-tuples are idiomatic pairs; tuple[X, ...] is a homogeneous
        // sequence, not a record; a 2-tuple nested in a 4-tuple flags the
        // OUTER arity only
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/wide_tuple_exempts_pairs_variadic_and_nested_inner_tuples__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "wide-tuple").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert_eq!(r[0].severity, "fail", "{f:?}");
    }

    #[test]
    fn wide_tuple_covers_returns_variables_and_methods() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/wide_tuple_covers_returns_variables_and_methods__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "wide-tuple").collect();
        assert_eq!(r.len(), 3, "{f:?}");
    }

    // ------------------------------------ data-clump / feature-envy / undeclared-attribute
    #[test]
    fn data_clump_fires_on_shared_param_pair() {
        // >=3 functions taking the same (em, vm) pair — a data clump
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/data_clump_fires_on_shared_param_pair__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "data-clump").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        // A3: the message names the pair, the two-parameter mechanism, and
        // the class action
        assert!(
            r[0].message.contains("pass the same parameter pair (em, vm) together"),
            "{}",
            r[0].message
        );
        assert!(
            r[0].message
                .contains("passing it as two parameters never says what that thing is"),
            "{}",
            r[0].message
        );
        assert!(
            r[0].message.contains("Make a class whose name states that thing"),
            "{}",
            r[0].message
        );
    }

    #[test]
    fn data_clump_ignores_two_function_pair() {
        // a pair shared by only two functions is not a clump
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/data_clump_ignores_two_function_pair__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "data-clump"), "{f:?}");
    }
    #[test]
    fn data_clump_pairs_aggregate_per_anchor() {
        // Two pairs sharing one anchor = ONE finding; the message names the
        // FIRST sorted pair and only ITS functions (each pair has its OWN
        // >= 3 functions — never claim one function set shares them all)
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/data_clump_pairs_aggregate_per_anchor__01.py"
        ));
        let clumps: Vec<&Finding> = f.iter().filter(|x| x.kind == "data-clump").collect();
        assert_eq!(clumps.len(), 1, "{f:?}");
        assert!(
            clumps[0]
                .message
                .contains("pass the same parameter pair (em, vm) together"),
            "{}",
            clumps[0].message
        );
        // the aggregated UNION (a, b, c, d) is not all on (em, vm) — the
        // pair's own functions (a, b, c) are named
        assert!(clumps[0].message.contains("(a, b, c)"), "{}", clumps[0].message);
        assert!(!clumps[0].message.contains("(em, x)"), "{}", clumps[0].message);
    }
    #[test]
    fn data_clump_names_the_pair_and_the_class_action() {
        // issue #22's matched-class fixture now carries the same uniform
        // A3 message — the fold-into-existing-class clause is gone
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/data_clump_suggests_existing_class__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "data-clump").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert!(
            r[0].message.contains("pass the same parameter pair"),
            "{}",
            r[0].message
        );
        assert!(
            r[0].message.contains("Make a class whose name states that thing"),
            "{}",
            r[0].message
        );
    }

    #[test]
    fn data_clump_splits_mismatched_param_types() {
        // Issue #57: shared names alone are not evidence. Wall, Point, and a
        // fixed tuple are three different pairs, so no group reaches the
        // three-function threshold.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/data_clump_splits_mismatched_param_types__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "data-clump"), "{f:?}");
    }

    #[test]
    fn data_clump_fires_on_agreeing_param_types() {
        // Annotations that agree keep the pair as one clump.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/data_clump_fires_on_agreeing_param_types__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "data-clump").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert!(r[0].message.contains("(combine, intersect, union)"), "{}", r[0].message);
    }

    #[test]
    fn data_clump_resolves_param_type_aliases() {
        // `WallAlias = Wall` is the same type; the alias must not split the
        // clump from the functions spelling `Wall` directly.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/data_clump_resolves_param_type_aliases__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "data-clump").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert!(r[0].message.contains("(combine, intersect, union)"), "{}", r[0].message);
    }

    #[test]
    fn data_clump_splits_typed_from_unannotated() {
        // Split policy: an absent annotation is its own key. Two typed
        // functions plus one unannotated one are not a group of three.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/data_clump_splits_typed_from_unannotated__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "data-clump"), "{f:?}");
    }

    #[test]
    fn data_clump_mixed_group_names_only_the_typed_functions() {
        // Three typed functions reach the threshold on their own; the
        // unannotated function stays out of the claim.
        let src = include_str!(
            "../../tests/fixtures/code/sample_data_clump_mixed_group_names_only_the_typed_functions__bf2f7b02.py"
        );
        let f = scan_src(src);
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "data-clump").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert!(r[0].message.contains("(a, b, c)"), "{}", r[0].message);
        assert!(!r[0].message.contains("a, b, c, d"), "{}", r[0].message);
    }

    #[test]
    fn per_buffer_scan_does_not_stale_repo_wide_markers() {
        // The repo-wide-only families never fire per-file, so their markers
        // cannot bind in a per-buffer scan — flagging them stale there was
        // a false fail artifact (the gate's repo-wide merge evaluates them).
        let src = include_str!("../../tests/fixtures/rust/per_buffer_scan_does_not_stale_repo_wide_markers__01.py");
        let per_buffer = Pipeline::scan(src, "prod_mod.py", false);
        assert!(
            !per_buffer.findings.iter().any(|f| f.kind == "stale-suppression"),
            "{:?}",
            per_buffer.findings
        );
        let gate = Pipeline::scan(src, "prod_mod.py", true);
        assert!(
            gate.findings.iter().any(|f| f.kind == "stale-suppression"),
            "{:?}",
            gate.findings
        );
    }

    #[test]
    fn feature_envy_fires_on_collaborator_reads() {
        // the render shape: graph = self.graph, then the method reads the
        // graph's fields more than its own state
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/feature_envy_fires_on_collaborator_reads__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "feature-envy").collect();
        assert_eq!(r.len(), 1, "{f:?}");
    }

    #[test]
    fn feature_envy_counts_reads_inside_tuple_assignments() {
        // `em, vm = graph.em, graph.vm` — the tuple's elements are field
        // reads too; a walker that stops at the Tuple would under-count to
        // 2 and the rule would miss the envy
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/feature_envy_counts_reads_inside_tuple_assignments__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "feature-envy").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert!(r[0].message.contains("4 times"), "{}", r[0].message);
    }

    #[test]
    fn feature_envy_ignores_own_inputs_and_values() {
        // a visitor callback consuming its parameter and a computed value
        // (pos = self.get_metadata(...)) are NOT envy
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/feature_envy_ignores_own_inputs_and_values__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "feature-envy"), "{f:?}");
    }

    #[test]
    fn undeclared_attribute_fires_on_quiet_assignment() {
        // self.done = False in a member function without a declaration, and
        // a plain __init__ assignment without a type
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/undeclared_attribute_fires_on_quiet_assignment__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "undeclared-attribute").collect();
        assert_eq!(r.len(), 2, "{f:?}");
    }

    #[test]
    fn undeclared_attribute_message_carries_fix_directive() {
        // the finding's message must name the automated fix (the engine's
        // directive parser matches it)
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/undeclared_attribute_message_carries_fix_directive__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "undeclared-attribute").collect();
        assert_eq!(r.len(), 1);
        assert!(r[0].message.contains("— fix: undeclared-attribute"), "{}", r[0].message);
    }

    #[test]
    fn undeclared_attribute_resolves_cross_file_base() {
        // the houses case: Node declares display_name in one file; a
        // subclass assigning it in another is NOT a quiet member — the old
        // per-file rule forced a redundant re-declaration on every subclass
        let f = scan_corpus(&[
            (
                "base.py",
                include_str!("../../tests/fixtures/rust/undeclared_attribute_resolves_cross_file_base__01.py"),
            ),
            (
                "sub.py",
                include_str!("../../tests/fixtures/rust/undeclared_attribute_resolves_cross_file_base__02.py"),
            ),
        ]);
        assert!(!f.iter().any(|x| x.kind == "undeclared-attribute"), "{f:?}");
    }

    #[test]
    fn undeclared_attribute_resolves_aliased_imported_base() {
        // `from base import Node as BaseNode` — the ancestor resolves by
        // its IMPORTED name, not the local alias (review-bot, PR #15)
        let f = scan_corpus(&[
            (
                "base.py",
                include_str!("../../tests/fixtures/rust/undeclared_attribute_resolves_aliased_imported_base__01.py"),
            ),
            (
                "sub.py",
                include_str!("../../tests/fixtures/rust/undeclared_attribute_resolves_aliased_imported_base__02.py"),
            ),
        ]);
        assert!(!f.iter().any(|x| x.kind == "undeclared-attribute"), "{f:?}");
    }

    #[test]
    fn undeclared_attribute_resolves_same_file_base() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/undeclared_attribute_resolves_same_file_base__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "undeclared-attribute"), "{f:?}");
    }

    #[test]
    fn undeclared_attribute_still_fires_when_no_base_declares() {
        // an undeclared member is still a finding, and a base-class cycle
        // must not hang the ancestor walk
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/undeclared_attribute_still_fires_when_no_base_declares__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "undeclared-attribute").collect();
        assert_eq!(r.len(), 1, "{f:?}");
    }

    #[test]
    fn future_import_never_changes_marker_binding() {
        // houses review-log quirk 8 suspected `from __future__ import
        // annotations` broke return-type marker binding ("identical
        // patterns bind without it"). The repro disproves it: with OR
        // without the import, binding is identical — kind + window decide.
        // The real confusion was the KIND TOKEN: the return-type record
        // family is `record-shape`; `tuple-record` is the positional-reads
        // family, and a `tuple-record` marker never matched it.
        let tuple_body = include_str!("../../tests/fixtures/rust/future_import_never_changes_marker_binding__01.py");
        let longparam_body =
            include_str!("../../tests/fixtures/rust/future_import_never_changes_marker_binding__02.py");
        let future = include_str!("../../tests/fixtures/rust/future_import_never_changes_marker_binding__03.py");
        let cases: Vec<(&str, String, bool)> = vec![
            // (label, source, marker must bind)
            (
                "record-shape marker",
                format!("# lucidlint: ignore record-shape the pair is the point\n{tuple_body}"),
                true,
            ),
            (
                "record-shape marker + future",
                format!("{future}# lucidlint: ignore record-shape the pair is the point\n{tuple_body}"),
                true,
            ),
            (
                "wrong-kind marker (tuple-record)",
                format!("# lucidlint: ignore tuple-record the pair is the point\n{tuple_body}"),
                false,
            ),
            (
                "wrong-kind marker + future",
                format!("{future}# lucidlint: ignore tuple-record the pair is the point\n{tuple_body}"),
                false,
            ),
            (
                "long-param-list marker",
                format!("# lucidlint: ignore long-param-list the framework signature\n{longparam_body}"),
                true,
            ),
            (
                "long-param-list marker + future",
                format!("{future}# lucidlint: ignore long-param-list the framework signature\n{longparam_body}"),
                true,
            ),
        ];
        for (label, src, must_bind) in &cases {
            let f = scan_corpus(&[("prod_mod.py", src)]);
            let target_kind = if label.starts_with("long-param") {
                "long-param-list"
            } else {
                "record-shape"
            };
            let bound = !f.iter().any(|x| x.kind == target_kind) && !f.iter().any(|x| x.kind == "stale-suppression");
            assert_eq!(bound, *must_bind, "{label}: {f:?}");
        }
    }

    #[test]
    fn undeclared_attribute_accepts_declared_members() {
        // annotated in __init__ or the class body -> declared
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/undeclared_attribute_accepts_declared_members__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "undeclared-attribute").collect();
        assert_eq!(r.len(), 1, "{f:?}"); // only self.done
    }

    #[test]
    fn a_large_cohesive_class_is_not_a_finding() {
        // Size alone is not a split order: this class has no field-disjoint
        // method groups for the partition rule to find, so there is nothing
        // to point the reader at. It used to be a `god-class` warn whose own
        // message said "split it ONLY where the partition rule finds
        // field-disjoint method groups" — an instruction with no antecedent
        // at exactly the sites that produced it, escapable only by a
        // suppression the census then chased (issue #47). The seam-bearing
        // case keeps its own finding: partition_field_disjoint_groups_is_found.
        let mut src = String::from("class Big:\n");
        for i in 0..20 {
            src.push_str(&format!("    def m{i}(self):\n        return {i}\n"));
        }
        let f = scan_src(&src);
        for kind in ["god-class", "latent-class", "partition"] {
            assert!(!f.iter().any(|x| x.kind == kind), "{kind} in {f:?}");
        }
    }

    #[test]
    fn duplicate_field_fires_across_containment() {
        // the request/options shape: the containing class and its contained
        // class both hold repo+rel — duplicated domain state
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/duplicate_field_fires_across_containment__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate-field").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert!(r[0].message.contains("repo, rel"), "{}", r[0].message);
    }

    #[test]
    fn duplicate_field_ignores_unrelated_shared_names() {
        // same field name on classes with NO containment edge — coincidence
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/duplicate_field_ignores_unrelated_shared_names__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "duplicate-field"), "{f:?}");
    }
    #[test]
    fn duplicate_class_and_function_name_is_found() {
        // def shadowing a class of the same name is the same hazard
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/duplicate_class_and_function_name_is_found__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "duplicate-def"));
    }

    #[test]
    fn class_module_ignores_tool_script_with_one_class() {
        // generate_tree.py shape: a script whose single class is a component
        // (module-level helpers + a build() entry) — not "a class file",
        // renaming the file after the class would be wrong
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/class_module_ignores_tool_script_with_one_class__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "class-module"), "{f:?}");
    }
    #[test]
    fn duplicate_def_import_shadow_is_found() {
        // a def whose name collides with an import (the def-in-imports edit
        // mistake class)
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/duplicate_def_import_shadow_is_found__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "duplicate-def"));
    }

    #[test]
    fn duplicate_def_suppressed_with_why() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/duplicate_def_suppressed_with_why__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "duplicate-def"), "{f:?}");
    }

    #[test]
    fn duplicate_def_overload_battery_exempt() {
        // the @overload idiom legally binds one name several times — the
        // stubs plus the implementation; "rename one" is unfollowable there
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/duplicate_def_overload_battery_exempt__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "duplicate-def"), "{f:?}");
    }

    #[test]
    fn duplicate_def_overload_aliased_decorator_exempt() {
        // the exemption resolves the BOUND name: `overload as ov` binds ov,
        // and @ov stubs + impl are the legal idiom (review finding)
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/duplicate_def_overload_aliased_decorator_exempt__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "duplicate-def"), "{f:?}");
    }

    #[test]
    fn duplicate_def_overload_impl_duplicate_still_flagged() {
        // stubs + impl are exempt — a FOURTH def of the same name after the
        // impl is a genuine duplicate and must still fire (review finding).
        // The exemption tracks the FIRST non-overload def (the impl): a
        // duplicate AFTER it is flagged, never the impl itself (the bot's
        // logic-inversion: last-def tracking flagged the impl and exempted
        // the duplicate).
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/duplicate_def_overload_impl_duplicate_still_flagged__01.py"
        ));
        let dup: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate-def").collect();
        assert_eq!(dup.len(), 1, "{f:?}");
        assert_eq!(
            dup[0].line, 10,
            "the genuine duplicate (line 10), not the impl (line 7): {f:?}"
        );
    }
    #[test]
    fn restating_docstring_is_found() {
        // the log's example: "the line's orientation must be consistent with
        // its box's aspect" beside code that says exactly that — every content
        // word is an identifier already in the body
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/restating_docstring_is_found__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "restating-docstring"), "{f:?}");
    }

    #[test]
    fn meaningful_docstring_passes() {
        // the docstring names what the body does not: the CONCEPT
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/meaningful_docstring_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "restating-docstring"), "{f:?}");
    }

    // ------------------------------------------------------------- duplicate-block
    #[test]
    fn duplicate_block_directive_only_for_exact_duplicates() {
        // renamed duplicates are still flagged (the transcription class)
        // but the fix requires libcst deep_equals — the directive must
        // attach only when the two blocks are token-identical, or agents
        // chase a fix that refuses to apply (review bot)
        let exact = scan_src(include_str!(
            "../../tests/fixtures/rust/duplicate_block_directive_only_for_exact_duplicates__01.py"
        ));
        let e: Vec<&Finding> = exact.iter().filter(|x| x.kind == "duplicate-block").collect();
        assert_eq!(e.len(), 1, "{exact:?}");
        // C1: the message names the mechanism (duplicated work) and the
        // paste-vs-twin decision; the [MECHANICAL] delete wording is gone
        assert!(
            e[0].message
                .contains("appears twice in this function — duplicated work."),
            "{}",
            e[0].message
        );
        assert!(
            e[0].message
                .contains("If one block is a paste copy of the other, delete it."),
            "{}",
            e[0].message
        );
        assert!(e[0].message.contains("extract the shared part"), "{}", e[0].message);
        assert!(!e[0].message.contains("delete the second copy"), "{}", e[0].message);
        assert!(e[0].message.contains("fix: duplicate-block"), "{}", e[0].message);
        let renamed = scan_src(include_str!(
            "../../tests/fixtures/rust/duplicate_block_directive_only_for_exact_duplicates__02.py"
        ));
        let r: Vec<&Finding> = renamed.iter().filter(|x| x.kind == "duplicate-block").collect();
        assert_eq!(r.len(), 1, "{renamed:?}");
        assert!(
            !r[0].message.contains("fix:"),
            "a renamed duplicate is not deep-equal — no directive: {}",
            r[0].message
        );
    }

    #[test]
    fn duplicate_block_never_pairs_across_functions() {
        // C1 gate: only blocks within the SAME function pair — two functions
        // that each contain the block once (flat 4 < 2*3, so no same-function
        // duplicate exists) must NOT produce a cross-function finding
        let src =
            include_str!("../../tests/fixtures/code/sample_duplicate_block_never_pairs_across_functions__a6f7b319.py");
        let f = scan_src(src);
        assert!(!f.iter().any(|x| x.kind == "duplicate-block"), "{f:?}");
        // and the same-function twins still fire (the rightmove_scraper case)
        let twins = scan_src(include_str!(
            "../../tests/fixtures/code/sample_duplicate_block_never_pairs_across_functions__ae2b6da2.py"
        ));
        assert!(twins.iter().any(|x| x.kind == "duplicate-block"), "{twins:?}");
    }

    #[test]
    fn adjacent_duplicate_block_is_found() {
        // the transcribe-twice class: a replaced loop header leaves the old
        // body in place — two identical transcribe->write->mark sequences
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/adjacent_duplicate_block_is_found__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "duplicate-block"), "{f:?}");
    }

    #[test]
    fn short_duplicate_pair_passes() {
        // two identical statements are common and often fine — 3+ is the
        // edit-mistake signature
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/short_duplicate_pair_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "duplicate-block"), "{f:?}");
    }

    // ------------------------------------------------- suppression scope
    #[test]
    fn suppression_scoped_to_its_line() {
        // an explained ignore on one except does not exempt a second except
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/suppression_scoped_to_its_line__01.py"
        ));
        let exc: Vec<&Finding> = f.iter().filter(|x| x.kind == "swallow").collect();
        assert_eq!(exc.len(), 1);
        assert_eq!(exc[0].line, 8); // the second handler (line 8) is not exempted
    }

    #[test]
    fn suppression_wrong_signal_does_not_exempt() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/suppression_wrong_signal_does_not_exempt__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "swallow")); // still a swallow; an explained mis-scoped ignore emits no suppression finding
    }

    #[test]
    fn ignore_file_without_why_is_a_finding() {
        let src = include_str!("../../tests/fixtures/rust/ignore_file_without_why_is_a_finding__01.py");
        let f = scan_src(src);
        assert!(f.iter().any(|x| x.kind == "swallow")); // not exempted
        assert!(f.iter().any(|x| x.kind == "suppression"));
    }

    #[test]
    fn type_ignore_in_docstring_is_not_a_finding() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/type_ignore_in_docstring_is_not_a_finding__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "type-ignore"));
    }

    // ------------------------------------------------- except edges
    #[test]
    fn except_with_raise_is_not_a_swallow() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/except_with_raise_is_not_a_swallow__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn except_returning_empty_dict_is_not_a_swallow() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/except_returning_empty_dict_is_not_a_swallow__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn except_return_none_is_not_a_swallow() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/except_return_none_is_not_a_swallow__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn except_continue_is_not_a_swallow() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/except_continue_is_not_a_swallow__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn empty_exception_catch_still_fails() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/empty_exception_catch_still_fails__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "swallow"));
    }

    #[test]
    fn accumulator_not_returned_still_swallows() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/accumulator_not_returned_still_swallows__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "swallow")); // issues not returned → swallow
    }

    // ------------------------------------------------- global-state edges
    #[test]
    fn constant_table_mutated_in_function_is_still_state() {
        // the all-constant literal fires AT THE ASSIGNMENT (the expanded
        // rule: any module-level variable), and the machine inside the
        // function is not double-reported — the mutation arm merged into the
        // assignment finding (one family, one message)
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/constant_table_mutated_in_function_is_still_state__01.py"
        ));
        let gs: Vec<&Finding> = f.iter().filter(|x| x.kind == "global-state").collect();
        assert_eq!(gs.len(), 1);
        assert_eq!(gs[0].line, 1);
    }

    // ------------------------------------------------- shadow pass
    #[test]
    fn no_builtin_shadow_in_clean_fn() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/no_builtin_shadow_in_clean_fn__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "shadow"));
    }

    // ------------------------------------------------- vague-name
    #[test]
    fn vague_name_thin_role_class_passes() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/vague_name_thin_role_class_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "vague-name"));
    }

    #[test]
    fn vague_name_load_bearing_class_is_found() {
        let mut src = String::from(include_str!(
            "../../tests/fixtures/code/sample_vague_name_load_bearing_class_is_found__cd1351ae.py"
        ));
        for i in 0..6 {
            src.push_str(&format!("    def method_{i}(self):\n        return {i}\n"));
        }
        let f = scan_src(&src);
        assert!(f.iter().any(|x| x.kind == "vague-name"));
    }

    // ------------------------------------------------- severity
    // ------------------------------------------- docs + abstraction
    #[test]
    fn docs_broken_link_is_found() {
        let dir = std::env::temp_dir().join(format!("docs_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(
            dir.join("docs/guide.md"),
            "see [missing](nope.md)
",
        )
        .unwrap();
        let f = docs::Docs::docs_findings(&dir, &std::collections::HashSet::new());
        assert!(f.iter().any(|x| x.kind == "docs-link" && x.message.contains("nope.md")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn abstraction_single_concrete_is_found() {
        let src_a = include_str!("../../tests/fixtures/rust/abstraction_single_concrete_is_found__01.py");
        let src_b = include_str!("../../tests/fixtures/rust/abstraction_single_concrete_is_found__02.py");
        let scan_a = Pipeline::scan_source(src_a, "a.py");
        let scan_b = Pipeline::scan_source(src_b, "b.py");
        let scans = vec![
            ("a.py".to_string(), scan_a.classes, scan_a.imports),
            ("b.py".to_string(), scan_b.classes, scan_b.imports),
        ];
        let f = checks::OverAbstraction::abstraction_findings(&scans);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].function, "Base");
    }

    #[test]
    fn abstraction_two_subclasses_passes() {
        let src_a = include_str!("../../tests/fixtures/rust/abstraction_two_subclasses_passes__01.py");
        let src_b = include_str!("../../tests/fixtures/rust/abstraction_two_subclasses_passes__02.py");
        let scan_a = Pipeline::scan_source(src_a, "a.py");
        let scan_b = Pipeline::scan_source(src_b, "b.py");
        let scans = vec![
            ("a.py".to_string(), scan_a.classes, scan_a.imports),
            ("b.py".to_string(), scan_b.classes, scan_b.imports),
        ];
        let f = checks::OverAbstraction::abstraction_findings(&scans);
        assert!(f.is_empty());
    }

    // ------------------------------------------------- class-module multi
    #[test]
    fn class_module_multi_misplaced_public_class_fails() {
        // User/Team in prod_mod.py: neither matches the stem — the multi arm
        // fires even though no single class "is" the module (was the old
        // multi-class PASS fixture; inverted per plan #31)
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/class_module_multi_misplaced_fails__01.py"
        ));
        assert!(
            f.iter()
                .any(|x| x.kind == "class-module" && x.message.contains("fix: split-module")),
            "{f:?}"
        );
        let hit = f.iter().find(|x| x.kind == "class-module").unwrap();
        assert!(
            hit.message.contains("User, Team"),
            "names the misplaced: {}",
            hit.message
        );
    }

    // ------------------------------------------- #27 husks, #28 process, #32 closures
    #[test]
    fn static_husk_fires_on_stateless_static_only_class() {
        let f = scan_src(include_str!("../../tests/fixtures/rust/static_husk_fires__01.py"));
        let hits: Vec<_> = f.iter().filter(|x| x.kind == "static-husk").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(hits[0].message.contains("no state of its own"), "{}", hits[0].message);
        assert!(hits[0].message.contains("put it IN the class"), "{}", hits[0].message);
        // P7: the remedy states the hidden half-step — the methods stop
        // being static and read the state from the instance.
        assert!(
            hits[0].message.contains(
                "The methods stop being static: give the class a constructor that takes the state, \
                 instance attributes that hold it, and instance methods that read it"
            ),
            "{}",
            hits[0].message
        );
    }
    #[test]
    fn static_husk_fires_when_classmethods_never_touch_cls() {
        // a classmethod that never reads `cls` holds no state, exactly as a
        // staticmethod does
        let f = scan_src(include_str!(
            "../../tests/fixtures/code/sample_static_husk_fires_when_classmethods_never_touch_cls__d8748f45.py"
        ));
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "static-husk").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
    }

    #[test]
    fn static_husk_exempts_a_classmethod_that_uses_cls() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/code/sample_static_husk_exempts_a_classmethod_that_uses_cls__7817ab9b.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "static-husk"), "{f:?}");
    }

    #[test]
    fn static_husk_exclusions_do_not_fire() {
        for (fx, src) in [
            (
                "static_husk_inherited_base_exempt__01.py",
                include_str!("../../tests/fixtures/rust/static_husk_inherited_base_exempt__01.py"),
            ),
            (
                "static_husk_abc_exempt__01.py",
                include_str!("../../tests/fixtures/rust/static_husk_abc_exempt__01.py"),
            ),
            (
                "static_husk_error_name_exempt__01.py",
                include_str!("../../tests/fixtures/rust/static_husk_error_name_exempt__01.py"),
            ),
        ] {
            let f = scan_src(src);
            assert!(!f.iter().any(|x| x.kind == "static-husk"), "{fx}: {f:?}");
        }
    }
    #[test]
    fn delegating_husk_fires_on_pure_forwarder_class() {
        let f = scan_src(include_str!("../../tests/fixtures/rust/delegating_husk_fires__01.py"));
        let hits: Vec<_> = f.iter().filter(|x| x.kind == "delegating-husk").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(hits[0].message.contains("fix: dissolve-husk"), "{}", hits[0].message);
    }
    #[test]
    fn delegating_husk_depth2_hands_off_to_forwarding_chain() {
        // Facade.fetch -> get_stored -> Store.load: the depth-2 chain is #30's
        // shape, NOT a husk — the forwarder target itself forwards
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/delegating_husk_depth2_hands_off__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "delegating-husk"), "{f:?}");
    }
    #[test]
    fn process_class_fires_with_the_three_part_test_message() {
        let f = scan_src(include_str!("../../tests/fixtures/rust/process_class_fires__01.py"));
        let hits: Vec<_> = f.iter().filter(|x| x.kind == "process-class").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        for part in [
            "is named for a process, not a thing",
            "Keep the name only when all three hold",
            "1) the domain calls this component by that name",
            "2) the data it operates on is all already represented by well-designed classes",
            "3) data and logic still remain that legitimately fit better in a process class",
            "represent it with classes first, then re-check 2 and 3",
        ] {
            assert!(hits[0].message.contains(part), "missing {part}: {}", hits[0].message);
        }
    }
    #[test]
    fn process_class_gof_names_pass() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/process_class_gof_exempt__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "process-class"), "{f:?}");
    }
    #[test]
    fn closure_cluster_fires_on_disjoint_local_closure_groups() {
        let f = scan_src(include_str!("../../tests/fixtures/rust/closure_cluster_fires__01.py"));
        let hits: Vec<_> = f.iter().filter(|x| x.kind == "closure-cluster").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(hits[0].message.contains("class-in-a-method"), "{}", hits[0].message);
        assert_eq!(hits[0].function, "main", "{:?}", hits[0]);
    }
    #[test]
    fn class_module_multi_each_public_class_matches_stem_passes() {
        // ProdMod in prod_mod.py matches the stem; _Helper is private — the
        // module's public identity is findable, no finding
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/class_module_multi_each_class_matches_stem_pass__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "class-module"), "{f:?}");
    }
    #[test]
    fn class_module_mixed_names_only_the_misplaced() {
        // User matches nothing, ProdMod matches prod_mod — the finding names
        // ONLY the misplaced class
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/class_module_mixed_names_only_misplaced__01.py"
        ));
        let hits: Vec<_> = f.iter().filter(|x| x.kind == "class-module").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(hits[0].message.contains("User"), "{}", hits[0].message);
        assert!(hits[0].message.contains("ProdMod"), "{}", hits[0].message);
    }

    #[test]
    fn class_module_exempts_a_protocol_the_named_class_inherits() {
        // issue #52: the interface exists because the cohabiting class does, so
        // it belongs beside it. The exemption reads INHERITANCE — the class the
        // file is named for must declare the Protocol as a base.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/class_module_protocol_beside_its_class_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "class-module"), "{f:?}");
        // the namesake class satisfies the Protocol structurally without
        // inheriting it: the interface exists because that class does, so it
        // belongs beside it (issue #52 — the DI seam is not always declared as
        // a base)
        let g = scan_src(include_str!(
            "../../tests/fixtures/rust/class_module_protocol_implemented_without_inheritance_passes__01.py"
        ));
        assert!(!g.iter().any(|x| x.kind == "class-module"), "{g:?}");
        // a Protocol the namesake class only PARTLY satisfies is not its
        // interface — it stays misplaced
        let u = scan_src(include_str!(
            "../../tests/fixtures/rust/class_module_protocol_unimplemented_member_fires__01.py"
        ));
        let hits: Vec<&Finding> = u.iter().filter(|x| x.kind == "class-module").collect();
        assert_eq!(hits.len(), 1, "{u:?}");
        assert!(hits[0].message.contains("ProdModLike"), "{}", hits[0].message);
        // inherited by another class in the module: not this class's interface
        let h = scan_src(include_str!(
            "../../tests/fixtures/rust/class_module_protocol_inherited_by_other_fails__01.py"
        ));
        let hits: Vec<&Finding> = h.iter().filter(|x| x.kind == "class-module").collect();
        assert_eq!(hits.len(), 1, "{h:?}");
        assert!(hits[0].message.contains("ProdModLike, Helper"), "{}", hits[0].message);
        // a public DATACLASS keeps its own module even when it also types the
        // seam: issue #52's own constraint. (@dataclass on a Protocol is not
        // valid typing — pyrefly/ty reject it — but the scanner reads AST, and
        // the constraint is stated, so the guard holds it.)
        let d = scan_src(include_str!(
            "../../tests/fixtures/rust/class_module_dataclass_protocol_beside_its_class_fails__01.py"
        ));
        let hits: Vec<&Finding> = d.iter().filter(|x| x.kind == "class-module").collect();
        assert_eq!(hits.len(), 1, "{d:?}");
        assert!(hits[0].message.contains("ProdModLike"), "{}", hits[0].message);
        // the Protocol spelled through its module (`te.Protocol`) is the same
        // base: the exemption reads the last name, never the spelling
        let p = scan_src(include_str!(
            "../../tests/fixtures/rust/class_module_protocol_through_its_module_passes__01.py"
        ));
        assert!(!p.iter().any(|x| x.kind == "class-module"), "{p:?}");
        // an ALIASED dataclass decorator (`from dataclasses import dataclass as
        // dc`) is still a dataclass: the ticket's constraint reads the class's
        // role, so the exemption refuses it under any of the module's names
        let q = scan_src(include_str!(
            "../../tests/fixtures/rust/class_module_dataclass_alias_beside_its_class_fails__01.py"
        ));
        let hits: Vec<&Finding> = q.iter().filter(|x| x.kind == "class-module").collect();
        assert_eq!(hits.len(), 1, "{q:?}");
        assert!(hits[0].message.contains("ProdModLike"), "{}", hits[0].message);
    }

    // ------------------------------------------------- record-shape
    #[test]
    fn record_grab_bag_and_collection_params_fail() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_grab_bag_and_collection_params_fail__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn record_map_and_domain_params_pass() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_map_and_domain_params_pass__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"));
    }

    #[test]
    fn record_return_collection_of_dicts_fails() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_return_collection_of_dicts_fails__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 1);
        assert!(
            r[0].message
                .contains("returns a dict that holds the fields of a record"),
            "{}",
            r[0].message
        );
    }

    #[test]
    fn record_variadic_tuple_and_fixed_tuple() {
        // tuple[str, ...] is a sequence; tuple[str, int] is a record pair
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_variadic_tuple_and_fixed_tuple__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 1);
        assert!(
            r[0].message
                .contains("pair is a fixed-size tuple; its positions are an unnamed record"),
            "{}",
            r[0].message
        );
        assert!(!r[0].message.contains("is a dict"), "{}", r[0].message);
    }

    #[test]
    fn record_fixed_tuple_message_names_the_tuple() {
        // Issue #57: a fixed tuple is a record, but the message must say so
        // instead of claiming the annotation is a dict. Both arms — parameter
        // and return — name the actual shape.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_fixed_tuple_message_names_the_tuple__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 2, "{f:?}");
        let param = r.iter().find(|x| x.line == 2).expect("parameter arm");
        assert!(
            param
                .message
                .contains("pair is a fixed-size tuple; its positions are an unnamed record"),
            "{}",
            param.message
        );
        assert!(!param.message.contains("is a dict"), "{}", param.message);
        let ret = r.iter().find(|x| x.line == 3).expect("return arm");
        assert!(
            ret.message
                .contains("stats returns a fixed-size tuple; its positions are an unnamed record"),
            "{}",
            ret.message
        );
        assert!(!ret.message.contains("returns a dict"), "{}", ret.message);
    }

    #[test]
    fn record_list_message_names_the_list() {
        // The list arm says what the annotation is, not "dict".
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_list_message_names_the_list__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert!(
            r[0].message.contains("items is a list whose items are unnamed records"),
            "{}",
            r[0].message
        );
        assert!(!r[0].message.contains("is a dict"), "{}", r[0].message);
    }

    #[test]
    fn record_deserializer_boundary_is_still_a_finding() {
        // the boundary doctrine (user ruling): a wire payload is NOT exempt —
        // raw JSON in gets a class with named fields and a serialisation
        // method at the edge
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_deserializer_boundary_is_still_a_finding__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert!(
            r[0].message.contains("Type raw with the class that models this shape"),
            "{}",
            r[0].message
        );
        assert!(!r[0].message.contains("to_dict"), "{}", r[0].message);
    }

    #[test]
    fn record_dict_literal_in_return_is_found() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_dict_literal_in_return_is_found__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 1);
        assert!(r[0].message.contains("This dict has constant keys"));
        assert_eq!(r[0].line, 2);
    }

    #[test]
    fn record_inline_call_args_flagged_lookup_tables_pass() {
        // uniform descent: a wire-format dict built at a call site IS a
        // record; an all-constant lookup table still passes
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_inline_call_args_flagged_lookup_tables_pass__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert!(r[0].message.contains("(Content-Type, X)"), "{f:?}");
    }

    #[test]
    fn record_spread_merge_is_not_a_record() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_spread_merge_is_not_a_record__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"));
    }

    #[test]
    fn record_dict_call_in_return_is_found() {
        // dict(a=1, b=x) is the literal's call-form twin — the bypass a
        // literal-only scan left open
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_dict_call_in_return_is_found__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert_eq!(r[0].line, 2);
    }

    #[test]
    fn record_dict_call_all_constant_passes() {
        // a lookup, not a record — same exemption as the literal form
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_dict_call_all_constant_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
    }
    #[test]
    fn record_scalar_map_literal_is_a_lookup_not_a_record() {
        // P1: the scalar-map sub-family — a dict whose EVERY value is a
        // scalar literal is a lookup keyed by identity, not a record: no
        // record-shape finding. A collection value keeps the finding (the
        // dict holds shape, not identities).
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_scalar_map_literal_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
        let g = scan_src(include_str!(
            "../../tests/fixtures/rust/record_collection_value_dict_still_fires__01.py"
        ));
        let r: Vec<&Finding> = g.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 1, "{g:?}");
        assert!(r[0].message.contains("This dict has constant keys"), "{}", r[0].message);
    }

    #[test]
    fn record_from_dict_functions_carry_the_wire_message() {
        // P1: the wire-parse sub-family — a dict arriving as a parameter or
        // return of a from_dict/from_json function is built at the parse
        // boundary, never ad hoc; the A1 premise ("call sites build it ad
        // hoc") is false there, so the message names the wire form, the
        // parse boundary, and the from_dict ingestion instead.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_from_dict_wire_message__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 2, "{f:?}");
        let param = r
            .iter()
            .find(|x| x.message.contains("raw is the wire form of a record"))
            .expect("param arm fires");
        assert!(
            param.message.contains("the parse boundary has no type"),
            "{}",
            param.message
        );
        assert!(
            param.message.contains("ingest the wire with its from_dict"),
            "{}",
            param.message
        );
        assert!(
            !param.message.contains("call sites build it ad hoc"),
            "{}",
            param.message
        );
        let ret = r
            .iter()
            .find(|x| x.message.contains("from_json returns the wire form of a record"))
            .expect("return arm fires");
        assert!(
            ret.message.contains("the parse boundary has no type"),
            "{}",
            ret.message
        );
        assert!(
            ret.message.contains("ingest the wire with its from_dict"),
            "{}",
            ret.message
        );
        assert!(!ret.message.contains("call sites build it ad hoc"), "{}", ret.message);
    }

    #[test]
    fn record_wire_functions_with_ingestion_class_emit_nothing() {
        // F1: a from_dict/from_json function whose module defines a class
        // that carries from_dict already IS the recommended shape — the wire
        // message's advice (give the shape a named record and ingest the
        // wire with its from_dict) describes a step already taken. No
        // record-shape finding at those parse boundaries.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_from_dict_with_ingestion_class_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
    }

    #[test]
    fn record_wire_functions_without_ingestion_class_keep_the_wire_message() {
        // F1: only wire-named functions whose module has NO from_dict class
        // keep the wire message — the parse boundary genuinely has no type.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_from_json_wire_message_fires__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert!(
            r[0].message.contains("from_json returns the wire form of a record"),
            "{}",
            r[0].message
        );
        assert!(
            r[0].message.contains("ingest the wire with its from_dict"),
            "{}",
            r[0].message
        );
    }
    #[test]
    fn record_export_edges_on_record_classes_emit_nothing() {
        // Issue #42, the export mirror of F1: a to_dict/to_json method whose
        // enclosing class DECLARES fields returns the class's own shape. The
        // class names it, so neither the `-> dict` return annotation nor the
        // literal rebuilding those fields has advice left to give.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_export_to_dict_class_fields_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
    }

    #[test]
    fn record_export_literal_with_uncovered_key_keeps_the_literal_finding() {
        // The literal exemption is keyed on the class's OWN fields: a
        // returned literal adding a key the class does not declare holds a
        // shape the class does not name — the literal arm still reports it.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_export_to_dict_uncovered_key_fires__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert!(
            r[0].message.contains("This dict has constant keys (rid, fresh, extra)"),
            "{}",
            r[0].message
        );
        assert_eq!(r[0].line, 10);
    }

    #[test]
    fn record_export_exemption_needs_both_the_name_and_the_class_fields() {
        // No export edge: a to_dict on a class that declares no field, a
        // non-export-named method on a field-bearing class, and a
        // module-level to_dict all keep both arms.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_export_non_edge_to_dict_keeps_findings__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 6, "{f:?}");
    }

    #[test]
    fn record_dict_call_as_inline_argument_is_found() {
        // uniform descent: dict(...) built as a call argument is a record
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_dict_call_as_inline_argument_is_found__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
    }

    #[test]
    fn record_dict_call_wrapping_literal_is_found() {
        // dict({"a": 1, "b": x}) — the inner literal is the record
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_dict_call_wrapping_literal_is_found__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(r.len(), 1, "{f:?}");
    }

    // ------------------------------------------- partition + test families
    #[test]
    fn partition_field_disjoint_groups_is_found() {
        // 6 methods, >= 150 lines: group A touches a/b, group B touches c/d
        let mut src = String::from(include_str!(
            "../../tests/fixtures/code/sample_partition_field_disjoint_groups_is_found__03231b8a.py"
        ));
        for i in 0..3 {
            src.push_str(&format!(
                "    def group_a_{i}(self):\n        x = self.a + self.b\n        for k in range(x):\n            x += k\n        return x\n"
            ));
        }
        for i in 0..3 {
            src.push_str(&format!(
                "    def group_b_{i}(self):\n        x = self.c + self.d\n        for k in range(x):\n            x += k\n        return x\n"
            ));
        }
        // span >= 150 code lines: a long docstring is a node (end_lineno
        // stops at the last node, comments don't count)
        src.push_str(include_str!(
            "../../tests/fixtures/code/sample_partition_field_disjoint_groups_is_found__33c971ab.py"
        ));
        for _ in 0..140 {
            src.push_str("padding line\n        ");
        }
        src.push_str("\"\"\"\n        return 1\n");
        let f = scan_src(&src);
        let p: Vec<&Finding> = f.iter().filter(|x| x.kind == "partition").collect();
        assert_eq!(p.len(), 1, "{f:?}");
        // A7: the missing action is appended — "Split it into N classes."
        assert!(p[0].message.contains("Split it into 2 classes."), "{}", p[0].message);
        assert!(p[0].message.contains("fix: extract-class"), "{}", p[0].message);
    }

    #[test]
    fn partition_small_class_passes() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/partition_small_class_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "partition"));
    }

    #[test]
    fn monkeypatch_setattr_and_patch_decorator_found() {
        let f = scan_src_test(include_str!(
            "../../tests/fixtures/rust/monkeypatch_setattr_and_patch_decorator_found__01.py"
        ));
        let m: Vec<&Finding> = f.iter().filter(|x| x.kind == "monkeypatch").collect();
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn skipif_on_environment_is_found() {
        let f = scan_src_test(include_str!(
            "../../tests/fixtures/rust/skipif_on_environment_is_found__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "skipif"));
    }

    #[test]
    fn bare_pytest_mark_skip_is_found() {
        let f = scan_src_test(include_str!(
            "../../tests/fixtures/rust/bare_pytest_mark_skip_is_found__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "skipif"));
    }

    #[test]
    fn pytest_mark_skip_with_parens_is_found_once() {
        let f = scan_src_test(include_str!(
            "../../tests/fixtures/rust/pytest_mark_skip_with_parens_is_found_once__01.py"
        ));
        let s: Vec<&Finding> = f.iter().filter(|x| x.kind == "skipif").collect();
        assert_eq!(s.len(), 1); // the Call node and its func Attribute node dedupe
    }

    #[test]
    fn other_markers_and_env_free_skipif_pass() {
        let f = scan_src_test(include_str!(
            "../../tests/fixtures/rust/other_markers_and_env_free_skipif_pass__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "skipif"));
    }

    #[test]
    fn fakefs_real_fs_without_pyfakefs_is_found() {
        let f = scan_src_test(include_str!(
            "../../tests/fixtures/rust/fakefs_real_fs_without_pyfakefs_is_found__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "fakefs"));
    }

    #[test]
    fn fakefs_sanctioned_subprocess_need_passes() {
        let f = scan_src_test(include_str!(
            "../../tests/fixtures/rust/fakefs_sanctioned_subprocess_need_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "fakefs"));
    }

    // ------------------------------------------------------------- no-assert-test
    #[test]
    fn test_with_assertion_passes() {
        let f = scan_src_test(include_str!(
            "../../tests/fixtures/rust/test_with_assertion_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "no-assert-test"));
    }

    #[test]
    fn test_with_pytest_raises_passes() {
        let f = scan_src_test(include_str!(
            "../../tests/fixtures/rust/test_with_pytest_raises_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "no-assert-test"));
    }

    #[test]
    fn test_with_fail_call_passes() {
        let f = scan_src_test(include_str!(
            "../../tests/fixtures/rust/test_with_fail_call_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "no-assert-test"));
    }

    #[test]
    fn test_with_assert_in_nested_function_passes() {
        let f = scan_src_test(include_str!(
            "../../tests/fixtures/rust/test_with_assert_in_nested_function_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "no-assert-test"));
    }

    #[test]
    fn test_without_assertion_is_found() {
        let f = scan_src_test(include_str!(
            "../../tests/fixtures/rust/test_without_assertion_is_found__01.py"
        ));
        let na: Vec<&Finding> = f.iter().filter(|x| x.kind == "no-assert-test").collect();
        assert_eq!(na.len(), 1);
        assert_eq!(na[0].function, "test_x");
        // A7: the message names why (proves nothing) and the action
        assert!(
            na[0]
                .message
                .contains("it can never fail, so it proves nothing. Add an assertion or delete the test."),
            "{}",
            na[0].message
        );
    }

    // ------------------------------------------------------------- test-asserts-source
    const TEST_ASSERTS_SOURCE_MESSAGE: &str = "test 'test_needle_is_code' asserts on code text, not on the behaviour under test — a text match pins one spelling, so it passes even when the code does nothing. Keep the test: assert what the code actually does instead. Do not delete the test or weaken it to match the text.";

    #[test]
    fn test_asserts_source_flags_code_text_assertions() {
        let f = scan_src_test(include_str!("../../tests/fixtures/asserts_source_bad.py"));
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "test-asserts-source").collect();
        assert_eq!(hits.len(), 5, "{f:?}");
        let functions: Vec<&str> = hits.iter().map(|hit| hit.function.as_str()).collect();
        assert_eq!(
            functions,
            vec![
                "test_needle_is_code",
                "test_output_text_is_code",
                "test_code_text_variable_prose_needle",
                "test_code_literal_comparison",
                "test_two_code_literals",
            ]
        );
        assert_eq!(hits[0].message, TEST_ASSERTS_SOURCE_MESSAGE);
    }

    #[test]
    fn test_asserts_source_allows_assertions_on_the_parsed_finding() {
        let f = scan_src_test(include_str!("../../tests/fixtures/asserts_source_ok.py"));
        assert!(!f.iter().any(|x| x.kind == "test-asserts-source"), "{f:?}");
    }

    #[test]
    fn test_asserts_source_flags_a_raw_read_of_a_source_path() {
        let f = scan_src_test(include_str!("../../tests/fixtures/asserts_source_read.py"));
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "test-asserts-source").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert_eq!(hits[0].function, "test_source_read_assertion");
    }

    #[test]
    fn test_asserts_source_flags_a_read_of_the_file_the_test_wrote() {
        // the blanket ban: the fixer's output is code text too
        let f = scan_src_test(include_str!("../../tests/fixtures/asserts_source_output_read.py"));
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "test-asserts-source").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
    }

    // ------------------------------------------------------------- code-in-literal
    #[test]
    fn code_in_literal_flags_a_code_block_and_a_scan_snippet() {
        let f = scan_src(include_str!("../../tests/fixtures/code_in_literal_bad.py"));
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "code-in-literal").collect();
        assert_eq!(hits.len(), 3, "{f:?}");
        assert!(hits
            .iter()
            .all(|h| h.message.contains("move it to a real file under tests/fixtures/")));
    }

    #[test]
    fn code_in_literal_excludes_docstrings_prose_and_regex() {
        let f = scan_src(include_str!("../../tests/fixtures/code_in_literal_ok.py"));
        assert!(!f.iter().any(|x| x.kind == "code-in-literal"), "{f:?}");
    }

    #[test]
    fn record_suppression_with_why_exempts() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_suppression_with_why_exempts__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"));
    }

    #[test]
    fn magic_number_is_a_warn_never_fail() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/magic_number_is_a_warn_never_fail__01.py"
        ));
        let magic: Vec<&Finding> = f.iter().filter(|x| x.kind == "magic-number").collect();
        assert_eq!(magic.len(), 1);
        assert_eq!(magic[0].severity, "warn");
        assert_eq!(magic[0].line, 2);
    }

    // ------------------------------------------------- duplicate (Dice)
    /// Corpus-level scan: names map to sources; returns merged findings — the
    /// production corpus over an in-memory scan set, plus the report's bulk
    /// warning and the gate's final suppression pass.
    fn scan_corpus(files: &[(&str, &str)]) -> Vec<Finding> {
        let scans: Vec<FileScan> = files
            .iter()
            .map(|(name, src)| Pipeline::scan_source(src, name))
            .collect();
        let mut all: Vec<Finding> = scans.iter().flat_map(|scan| scan.findings.clone()).collect();
        let supps_by_rel: std::collections::HashMap<String, crate::suppressions::Suppressions> = scans
            .iter()
            .map(|scan| (scan.file_name.clone(), scan.supps.clone()))
            .collect();
        let census = Pipeline::suppression_counts(supps_by_rel.values());
        all.extend(Pipeline::bulk_suppression_findings(
            &census,
            &supps_by_rel,
            BULK_SUPPRESSION_SITES,
        ));
        let corpus = RepoScan::of_names(&scans);
        corpus.reconcile(&mut all);
        corpus.apply_suppressions(all)
    }

    #[test]
    fn duplicate_near_identical_functions_are_found() {
        // same shape with renamed identifiers — Dice >= 0.9
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/duplicate_near_identical_functions_are_found__01.py"
        ));
        let d: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate").collect();
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn duplicate_different_shapes_are_not_found() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/duplicate_different_shapes_are_not_found__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "duplicate"));
    }

    #[test]
    fn test_with_unittest_assert_passes() {
        // unittest-style assertions count: self.assertEqual/assertTrue/...
        let ok = scan_src_test(include_str!(
            "../../tests/fixtures/rust/test_with_unittest_assert_passes__01.py"
        ));
        assert!(!ok.iter().any(|x| x.kind == "no-assert-test"));
        let ok2 = scan_src_test(include_str!(
            "../../tests/fixtures/rust/test_with_unittest_assert_passes__02.py"
        ));
        assert!(!ok2.iter().any(|x| x.kind == "no-assert-test"));
    }

    #[test]
    fn async_def_record_shape_is_found() {
        // async def parses as FunctionDef with is_async at the ruff pin —
        // the signature pass must still see it (review: async gap)
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/async_def_record_shape_is_found__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "record-shape"));
    }

    #[test]
    fn repo_wide_duplicate_suppressed_by_comment() {
        // a why'd `ignore duplicate` on the finding's line suppresses the
        // repo-wide duplicate AND is not itself reported stale
        let f = scan_corpus(&[
            (
                "one.py",
                include_str!("../../tests/fixtures/rust/repo_wide_duplicate_suppressed_by_comment__01.py"),
            ),
            (
                "two.py",
                include_str!("../../tests/fixtures/rust/repo_wide_duplicate_suppressed_by_comment__02.py"),
            ),
        ]);
        assert!(!f.iter().any(|x| x.kind == "duplicate"), "duplicate not suppressed");
        assert!(
            !f.iter().any(|x| x.kind == "stale-suppression"),
            "used suppression flagged stale"
        );
    }

    #[test]
    fn same_variable_name_different_types_are_distinct_families() {
        // two functions both dispatch a variable named x but over different
        // types — distinct element families, so no latent-visitor (the old
        // value-name key merged them into one family)
        let src =
            include_str!("../../tests/fixtures/rust/same_variable_name_different_types_are_distinct_families__01.py");
        let f = scan_src(src);
        assert!(!f.iter().any(|x| x.kind == "latent-visitor"));
    }

    #[test]
    fn why_less_marker_does_not_exempt_the_finding() {
        // R18: an exemption applies only with a why. A why-less marker is
        // reported as its own finding and does NOT suppress the signal it
        // names — the marker's message says exactly that, so honoring it
        // silently would contradict the report.
        let src =
            include_str!("../../tests/fixtures/code/sample_why_less_marker_does_not_exempt_the_finding__2e4269d6.py");
        let f = scan_src(src);
        assert!(f.iter().any(|x| x.kind == "suppression"), "{f:?}");
        assert!(f.iter().any(|x| x.kind == "swallow"), "{f:?}");
    }

    #[test]
    fn why_less_comma_suppression_reports_each_signal() {
        // `ignore sig1,sig2` without a why: BOTH signals need a reason
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/why_less_comma_suppression_reports_each_signal__01.py"
        ));
        let s: Vec<&Finding> = f.iter().filter(|x| x.kind == "suppression").collect();
        assert_eq!(s.len(), 2, "each signal's missing why must be reported");
    }

    #[test]
    fn duplicate_dice_contract_no_set_hash_shortcut() {
        // The Dice contract: >= 0.9 by MULTISET bigrams. Equal unique bigram
        // sets with different multiplicities hash equal under a set hash but
        // score Dice < 0.9 — the set-hash shortcut reported these as 100%
        // similar (2026-08-17 review-log). There is no hash shortcut; the
        // pair must NOT be flagged.
        use crate::skeleton::Similarity;
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // [a,b,a,b] has multiset bigrams (a,b),(b,a),(a,b) — the (a,b) repeats
        let rep = s(&["a", "b", "a", "b"]);
        // [a,b,a] has (a,b),(b,a) — same unique set, one fewer (a,b)
        let uniq = s(&["a", "b", "a"]);
        let sim = Similarity::dice_similarity(&rep, &uniq);
        // 2*2 / (3+2) = 0.8 < 0.9 — NOT a duplicate despite equal unique sets
        assert!(sim < 0.9, "dice must count bigram multiplicities: {sim}");
        // sanity: the exact multiset is 1.0
        assert_eq!(Similarity::dice_similarity(&rep, &s(&["a", "b", "a", "b"])), 1.0);
    }

    #[test]
    fn duplicate_skips_init_and_accessors() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/duplicate_skips_init_and_accessors__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "duplicate"));
    }

    #[test]
    fn duplicate_bfs_order_flags_the_later_class_method() {
        // module-level fn comes BEFORE class methods in ast.walk BFS — the
        // finding lands on the class method even though it has the lower line
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/duplicate_bfs_order_flags_the_later_class_method__01.py"
        ));
        let d: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate").collect();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].function, "cell_rect");
        assert_eq!(d[0].line, 9);
    }

    // ------------------------------------------------- forwarding-chain (#30)
    #[test]
    fn forwarding_chain_coupled_chain_fires() {
        // Facade.fetch -> get_stored -> Store.load all in one module: C's
        // module already depends on D's module (same file) — the dead
        // indirection is flagged
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/delegating_husk_depth2_hands_off__01.py"
        ));
        let hits: Vec<_> = f.iter().filter(|x| x.kind == "forwarding-chain").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(hits[0].message.contains("fix: collapse-chain"), "{}", hits[0].message);
        assert_eq!(hits[0].function, "fetch", "{:?}", hits[0]);
    }
    #[test]
    fn forwarding_chain_boundary_between_independent_modules_is_silent() {
        // Server in server.py never imports store.py — the chain is the ONLY
        // coupling between the modules: a deliberate boundary, NO finding and
        // NO message (severity principle, 2026-09-30)
        let f = scan_corpus(&[
            (
                "server.py",
                include_str!("../../tests/fixtures/rust/forwarding_chain_boundary_silent__01.py"),
            ),
            (
                "store.py",
                include_str!("../../tests/fixtures/rust/forwarding_chain_boundary_silent__02.py"),
            ),
        ]);
        assert!(!f.iter().any(|x| x.kind == "forwarding-chain"), "{f:?}");
    }

    #[test]
    fn dice_partial_similarity_below_threshold() {
        let a: Vec<String> = "A B C D E".split(' ').map(str::to_string).collect();
        let b: Vec<String> = "A B X Y Z".split(' ').map(str::to_string).collect();
        assert!(crate::skeleton::Similarity::dice_similarity(&a, &b) < 0.9);
        let same: Vec<String> = "A B C D E".split(' ').map(str::to_string).collect();
        assert_eq!(crate::skeleton::Similarity::dice_similarity(&a, &same), 1.0);
    }

    #[test]
    fn global_state_exempts_type_aliases_but_keeps_constants() {
        // a union/subscript alias is a type, not a value a caller varies
        // (album round: ToolArgs, ToolHit, RowRuns were all reported)
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/global_state_type_aliases_pass__01.py"
        ));
        let gs: Vec<&Finding> = f.iter().filter(|x| x.kind == "global-state").collect();
        assert_eq!(gs.len(), 1, "{gs:?}");
        assert!(gs[0].message.contains("MAX_ROWS"), "{}", gs[0].message);
    }

    #[test]
    fn sibling_value_class_stays_silent_on_a_shared_domain_base() {
        // two operators of one family: the shared half belongs on the base
        // they already have, not in a merge of the variants
        let src = include_str!("../../tests/fixtures/rust/sibling_value_class_shared_base_passes__01.py");
        let parsed = parse_module(src).expect("fixture parses");
        let body = parsed.syntax().body.clone();
        let classes = checks::SiblingValueClass::collect_sibling_classes(&body, src);
        let f = checks::SiblingValueClass::sibling_value_class_findings(&[("pkg/expr.py".to_string(), classes)]);
        assert!(!f.iter().any(|x| x.kind == "sibling-value-class"), "{f:?}");
    }

    #[test]
    fn duplicate_skips_a_nested_function_and_its_parent() {
        // a closure always resembles the function that defines it — the
        // similarity is containment, not copy-paste (houses: _fetch/api_fetch)
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/duplicate_nested_in_parent_passes__01.py"
        ));
        // the independent twin beside it still fires, so the skip is a
        // containment test and not a blanket silence
        let dups: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate").collect();
        assert!(
            dups.iter()
                .all(|d| !(d.message.contains("api_fetch") && d.message.contains("'_fetch'"))),
            "{dups:?}"
        );
        assert!(dups.iter().any(|d| d.message.contains("other_fetch")), "{dups:?}");
    }

    // ------------------------------------------------- duplicate-module (#26)
    #[test]
    fn sibling_value_class_fires_on_shared_fields_and_accessors() {
        let src = include_str!("../../tests/fixtures/rust/sibling_value_class_shared_accessors_fires__01.py");
        let parsed = parse_module(src).expect("fixture parses");
        let body = parsed.syntax().body.clone();
        let classes = checks::SiblingValueClass::collect_sibling_classes(&body, src);
        let f = checks::SiblingValueClass::sibling_value_class_findings(&[("pkg/values.py".to_string(), classes)]);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].kind, "sibling-value-class");
        assert_eq!(f[0].severity, "warn");
        assert!(f[0].message.contains("BatchPaths"), "{}", f[0].message);
        assert!(f[0].message.contains("BatchReadings"), "{}", f[0].message);
        assert!(f[0].message.contains("work_dir"), "{}", f[0].message);
        assert!(f[0].message.contains("guess_dir"), "{}", f[0].message);
    }

    #[test]
    fn sibling_value_class_stays_silent_on_containment() {
        // the second class holds the first and names it in a signature — the
        // containment case duplicate-field owns, not a sibling
        let src = include_str!("../../tests/fixtures/rust/sibling_value_class_containment_passes__01.py");
        let parsed = parse_module(src).expect("fixture parses");
        let body = parsed.syntax().body.clone();
        let classes = checks::SiblingValueClass::collect_sibling_classes(&body, src);
        let f = checks::SiblingValueClass::sibling_value_class_findings(&[("pkg/values.py".to_string(), classes)]);
        assert!(!f.iter().any(|x| x.kind == "sibling-value-class"), "{f:?}");
    }

    #[test]
    fn sibling_value_class_reads_a_forward_reference_as_containment() {
        // `paths: "BatchPaths"` is the same containment as the unquoted form —
        // a missed quoted annotation would report a composed pair as siblings
        let src =
            include_str!("../../tests/fixtures/rust/sibling_value_class_forward_reference_containment_passes__01.py");
        let parsed = parse_module(src).expect("fixture parses");
        let body = parsed.syntax().body.clone();
        let classes = checks::SiblingValueClass::collect_sibling_classes(&body, src);
        let f = checks::SiblingValueClass::sibling_value_class_findings(&[("pkg/values.py".to_string(), classes)]);
        assert!(!f.iter().any(|x| x.kind == "sibling-value-class"), "{f:?}");
    }

    #[test]
    fn sibling_value_class_spans_files_in_one_package() {
        // one class per file is the house convention — the rule must read the
        // package, not the file
        let a = include_str!(
            "../../tests/fixtures/code/sample_sibling_value_class_spans_files_in_one_package__9078729a.py"
        );
        let b = a.replace("BatchPaths", "BatchReadings");
        let mut classes = Vec::new();
        for (rel, src) in [("pkg/a.py", a), ("pkg/b.py", b.as_str())] {
            let parsed = parse_module(src).expect("source parses");
            let body = parsed.syntax().body.clone();
            classes.push((
                rel.to_string(),
                checks::SiblingValueClass::collect_sibling_classes(&body, src),
            ));
        }
        let f = checks::SiblingValueClass::sibling_value_class_findings(&classes);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].file, "pkg/b.py");
    }

    #[test]
    fn flat_folder_fires_at_twenty_and_names_the_split() {
        // a directory is a filing system, not a bag: 20+ browsable files in
        // one folder (modules + docs) has no structure to browse
        let modules: Vec<String> = (0..18).map(|i| format!("pkg/m{i}.py")).collect();
        let docs: Vec<String> = (0..2).map(|i| format!("pkg/d{i}.md")).collect();
        let f = checks::FlatFolder::findings(&modules, &docs);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].kind, "flat-folder");
        assert_eq!(f[0].severity, "warn");
        assert_eq!(f[0].file, "pkg");
        assert!(f[0].message.contains("18 modules"), "{}", f[0].message);
        assert!(f[0].message.contains("2 docs"), "{}", f[0].message);
    }

    #[test]
    fn flat_folder_stays_silent_below_twenty() {
        let modules: Vec<String> = (0..19).map(|i| format!("pkg/m{i}.py")).collect();
        assert!(checks::FlatFolder::findings(&modules, &[]).is_empty());
    }

    #[test]
    fn flat_folder_counts_docs_and_skips_init() {
        let docs: Vec<String> = (0..21).map(|i| format!("docs/d{i}.md")).collect();
        let f = checks::FlatFolder::findings(&[], &docs);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].file, "docs");
        // a docs pile has no modules to group: the action must match the shape
        assert!(!f[0].message.contains("Group the modules"), "{}", f[0].message);
        assert!(f[0].message.contains("coherent topic subsets"), "{}", f[0].message);
        assert!(f[0].message.contains("at 20+ files"), "{}", f[0].message);
        assert!(f[0].message.contains("not a subset"), "{}", f[0].message);
        assert!(!f[0].message.contains("leave the folder"), "{}", f[0].message);
        // `__init__.py` is not a browsable module — 19 real modules + one
        // init is still a clean folder
        let mut modules: Vec<String> = (0..19).map(|i| format!("pkg/m{i}.py")).collect();
        modules.push("pkg/__init__.py".to_string());
        assert!(checks::FlatFolder::findings(&modules, &[]).is_empty());
    }

    #[test]
    fn flat_folder_names_the_subsets_the_filenames_offer() {
        // the action's evidence: a folder's own names already carry the domain
        // nouns a split should use — and a family token shared by most files
        // (`node` here) is the folder's identity, not a subset
        let mut modules: Vec<String> = Vec::new();
        for i in 0..6 {
            modules.push(format!("pkg/commute_{i}.py"));
        }
        for i in 0..4 {
            modules.push(format!("pkg/school_{i}.py"));
        }
        for i in 0..15 {
            modules.push(format!("pkg/thing_node_{i}.py"));
        }
        let f = checks::FlatFolder::findings(&modules, &[]);
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].message.contains("coherent domain subsets"), "{}", f[0].message);
        assert!(f[0].message.contains("commute (6)"), "{}", f[0].message);
        assert!(f[0].message.contains("school (4)"), "{}", f[0].message);
        // the family token is not offered as a subset
        assert!(!f[0].message.contains("node (15)"), "{}", f[0].message);
    }

    #[test]
    fn flat_folder_family_noun_survives_a_singular_folder_name() {
        // `bus/` ends in `s` and is not a plural: the family is `bus`, and the
        // candidate list must not offer it (the naive singularisation read
        // `bu`)
        let modules: Vec<String> = (0..6)
            .map(|i| format!("bus/bus_stop_{i}.py"))
            .chain((0..5).map(|i| format!("bus/bus_route_{i}.py")))
            .chain((0..9).map(|i| format!("bus/thing_{i}.py")))
            .collect();
        let f = checks::FlatFolder::findings(&modules, &[]);
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(!f[0].message.contains("bus ("), "{}", f[0].message);
        assert!(f[0].message.contains("stop (6)"), "{}", f[0].message);
    }

    #[test]
    fn flat_folder_reads_camel_case_filenames() {
        // `CommuteStop` carries two nouns; a split on `_` alone would see one
        let modules: Vec<String> = (0..6)
            .map(|i| format!("pkg/CommuteStop{i}.py"))
            .chain((0..4).map(|i| format!("pkg/SchoolGate{i}.py")))
            .chain((0..10).map(|i| format!("pkg/ThingNode{i}.py")))
            .collect();
        let f = checks::FlatFolder::findings(&modules, &[]);
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].message.contains("commute (6)"), "{}", f[0].message);
        assert!(f[0].message.contains("school (4)"), "{}", f[0].message);
    }

    #[test]
    fn flat_folder_never_offers_the_family_noun_as_a_subset() {
        // `nodes/` is the family; `node` is its name, not a subset of it
        let modules: Vec<String> = (0..12)
            .map(|i| format!("houses/nodes/thing_node_{i}.py"))
            .chain((0..8).map(|i| format!("houses/nodes/commute_{i}.py")))
            .collect();
        let f = checks::FlatFolder::findings(&modules, &[]);
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].message.contains("commute (8)"), "{}", f[0].message);
        assert!(!f[0].message.contains("node ("), "{}", f[0].message);
    }

    #[test]
    fn flat_folder_never_counts_a_package_marker() {
        // a root-level __init__.py and a package one are both markers, not
        // modules the reader opens
        let mut modules: Vec<String> = (0..19).map(|i| format!("pkg/m{i}.py")).collect();
        modules.push("__init__.py".to_string());
        assert!(checks::FlatFolder::findings(&modules, &[]).is_empty());
    }

    #[test]
    fn flat_folder_keeps_directories_apart() {
        // 19 modules and 19 docs in the SAME folder is 38 files — one finding,
        // not two, and never summed across sibling folders
        let modules: Vec<String> = (0..19).map(|i| format!("pkg/m{i}.py")).collect();
        let docs: Vec<String> = (0..19).map(|i| format!("pkg/d{i}.md")).collect();
        let f = checks::FlatFolder::findings(&modules, &docs);
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].message.contains("19 modules"), "{}", f[0].message);
        assert!(f[0].message.contains("19 docs"), "{}", f[0].message);
    }

    #[test]
    fn duplicate_module_fork_pair_fires_with_matched_members() {
        let f = scan_corpus(&[
            (
                "config_a.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_fork_pair__01.py"),
            ),
            (
                "config_b.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_fork_pair__02.py"),
            ),
        ]);
        let d: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate-module").collect();
        assert_eq!(d.len(), 1, "{f:?}");
        assert!(d[0].message.contains("identical constants"), "{}", d[0].message);
        assert!(
            d[0].message.contains("matched members: close, connect"),
            "{}",
            d[0].message
        );
        assert!(d[0].message.contains("structural match"), "{}", d[0].message);
        assert!(d[0].message.contains("generated from the other"), "{}", d[0].message);
        assert!(d[0].message.contains("this is not a fork"), "{}", d[0].message);
        assert!(!d[0].message.contains("— a fork:"), "{}", d[0].message);
    }

    #[test]
    fn duplicate_module_base_class_pair_gets_the_subclass_message() {
        // C2: two classes inheriting the SAME base with identical shape get
        // the subclass message — the fork prose is false for base-class pairs
        let f = scan_corpus(&[
            (
                "config_a.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_base_class_pair__01.py"),
            ),
            (
                "config_b.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_base_class_pair__02.py"),
            ),
        ]);
        let d: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate-module").collect();
        assert_eq!(d.len(), 1, "{f:?}");
        assert!(
            d[0].message
                .contains("are two subclasses of one base with identical shape"),
            "{}",
            d[0].message
        );
        assert!(
            d[0].message
                .contains("move the constants into the base and delete the duplicate subclasses"),
            "{}",
            d[0].message
        );
        assert!(
            d[0].message
                .contains("If they were meant to differ, one is missing its override."),
            "{}",
            d[0].message
        );
        // the fork prose must NOT appear for the base-class pair
        assert!(!d[0].message.contains("a fix lands in one copy"), "{}", d[0].message);
    }
    #[test]
    fn duplicate_module_three_statement_floor_never_fires() {
        // the small file is a fragment of the fork (same constants) but its
        // <3 statements keep it out of the pairing entirely
        let f = scan_corpus(&[
            (
                "config_a.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_fork_pair__01.py"),
            ),
            (
                "config_b.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_fork_pair__02.py"),
            ),
            (
                "frag.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_small_file_never_fires__01.py"),
            ),
        ]);
        let d: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate-module").collect();
        assert_eq!(d.len(), 1, "{f:?}");
        assert!(!d[0].file.ends_with("frag.py"), "{}", d[0].file);
    }
    #[test]
    fn duplicate_module_ignore_file_silences() {
        let f = scan_corpus(&[
            (
                "config_a.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_fork_pair__01.py"),
            ),
            (
                "config_b.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_ignored_file__02.py"),
            ),
        ]);
        assert!(!f.iter().any(|x| x.kind == "duplicate-module"), "{f:?}");
    }

    #[test]
    fn duplicate_module_fires_without_constants_and_claims_none() {
        // Phase 6: the constants leave the gate — two near-identical modules
        // with NO module-level constants are still a structural match, and the message
        // makes no constants claim when there are none.
        let f = scan_corpus(&[
            (
                "dag_error.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_http_error_pair__01.py"),
            ),
            (
                "houses_error.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_http_error_pair__02.py"),
            ),
        ]);
        let d: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate-module").collect();
        assert_eq!(d.len(), 1, "{f:?}");
        assert!(!d[0].message.contains("identical constants"), "{}", d[0].message);
        assert!(d[0].message.contains("structural match"), "{}", d[0].message);
    }

    #[test]
    fn duplicate_module_record_siblings_with_different_fields_stay_silent() {
        // #73: two one-record modules are 100% similar by structure because a
        // field NAME is an "N" token — the skeleton cannot tell `name: str`
        // from `address: str`. A record's declared fields ARE its facts:
        // different fields mean two different records, with no shared
        // behaviour to fork.
        let f = scan_corpus(&[
            (
                "houses/nodes/commute_card_person.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_record_siblings_differing_fields__01.py"),
            ),
            (
                "houses/nodes/commute_card_destination.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_record_siblings_differing_fields__02.py"),
            ),
        ]);
        assert!(!f.iter().any(|x| x.kind == "duplicate-module"), "{f:?}");
    }

    #[test]
    fn duplicate_module_record_siblings_with_different_init_fields_stay_silent() {
        // the same rule for a record written as a plain class: the fields are
        // its `self.<name> = ...` assignments, so they must be compared too
        let f = scan_corpus(&[
            (
                "houses/nodes/center.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_record_siblings_differing_init_fields__01.py"),
            ),
            (
                "houses/nodes/radius.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_record_siblings_differing_init_fields__02.py"),
            ),
        ]);
        assert!(!f.iter().any(|x| x.kind == "duplicate-module"), "{f:?}");
    }

    #[test]
    fn duplicate_module_identical_records_in_two_modules_still_fire() {
        // the inverse: the same record written twice — same field, same
        // shape — is still a structural match, and the message says so
        let f = scan_corpus(&[
            (
                "dag/person.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_identical_records_fire__01.py"),
            ),
            (
                "houses/person.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_identical_records_fire__02.py"),
            ),
        ]);
        let d: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate-module").collect();
        assert_eq!(d.len(), 1, "{f:?}");
        assert!(d[0].message.contains("structural match"), "{}", d[0].message);
    }

    #[test]
    fn duplicate_module_below_the_similarity_bar_stays_silent() {
        // Phase 6: Dice >= 0.9 alone decides — a pair of similar-length
        // classes with different structure stays silent.
        let f = scan_corpus(&[
            (
                "alpha.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_below_threshold__01.py"),
            ),
            (
                "beta.py",
                include_str!("../../tests/fixtures/rust/duplicate_module_below_threshold__02.py"),
            ),
        ]);
        assert!(!f.iter().any(|x| x.kind == "duplicate-module"), "{f:?}");
    }

    // ------------------------------------------------- unused
    #[test]
    fn message_wording_states_the_real_fix_and_the_exemption() {
        // the wording must steer agents away from non-fixes (value-shaped
        // names) and bulk suppression: each message names what a real fix
        // is AND the only legitimate suppression
        let b = scan_src(include_str!(
            "../../tests/fixtures/rust/message_wording_states_the_real_fix_and_the_exemption__03.py"
        ));
        let be = b.iter().find(|x| x.kind == "broad-except").expect("broad-except fires");
        assert!(be.message.contains("blast radius"), "{}", be.message);
        assert!(be.message.contains("catch what you actually handle"), "{}", be.message);

        let scan_magic = scan_src_test(include_str!(
            "../../tests/fixtures/rust/message_wording_states_the_real_fix_and_the_exemption__04.py"
        ));
        let mn = scan_magic
            .iter()
            .find(|x| x.kind == "magic-number")
            .expect("magic-number fires");
        assert!(
            mn.message
                .contains("magic number 60 in f — nothing states what it means or why this magnitude"),
            "{}",
            mn.message
        );
        assert!(
            mn.message
                .contains("Write it as a named constant where the computation uses it"),
            "{}",
            mn.message
        );
        assert!(mn.message.contains("why this size"), "{}", mn.message);
        assert!(mn.message.contains("data tables"), "{}", mn.message);

        let r = scan_src(include_str!(
            "../../tests/fixtures/rust/message_wording_states_the_real_fix_and_the_exemption__02.py"
        ));
        let rs = r.iter().find(|x| x.kind == "record-shape").expect("record-shape fires");
        assert!(rs.message.contains("g returns a dict"), "{}", rs.message);
        assert!(
            rs.message
                .contains("Type the return with the class that models the shape"),
            "{}",
            rs.message
        );
        assert!(
            rs.message.contains("create that class if none exists"),
            "{}",
            rs.message
        );
        let mut files: Vec<(&str, &str)> = Vec::new();
        for i in 0..11 {
            files.push((
                Box::leak(format!("x{i}.py").into_boxed_str()),
                include_str!("../../tests/fixtures/rust/abstraction_single_concrete_is_found__04.py"),
            ));
        }
        let heavy = include_str!("../../tests/fixtures/rust/abstraction_single_concrete_is_found__03.py");
        files.push(("heavy.py", Box::leak(heavy.to_string().into_boxed_str())));
        let f = scan_corpus(&files);
        let bulk: Vec<&Finding> = f.iter().filter(|x| x.kind == "bulk-suppression").collect();
        assert_eq!(bulk.len(), 1, "{f:?}");
        assert!(bulk[0].message.contains("13 sites"), "{}", bulk[0].message);
        assert!(bulk[0].message.to_lowercase().contains("policy"), "{}", bulk[0].message);
        assert_eq!(bulk[0].file, "heavy.py", "attached to the file with the most sites");

        let small = scan_corpus(&[
            ("a.py", "x = 1  # lucidlint: ignore record-shape data table row\n"),
            ("b.py", "y = 2  # lucidlint: ignore record-shape data table row\n"),
        ]);
        assert!(!small.iter().any(|x| x.kind == "bulk-suppression"));
    }

    #[test]
    fn unused_never_referenced_is_found() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/unused_never_referenced_is_found__01.py"
        ));
        let u: Vec<&Finding> = f.iter().filter(|x| x.kind == "unused").collect();
        assert_eq!(u.len(), 1);
        assert!(u[0].message.contains("never referenced"));
    }
    #[test]
    fn unused_class_arm_finds_never_referenced_class() {
        // #28 dead-class arm: a class defined in production code that
        // nothing references is as dead as an unreferenced function
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/unused_class_never_referenced_is_found__01.py"
        ));
        let u: Vec<&Finding> = f.iter().filter(|x| x.kind == "unused").collect();
        assert_eq!(u.len(), 1, "{f:?}");
        assert!(u[0].message.contains("class 'Orphan'"), "{}", u[0].message);
    }
    #[test]
    fn unused_class_arm_passes_when_referenced() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/unused_class_referenced_not_flagged__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "unused"), "{f:?}");
    }

    #[test]
    fn unused_referenced_and_main_skip() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/unused_referenced_and_main_skip__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "unused"));
    }

    #[test]
    fn unused_string_dispatch_mention_skips() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/unused_string_dispatch_mention_skips__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "unused"));
    }

    #[test]
    fn unused_decorated_is_referenced() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/unused_decorated_is_referenced__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "unused"));
    }

    #[test]
    fn unused_sees_calls_inside_class_method_lambdas() {
        // the class-body expression descent: a call inside a lambda inside a
        // method is a reference — the old in-class early return (a magic-number
        // suppression that swallowed the whole walk) reported helper as unused,
        // telling the agent to delete live code
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/unused_sees_calls_inside_class_method_lambdas__01.py"
        ));
        let u: Vec<&Finding> = f
            .iter()
            .filter(|x| x.kind == "unused" && x.message.contains("'helper'"))
            .collect();
        assert!(u.is_empty(), "live code flagged unused: {f:?}");
    }

    #[test]
    fn unused_test_only_is_conditional() {
        let f = scan_corpus(&[
            (
                "prod.py",
                include_str!("../../tests/fixtures/rust/unused_test_only_is_conditional__01.py"),
            ),
            (
                "tests/test_prod.py",
                include_str!("../../tests/fixtures/rust/unused_test_only_is_conditional__02.py"),
            ),
        ]);
        let u: Vec<&Finding> = f.iter().filter(|x| x.kind == "unused").collect();
        assert_eq!(u.len(), 1);
        assert!(u[0].message.contains("referenced only from tests"));
    }

    #[test]
    fn unused_in_module_helper_used_later_is_not_flagged() {
        // B2 (review log §1.2): a helper defined and CALLED within the same
        // module is referenced — never a false "unused". (The v0.1.0 bundle
        // counted only cross-file refs and flagged in-module helpers.)
        let f = scan_corpus(&[(
            "m.py",
            include_str!("../../tests/fixtures/rust/unused_in_module_helper_used_later_is_not_flagged__01.py"),
        )]);
        assert!(!f.iter().any(|x| x.kind == "unused"), "{:?}", f);
    }

    #[test]
    fn unused_ignore_comment_is_not_self_defeating() {
        // B3 (review log §5.1): `# lucidlint: ignore unused <why>` must
        // suppress the unused finding AND not be reported stale — a
        // suppression that names a real finding is used, never dead weight.
        let f = scan_corpus(&[(
            "m.py",
            include_str!("../../tests/fixtures/rust/unused_ignore_comment_is_not_self_defeating__01.py"),
        )]);
        assert!(!f.iter().any(|x| x.kind == "unused"), "{:?}", f);
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{:?}", f);
    }

    #[test]
    fn positional_literals_flagged() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/positional_literals_flagged__01.py"
        ));
        assert!(f
            .iter()
            .any(|x| x.kind == "positional-literals" && x.severity == "warn"));
        let ok = scan_src(include_str!(
            "../../tests/fixtures/rust/positional_literals_flagged__02.py"
        ));
        assert!(!ok.iter().any(|x| x.kind == "positional-literals"));
        let ok2 = scan_src(include_str!(
            "../../tests/fixtures/rust/positional_literals_flagged__03.py"
        ));
        assert!(!ok2.iter().any(|x| x.kind == "positional-literals")); // builtin exempt
    }
    #[test]
    fn positional_directive_states_the_callee_case() {
        // The directive states THIS site's case: a same-file callee's bare
        // command fixes (the engine resolves the signature from the call's
        // file); anything else carries its --params slot up front — the
        // magic-number --name contract. No conditional for the reader.
        let local = scan_src(include_str!(
            "../../tests/fixtures/rust/positional_directive_states_the_callee_case__01.py"
        ));
        let m = local.iter().find(|x| x.kind == "positional-literals").unwrap();
        assert!(!m.message.contains("--params"), "{m:?}");
        assert!(m.message.ends_with("— fix: positional-literals"), "{m:?}");
        let ext = scan_src(include_str!(
            "../../tests/fixtures/rust/positional_directive_states_the_callee_case__02.py"
        ));
        let m = ext.iter().find(|x| x.kind == "positional-literals").unwrap();
        assert!(
            m.message.ends_with("— fix: positional-literals --params <names>"),
            "{m:?}"
        );
    }

    #[test]
    fn detached_method_exempts_init_super_and_override() {
        // __init__ can never be static; super() needs the binding even when
        // the body never names self (super().__init__(v) carries no literal
        // self); an @override's binding is the BASE class's contract, not a
        // local judgment call
        let init = scan_src(include_str!(
            "../../tests/fixtures/rust/detached_method_exempts_init_super_and_override__01.py"
        ));
        assert!(!init.iter().any(|x| x.kind == "detached-method"), "{init:?}");
        let sup = scan_src(include_str!(
            "../../tests/fixtures/rust/detached_method_exempts_init_super_and_override__02.py"
        ));
        assert!(!sup.iter().any(|x| x.kind == "detached-method"), "{sup:?}");
        let ovr = scan_src(include_str!(
            "../../tests/fixtures/rust/detached_method_exempts_init_super_and_override__03.py"
        ));
        assert!(!ovr.iter().any(|x| x.kind == "detached-method"), "{ovr:?}");
    }

    #[test]
    fn detached_method_exempts_trivial_stub() {
        // a protocol/abstract placeholder never touches state BY DESIGN —
        // staticmethod-ing it is meaningless (houses:
        // CommuteRouterLike.get_commute, a `...`-body Protocol stub)
        let ell = scan_src(include_str!(
            "../../tests/fixtures/rust/detached_method_exempts_trivial_stub__01.py"
        ));
        assert!(!ell.iter().any(|x| x.kind == "detached-method"), "{ell:?}");
        let rais = scan_src(include_str!(
            "../../tests/fixtures/rust/detached_method_exempts_trivial_stub__02.py"
        ));
        assert!(!rais.iter().any(|x| x.kind == "detached-method"), "{rais:?}");
        // a raise that computes WITHOUT self is real code, not a stub
        let real = scan_src(include_str!(
            "../../tests/fixtures/rust/detached_method_exempts_trivial_stub__03.py"
        ));
        assert!(real.iter().any(|x| x.kind == "detached-method"), "{real:?}");
    }

    #[test]
    fn detached_method_flagged() {
        let f = scan_src(include_str!("../../tests/fixtures/rust/detached_method_flagged__01.py"));
        assert!(f.iter().any(|x| x.kind == "detached-method"));
        let ok = scan_src(include_str!("../../tests/fixtures/rust/detached_method_flagged__02.py"));
        assert!(!ok.iter().any(|x| x.kind == "detached-method"));
        let cm = scan_src(include_str!("../../tests/fixtures/rust/detached_method_flagged__03.py"));
        assert!(!cm.iter().any(|x| x.kind == "detached-method")); // classmethod exempt
        let st = scan_src(include_str!("../../tests/fixtures/rust/detached_method_flagged__04.py"));
        assert!(!st.iter().any(|x| x.kind == "detached-method"));
    }

    #[test]
    fn ignore_duplicate_suppresses_repo_wide_pair_without_stale() {
        // an `ignore duplicate <why>` comment must exempt the repo-wide
        // duplicate finding AND not be flagged stale (B3 wired for every
        // repo-wide family, not just unused — the per-file pass runs before
        // the duplicate pass)
        // the two near-identical bodies are real fixture files; the second
        // variant carries the suppression comment on its first line
        let src = [
            include_str!("../../tests/fixtures/rust/grid_math_pair__01.py"),
            include_str!("../../tests/fixtures/rust/grid_math_pair__03.py"),
        ]
        .concat();
        let f = scan_corpus(&[("prod_mod.py", &src)]);
        assert!(
            !f.iter().any(|x| x.kind == "duplicate"),
            "the ignore must suppress the duplicate"
        );
        assert!(
            !f.iter().any(|x| x.kind == "stale-suppression"),
            "the used suppression must not be stale"
        );
    }

    #[test]
    fn record_shape_twins_carry_distinct_cols_and_keys() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_shape_twins_carry_distinct_cols_and_keys__01.py"
        ));
        let rs: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(rs.len(), 2, "{f:?}");
        assert_ne!(rs[0].col, rs[1].col, "twin anchors must differ: {rs:?}");
        assert_ne!(rs[0].message, rs[1].message, "twin messages must name their keys");
        // A2: the dict-literal message names the constant keys, the drift
        // mechanism, and the class action
        for r in &rs {
            assert!(r.message.contains("This dict has constant keys"), "{}", r.message);
            assert!(r.message.contains("the copies can drift apart"), "{}", r.message);
            assert!(
                r.message.contains("Make a class with these fields and build it once"),
                "{}",
                r.message
            );
        }
        assert!(rs.iter().any(|x| x.message.contains("(a, b)")), "{rs:?}");
        assert!(rs.iter().any(|x| x.message.contains("(x, y)")), "{rs:?}");
    }

    #[test]
    fn suppression_peels_innermost_first() {
        // Phase 4/4a: the window is the finding's own line, the line before,
        // and the logical line's pair — here the return statement is one
        // source line, so the logical line is the finding's own. Stacked
        // markers reach only the line-before marker, so one marker peels the
        // innermost record (highest col) and the outer remains.
        let src_two = include_str!("../../tests/fixtures/code/sample_suppression_peels_innermost_first__0e1dbe26.py");
        let two = scan_src(src_two);
        assert_eq!(
            two.iter().filter(|x| x.kind == "record-shape").count(),
            1,
            "only the line-before marker binds; the outer record remains: {two:?}"
        );

        let src_one = include_str!("../../tests/fixtures/code/sample_suppression_peels_innermost_first__49155807.py");
        let one = scan_src(src_one);
        let left: Vec<&Finding> = one.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(left.len(), 1, "{one:?}");
        assert!(
            left[0].message.contains("(a, b)"),
            "outer must remain: {}",
            left[0].message
        );
        assert!(!one.iter().any(|x| x.kind == "stale-suppression"), "{one:?}");
    }
    #[test]
    fn record_shape_fires_on_call_argument() {
        // uniform descent: a record built at a call site is a record — the
        // old record-position carve-out hid exactly this wire-format shape
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_shape_fires_on_call_argument__01.py"
        ));
        assert!(
            f.iter()
                .any(|x| x.kind == "record-shape" && x.message.contains("(user, stamp)")),
            "{f:?}"
        );
        assert!(
            f.iter()
                .any(|x| x.kind == "record-shape" && x.message.contains("This dict has constant keys")),
            "{f:?}"
        );
    }

    #[test]
    fn record_shape_parameter_names_the_param_and_the_action() {
        // A1: the parameter arm names the parameter, the untyped-dict
        // mechanism, and the class action
        let f = scan_src(include_str!(
            "../../tests/fixtures/code/sample_record_shape_parameter_names_the_param_and_the_action__ad65c27b.py"
        ));
        let rs: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(rs.len(), 1, "{f:?}");
        assert!(
            rs[0]
                .message
                .contains("data is a dict; its value is a fixed-shape record"),
            "{}",
            rs[0].message
        );
        assert!(
            rs[0]
                .message
                .contains("call sites build it ad hoc, and field changes go unchecked"),
            "{}",
            rs[0].message
        );
        assert!(
            rs[0]
                .message
                .contains("Type data with the class that models this shape"),
            "{}",
            rs[0].message
        );
    }
    #[test]
    fn record_scalar_map_signature_is_a_lookup_not_a_record() {
        // G1: a parameter or return typed with a dict whose VALUE element is
        // scalar (str/None/bool stamps and keys) is a lookup, not a record —
        // derived_node's dep_timestamps: dict[str, str] shape emits nothing.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_scalar_map_signature_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{f:?}");
    }

    #[test]
    fn record_class_value_in_signature_keeps_the_finding() {
        // H1: a dict whose value element is class-name-like (capitalized)
        // holds shaped objects — the finding stays, with the COLLECTION as
        // the object of the fix (the value's class is already named by the
        // annotation), never the fixed-shape-record pointer.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_class_value_signature_keeps_finding__01.py"
        ));
        let rs: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(rs.len(), 1, "{f:?}");
        assert!(
            rs[0].message.contains("data is a map whose values are User"),
            "{}",
            rs[0].message
        );
        assert!(
            rs[0]
                .message
                .contains("Make a class for the collection, named with a domain noun — an alias to dict names the problem, it does not solve it"),
            "{}",
            rs[0].message
        );
        assert!(!rs[0].message.contains("fixed-shape record"), "{}", rs[0].message);
    }

    #[test]
    fn record_class_value_map_names_the_collection_not_the_value() {
        // H1: dict[str, SomeClass] — the collection is the unnamed shape;
        // the message names the MAP (sources), never the value (Provenance
        // is already named by the annotation).
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_class_value_map_collection_message__01.py"
        ));
        let rs: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(rs.len(), 1, "{f:?}");
        assert!(
            rs[0].message.contains("sources is a map whose values are Provenance"),
            "{}",
            rs[0].message
        );
        assert!(
            rs[0]
                .message
                .contains("Make a class for the collection, named with a domain noun — an alias to dict names the problem, it does not solve it"),
            "{}",
            rs[0].message
        );
        assert!(!rs[0].message.contains("fixed-shape record"), "{}", rs[0].message);
    }

    #[test]
    fn record_named_type_values_render_the_collection_message_in_every_arm() {
        // P1: the dict value element is classified by AST name resolution,
        // never capitalization — a module class (Provenance), an imported
        // name (from decimal import Decimal), and a known builtin
        // constructor (list) are named types, and the collection message
        // renders in EVERY arm: parameter annotation, return annotation,
        // and dict literal. The unbound-capitalized case keeps its existing
        // behavior (record_class_value_in_signature_keeps_the_finding).
        // Issue #46 narrowed ONE parameter case: a read-only parameter whose
        // value class this module declares is exempt — see
        // record_shape_exempts_a_read_only_typed_map_parameter.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_named_type_value_map_renders_collection_message__01.py"
        ));
        let rs: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(rs.len(), 4, "{f:?}");
        assert!(
            rs.iter()
                .any(|x| x.message.contains("data is a map whose values are Decimal")),
            "{f:?}"
        );
        assert!(
            rs.iter()
                .any(|x| x.message.contains("convert returns a map whose values are Decimal")),
            "{f:?}"
        );
        assert!(
            rs.iter()
                .any(|x| x.message.contains("rows is a map whose values are list")),
            "{f:?}"
        );
        assert!(
            rs.iter()
                .any(|x| x.message.contains("This dict is a map whose values are Provenance")),
            "{f:?}"
        );
        for x in &rs {
            assert!(
                x.message
                    .contains("Make a class for the collection, named with a domain noun — an alias to dict names the problem, it does not solve it"),
                "{}",
                x.message
            );
            assert!(!x.message.contains("fixed-shape record"), "{}", x.message);
        }
    }

    #[test]
    fn record_shape_exempts_a_read_only_typed_map_parameter() {
        // Issue #46: the map arrives typed with the class that names its
        // values, and the function only reads it — the content is named and
        // no operation belongs to the collection, so there is no missing noun
        // to point at. A map the function mutates is being built here: it
        // keeps the collection message.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_named_type_map_parameter_read_only_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
        let g = scan_src(include_str!(
            "../../tests/fixtures/rust/record_named_type_map_parameter_mutated_fails__01.py"
        ));
        let rs: Vec<&Finding> = g.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(rs.len(), 2, "{g:?}");
        for x in &rs {
            assert!(
                x.message.contains("is a map whose values are Provenance"),
                "{}",
                x.message
            );
        }
    }

    #[test]
    fn record_optional_and_aliased_named_type_maps_render_the_collection_message() {
        // Phase 1: the union wrapper (`dict[str, Provenance] | None`) and the
        // raw-dict alias (`Sources = dict[str, Provenance]`) both resolve to
        // the same map — the collection message renders in every arm, and the
        // alias names the problem rather than exempting it.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_optional_named_type_map__01.py"
        ));
        let rs: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(rs.len(), 1, "{f:?}");
        assert!(
            rs[0].message.contains("sources is a map whose values are Provenance"),
            "{}",
            rs[0].message
        );
        assert!(
            rs[0]
                .message
                .contains("an alias to dict names the problem, it does not solve it"),
            "{}",
            rs[0].message
        );
        assert!(!rs[0].message.contains("fixed-shape record"), "{}", rs[0].message);

        let a = scan_src(include_str!(
            "../../tests/fixtures/rust/record_raw_dict_alias_does_not_exempt__01.py"
        ));
        let ars: Vec<&Finding> = a.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(ars.len(), 1, "{a:?}");
        assert!(
            ars[0].message.contains("sources is a map whose values are Provenance"),
            "{}",
            ars[0].message
        );
        assert!(
            ars[0]
                .message
                .contains("an alias to dict names the problem, it does not solve it"),
            "{}",
            ars[0].message
        );
        assert!(
            ars[0]
                .message
                .contains("Move the operations that take, build, or read this map onto the class as methods"),
            "{}",
            ars[0].message
        );
    }

    #[test]
    fn record_unresolvable_value_element_keeps_the_g1_path() {
        // P1: a value element that resolves to nothing is NOT a named type —
        // dict[str, Any] keeps the fixed-shape text (grab-bag) and a scalar
        // map stays silent; neither renders the collection message.
        let f = scan_src(include_str!(
            "../../tests/fixtures/code/sample_record_shape_parameter_names_the_param_and_the_action__ad65c27b.py"
        ));
        let rs: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(rs.len(), 1, "{f:?}");
        assert!(
            rs[0]
                .message
                .contains("data is a dict; its value is a fixed-shape record"),
            "{}",
            rs[0].message
        );
        assert!(
            !rs[0].message.contains("is a map whose values are"),
            "{}",
            rs[0].message
        );
        let g = scan_src(include_str!(
            "../../tests/fixtures/code/sample_record_unresolvable_value_element_keeps_the_g1_path__8ebdcce8.py"
        ));
        assert!(!g.iter().any(|x| x.kind == "record-shape"), "{g:?}");
    }

    #[test]
    fn record_signature_findings_anchor_at_the_parameter_line() {
        // H2: a parameter finding anchors at the annotated parameter's own
        // line and a return finding at the return annotation's line — never
        // the def line for a multi-line signature.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_multiline_signature_anchors_param_line__01.py"
        ));
        let rs: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        let mut lines: Vec<usize> = rs.iter().map(|x| x.line).collect();
        lines.sort_unstable();
        assert_eq!(lines, vec![2, 3, 10], "{f:?}");
    }

    #[test]
    fn record_signature_marker_binds_on_the_parameter_line() {
        // H2: the marker window is resolution-invariant — a marker on the
        // parameter's own line binds the finding anchored there, and is not
        // stale.
        let src = include_str!(
            "../../tests/fixtures/code/sample_record_signature_marker_binds_on_the_parameter_line__b4b24b4f.py"
        );
        let f = scan_src(src);
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{f:?}");
    }

    #[test]
    fn record_def_above_marker_binds_every_parameter() {
        // Phase 4/4a: the binding window is the finding's own line, the line
        // before, and the logical line's pair (nothing extra here — the def
        // is one source line). A single-line def anchors every parameter
        // finding at the def line, so ONE marker one line above the def binds
        // all four params — the def site is one decision.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_def_above_marker_binds_def_site__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{f:?}");
    }

    #[test]
    fn record_def_line_marker_binds_every_parameter() {
        // Phase 4: a marker ON the def line (a trailing comment) is the
        // finding's own line — it binds every parameter anchored there.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_def_line_marker_binds_def_site__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{f:?}");
    }

    #[test]
    fn record_logical_def_above_marker_binds_multiline_signature() {
        // Phase 4a: one line of code can span several source lines. The
        // marker sits one line above the def, the parameter and return
        // findings anchor on their own interior lines — the def line is the
        // signature's LOGICAL line, so the single marker binds every finding
        // in the signature and is not stale (the round-9 shape:
        // dag/attempt.py:298 marker -> :304 finding).
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_logical_def_above_marker_binds_multiline_signature__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{f:?}");
    }

    #[test]
    fn record_logical_def_line_marker_binds_multiline_signature() {
        // Phase 4a: a marker ON the def line binds every finding in a
        // multi-line signature — the def line is the logical line the
        // parameter and return findings belong to.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_logical_def_line_marker_binds_multiline_signature__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{f:?}");
    }

    #[test]
    fn record_logical_wrapped_call_marker_binds() {
        // Phase 4a: the record literal's own line is inside a wrapped call,
        // whose logical line starts at `send(`. A marker one line above the
        // call's first line binds the argument's finding and is not stale.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_logical_wrapped_call_marker_binds__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{f:?}");
    }

    #[test]
    fn record_logical_physical_line_marker_binds_inside_wrapped_call() {
        // Phase 4a: statement-level precision survives — a marker on the
        // finding's own source line binds it even when the construct started
        // on an earlier line.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_logical_physical_line_marker_binds__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{f:?}");
    }

    #[test]
    fn record_logical_two_lines_above_binds_nothing_and_is_not_stale() {
        // Phase 4a guard: a marker two lines above the logical line (and
        // above the physical line too) is beyond the union window — the
        // finding fires, the marker stays as documentation, and no stale
        // verdict is emitted (the family still fires).
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_logical_two_lines_above_binds_nothing__01.py"
        ));
        let rs: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(rs.len(), 1, "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{f:?}");
    }

    #[test]
    fn record_signature_gap_marker_does_not_bind_and_is_not_stale() {
        // Phase 4/4a: a marker at def+2 binds neither the finding's own line
        // pair nor the signature's logical line pair — it is beyond the union
        // window, so the finding still fires, the marker stays as
        // documentation, and no stale verdict is emitted (the family still
        // fires).
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_signature_gap_marker_does_not_bind__01.py"
        ));
        let rs: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(rs.len(), 1, "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{f:?}");
    }

    #[test]
    fn record_union_with_module_from_dict_class_emits_the_union_message() {
        // G2: `X | dict` where X is a class IN THE MODULE with from_dict —
        // tfl_client's _disambiguate_national_rail shape. The message states
        // the union and directs the ingestion; the from_dict method itself
        // is a typed parse boundary and emits nothing (F1).
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_union_with_module_from_dict_class__01.py"
        ));
        let rs: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(rs.len(), 1, "{f:?}");
        assert!(
            rs[0]
                .message
                .contains("data accepts the wire dict alongside the _TflJourneyResponse class"),
            "{}",
            rs[0].message
        );
        assert!(
            rs[0].message.contains(
                "Ingest the wire at entry with _TflJourneyResponse.from_dict and drop the dict from the union"
            ),
            "{}",
            rs[0].message
        );
    }

    #[test]
    fn record_union_without_module_class_keeps_the_ad_hoc_text() {
        // G2: the same union with NO in-module from_dict class stays on the
        // existing path — the ad-hoc dict text, not the union message.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_union_without_module_class_keeps_ad_hoc__01.py"
        ));
        let rs: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(rs.len(), 1, "{f:?}");
        assert!(
            rs[0]
                .message
                .contains("data is a dict; its value is a fixed-shape record"),
            "{}",
            rs[0].message
        );
        assert!(
            !rs[0].message.contains("accepts the wire dict alongside"),
            "{}",
            rs[0].message
        );
    }

    #[test]
    fn record_suppression_one_line_above_def_binds_the_def_site() {
        // Phase 4/4a: the marker on the line before the def binds the
        // def-anchored return finding — the window is the finding's own line,
        // the line before it, and the logical line's pair (here the def is one
        // source line). The method is deliberately NOT to_dict/to_json: an
        // export-named method on a field-bearing class emits no finding at all
        // (issue #42), which would leave the marker stale rather than binding.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_suppression_one_line_above_def_binds__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{f:?}");
    }

    #[test]
    fn record_suppression_one_marker_binds_the_def_site_twins() {
        // G3 extract_bus_fares shape: two record-shaped params on one def
        // line both report at the def anchor (col 0) — ONE marker above the
        // def binds both; the site is one decision.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_suppression_one_marker_binds_def_site__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{f:?}");
    }

    #[test]
    fn suppression_marker_above_class_binds_the_class_finding() {
        // G3 class anchor: a marker one line above a class binds a finding
        // reported at the class line (process-class) and is not stale.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/suppression_marker_above_class_binds__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "process-class"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{f:?}");
    }

    #[test]
    fn suppression_far_above_def_does_not_bind_and_is_not_stale() {
        // Phase 4/4a guard: a marker 3+ lines above the def is beyond the
        // union window — the def is one source line, so its logical line adds
        // no earlier anchor. The finding fires, the marker stays as
        // documentation, and no stale verdict is emitted (api_router's 9-line
        // gap).
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_suppression_far_above_does_not_bind__01.py"
        ));
        let rs: Vec<&Finding> = f.iter().filter(|x| x.kind == "record-shape").collect();
        assert_eq!(rs.len(), 1, "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{f:?}");
    }

    #[test]
    fn truncated_marker_binds_by_signal_alone() {
        // G5: a repo marker cut mid-expression at line end (bus.py:122 —
        // "…→ Attempt (dict[str,") still binds: the why may truncate; only
        // the signal matters, and the why is not empty, so the why-less
        // rule never applies.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/record_truncated_marker_binds__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "record-shape"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"), "{f:?}");
    }

    #[test]
    fn complexity_marker_binding_follows_the_two_line_window() {
        // Phase 4/4a: the window is the finding's own line and the line
        // before. Complexity summaries are not Finding records and carry no
        // logical line, so the plain two-line window governs here. The cc
        // retain uses the same window, so a marker on the line before the def
        // is consumed (not stale); a marker two lines above (a decorator
        // intervenes) binds nothing, and since the findings stream carries no
        // complexity finding it is reported stale.
        let mut body = String::new();
        for i in 0..16 {
            body.push_str(&format!("    if a{i}:\n        x{i} = {i}\n"));
        }
        let params = "a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15";
        let bound = format!("# lucidlint: ignore complexity calibrated\ndef f({params}):\n{body}    return 0\n");
        let b = scan_src(&bound);
        assert!(!b.iter().any(|x| x.kind == "stale-suppression"), "{b:?}");

        let out = format!("# lucidlint: ignore complexity calibrated\n@deco\ndef f({params}):\n{body}    return 0\n");
        let o = scan_src(&out);
        assert!(o.iter().any(|x| x.kind == "stale-suppression"), "{o:?}");
    }

    #[test]
    fn comma_signal_suppresses_both_families() {
        // one comment, comma-separated signals — the only shape that fits the
        // line/line-1 window for two families on one def
        let src = include_str!("../../tests/fixtures/rust/comma_signal_suppresses_both_families__01.py");
        let f = scan_src(src);
        assert!(!f.iter().any(|x| x.kind == "long-param-list"));
        assert!(!f.iter().any(|x| x.kind == "detached-method"));
        assert!(!f.iter().any(|x| x.kind == "stale-suppression"));
    }

    #[test]
    fn guard_clauses_arrow_code_detected() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/guard_clauses_arrow_code_detected__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "guard-clauses"));
        let ok = scan_src(include_str!(
            "../../tests/fixtures/rust/guard_clauses_arrow_code_detected__02.py"
        ));
        assert!(!ok.iter().any(|x| x.kind == "guard-clauses")); // 2 levels is fine
    }

    #[test]
    fn scattered_instance_objects_detected() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/scattered_instance_objects_detected__01.py"
        ));
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "scattered-instance").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(hits[0].message.contains("A, B, and G"), "{}", hits[0].message);
        // one object is the container itself; a stdlib constructor is not a
        // repo class
        let ok = scan_src(include_str!(
            "../../tests/fixtures/rust/scattered_instance_objects_detected__02.py"
        ));
        assert!(!ok.iter().any(|x| x.kind == "scattered-instance"), "{ok:?}");
        // two objects of DIFFERENT classes are still a scattering: nothing
        // says which one holds the other
        let two = scan_src(include_str!(
            "../../tests/fixtures/rust/scattered_instance_objects_detected__03.py"
        ));
        assert!(two.iter().any(|x| x.kind == "scattered-instance"), "{two:?}");
    }

    #[test]
    fn unnamed_condition_joins_facts_detected() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/unnamed_condition_joins_facts_detected__01.py"
        ));
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "unnamed-condition").collect();
        assert_eq!(hits.len(), 2, "{f:?}"); // decide + shape
        assert!(
            hits.iter().any(|h| h.message.contains("about order and user")),
            "{hits:?}"
        );
        assert!(hits.iter().any(|h| h.message.contains("about value")), "{hits:?}");
        // two facts, a named predicate, and self-only state are all silent
        let ok = scan_src(include_str!(
            "../../tests/fixtures/rust/unnamed_condition_joins_facts_detected__02.py"
        ));
        assert!(!ok.iter().any(|x| x.kind == "unnamed-condition"), "{ok:?}");
        // __eq__ IS the decision: the name exists
        let eq = scan_src(include_str!(
            "../../tests/fixtures/rust/unnamed_condition_joins_facts_detected__03.py"
        ));
        assert!(!eq.iter().any(|x| x.kind == "unnamed-condition"), "{eq:?}");
    }

    #[test]
    fn unnamed_condition_sees_a_nested_bool_op_in_a_comparison() {
        // `a > (b or c)` is ONE operand at the top level and three nouns
        // inside it — the nested-BoolOp arm of operand_nouns is live
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/unnamed_condition_joins_facts_detected__04.py"
        ));
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "unnamed-condition").collect();
        assert_eq!(hits.len(), 1, "{f:?}"); // the 3-operand nested one; 2 operands is silent
        assert!(
            hits[0].message.contains("about a, b, c, d, and e"),
            "{}",
            hits[0].message
        );
    }

    #[test]
    fn unnamed_condition_answer_shape_ignores_a_nested_defs_returns() {
        // a nested def's `return 1` belongs to that def: if it counted, `outer`
        // would look like it answers twice and its own returned condition would
        // be reported
        let ok = scan_src(include_str!(
            "../../tests/fixtures/rust/unnamed_condition_joins_facts_detected__02.py"
        ));
        assert!(!ok.iter().any(|x| x.kind == "unnamed-condition"), "{ok:?}");
    }

    #[test]
    fn unnamed_classification_cases_detected() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/unnamed_classification_cases_detected__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "unnamed-classification"), "{f:?}");
        // the match form, and a plain function is not a classification
        let m = scan_src(include_str!(
            "../../tests/fixtures/rust/unnamed_classification_cases_detected__02.py"
        ));
        let hits: Vec<&Finding> = m.iter().filter(|x| x.kind == "unnamed-classification").collect();
        assert_eq!(hits.len(), 1, "{m:?}");
    }

    #[test]
    fn shared_path_names_the_pair_class() {
        let a = include_str!("../../tests/fixtures/code/sample_shared_path_names_the_pair_class__7f82cf64.py");
        let b = include_str!("../../tests/fixtures/code/sample_shared_path_names_the_pair_class__4da8b081.py");
        let f = scan_corpus(&[("pkg/paths.py", a), ("pkg/other.py", b)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "shared-path").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(hits[0].message.contains("(work, batch)"), "{}", hits[0].message);
        assert!(hits[0].message.contains("\"ocr-guess\""), "{}", hits[0].message);
    }

    #[test]
    fn shared_path_without_a_pair_names_one_home() {
        let a = include_str!("../../tests/fixtures/code/sample_shared_path_without_a_pair_names_one_home__f3999d88.py");
        let b = include_str!("../../tests/fixtures/code/sample_shared_path_without_a_pair_names_one_home__35af30f3.py");
        let f = scan_corpus(&[("a.py", a), ("b.py", b)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "shared-path").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(hits[0].message.contains("\"words.json\""), "{}", hits[0].message);
        assert!(hits[0].message.contains("no home"), "{}", hits[0].message);
    }

    #[test]
    fn shared_path_ignores_bare_extensions_and_test_files() {
        let a = include_str!(
            "../../tests/fixtures/code/sample_shared_path_ignores_bare_extensions_and_test_files__6919540d.py"
        );
        let b = include_str!(
            "../../tests/fixtures/code/sample_shared_path_ignores_bare_extensions_and_test_files__260a8a31.py"
        );
        let prod = scan_corpus(&[("a.py", a), ("b.py", b)]);
        assert!(!prod.iter().any(|x| x.kind == "shared-path"), "{prod:?}");
        let tests = scan_corpus(&[("tests/a.py", a), ("tests/b.py", b)]);
        assert!(!tests.iter().any(|x| x.kind == "shared-path"), "{tests:?}");
    }

    #[test]
    fn shared_path_needs_two_modules() {
        let a = include_str!("../../tests/fixtures/code/sample_shared_path_needs_two_modules__f0b4f5f4.py");
        let f = scan_corpus(&[("a.py", a)]);
        assert!(!f.iter().any(|x| x.kind == "shared-path"), "{f:?}");
    }

    #[test]
    fn shared_path_reads_a_path_constructor() {
        let a = include_str!("../../tests/fixtures/code/sample_shared_path_reads_a_path_constructor__d8184de9.py");
        let b = include_str!("../../tests/fixtures/code/sample_shared_path_reads_a_path_constructor__26bc0418.py");
        let f = scan_corpus(&[("a.py", a), ("b.py", b)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "shared-path").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(
            hits[0].message.contains("Path() / \"data\" / \"commute\""),
            "{}",
            hits[0].message
        );
    }

    #[test]
    fn shared_path_normalises_a_suffix_literal_and_an_fstring() {
        let a = include_str!(
            "../../tests/fixtures/code/sample_shared_path_normalises_a_suffix_literal_and_an_fstring__b2c1da26.py"
        );
        let b = include_str!(
            "../../tests/fixtures/code/sample_shared_path_normalises_a_suffix_literal_and_an_fstring__613ab0b7.py"
        );
        let c = include_str!(
            "../../tests/fixtures/code/sample_shared_path_normalises_a_suffix_literal_and_an_fstring__f69a2fde.py"
        );
        let f = scan_corpus(&[("a.py", a), ("b.py", b), ("c.py", c)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "shared-path").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(hits[0].message.contains("\".layout.json\""), "{}", hits[0].message);
    }

    #[test]
    fn shared_path_does_not_claim_a_pair_one_use_lacks() {
        let a = include_str!(
            "../../tests/fixtures/code/sample_shared_path_does_not_claim_a_pair_one_use_lacks__28118d06.py"
        );
        let b = include_str!(
            "../../tests/fixtures/code/sample_shared_path_does_not_claim_a_pair_one_use_lacks__163f5e2a.py"
        );
        let f = scan_corpus(&[("a.py", a), ("b.py", b)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "shared-path").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(!hits[0].message.contains("(work, batch)"), "{}", hits[0].message);
    }

    #[test]
    fn duplicate_expression_names_a_helper() {
        let a = include_str!("../../tests/fixtures/code/sample_duplicate_expression_names_a_helper__6ea57db9.py");
        let b = include_str!("../../tests/fixtures/code/sample_duplicate_expression_names_a_helper__35088aad.py");
        let f = scan_corpus(&[("a.py", a), ("b.py", b)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate-expression").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(
            hits[0].message.contains("Rectangle(p.x0 * SCALE"),
            "{}",
            hits[0].message
        );
        assert!(hits[0].message.contains("helper"), "{}", hits[0].message);
    }

    #[test]
    fn duplicate_expression_counts_two_occurrences_in_one_module() {
        let src = include_str!(
            "../../tests/fixtures/code/sample_duplicate_expression_counts_two_occurrences_in_one_module__8f53f3d7.py"
        );
        let f = scan_corpus(&[("a.py", src)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate-expression").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
    }

    #[test]
    fn duplicate_expression_ignores_one_step_reads_and_defaults() {
        let a = include_str!(
            "../../tests/fixtures/code/sample_duplicate_expression_ignores_one_step_reads_and_defaults__a4e34a96.py"
        );
        let b = include_str!(
            "../../tests/fixtures/code/sample_duplicate_expression_ignores_one_step_reads_and_defaults__d6559429.py"
        );
        let f = scan_corpus(&[("a.py", a), ("b.py", b)]);
        assert!(!f.iter().any(|x| x.kind == "duplicate-expression"), "{f:?}");
    }

    #[test]
    fn duplicate_expression_reports_the_bound_expression_not_the_name() {
        let a = include_str!("../../tests/fixtures/code/sample_duplicate_expression_reports_the_bound_expression_not_the_name__626d02a5.py");
        let b = include_str!("../../tests/fixtures/code/sample_duplicate_expression_reports_the_bound_expression_not_the_name__73846314.py");
        let f = scan_corpus(&[("a.py", a), ("b.py", b)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate-expression").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(hits[0].message.contains("x > 0 and y > 0"), "{}", hits[0].message);
    }

    #[test]
    fn duplicate_expression_counts_a_multi_fact_condition() {
        let a = include_str!(
            "../../tests/fixtures/code/sample_duplicate_expression_counts_a_multi_fact_condition__8cc24af1.py"
        );
        let b = include_str!(
            "../../tests/fixtures/code/sample_duplicate_expression_counts_a_multi_fact_condition__0f5f43d3.py"
        );
        let f = scan_corpus(&[("a.py", a), ("b.py", b)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate-expression").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
    }

    #[test]
    fn duplicate_expression_counts_a_filtered_comprehension() {
        let a = include_str!(
            "../../tests/fixtures/code/sample_duplicate_expression_counts_a_filtered_comprehension__a2597679.py"
        );
        let b = include_str!(
            "../../tests/fixtures/code/sample_duplicate_expression_counts_a_filtered_comprehension__6b475431.py"
        );
        let f = scan_corpus(&[("a.py", a), ("b.py", b)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate-expression").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
    }

    #[test]
    fn duplicate_expression_leaves_paths_to_shared_path() {
        let a = include_str!(
            "../../tests/fixtures/code/sample_duplicate_expression_leaves_paths_to_shared_path__b81e9491.py"
        );
        let b = include_str!(
            "../../tests/fixtures/code/sample_duplicate_expression_leaves_paths_to_shared_path__c48525f3.py"
        );
        let f = scan_corpus(&[("a.py", a), ("b.py", b)]);
        assert!(!f.iter().any(|x| x.kind == "duplicate-expression"), "{f:?}");
    }

    #[test]
    fn duplicate_expression_ignores_bare_statement_calls() {
        let a = include_str!(
            "../../tests/fixtures/code/sample_duplicate_expression_ignores_bare_statement_calls__e5212682.py"
        );
        let f = scan_corpus(&[("a.py", a)]);
        assert!(!f.iter().any(|x| x.kind == "duplicate-expression"), "{f:?}");
    }

    #[test]
    fn duplicate_expression_folds_a_subexpression_into_its_parent() {
        let a = include_str!(
            "../../tests/fixtures/code/sample_duplicate_expression_folds_a_subexpression_into_its_parent__6ea2f686.py"
        );
        let b = include_str!(
            "../../tests/fixtures/code/sample_duplicate_expression_folds_a_subexpression_into_its_parent__74071eb9.py"
        );
        let f = scan_corpus(&[("a.py", a), ("b.py", b)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate-expression").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(hits[0].message.contains("Rectangle"), "{}", hits[0].message);
    }

    #[test]
    fn duplicate_expression_abstracts_the_key_a_helper_would_take() {
        let a = include_str!(
            "../../tests/fixtures/code/sample_duplicate_expression_abstracts_the_key_a_helper_would_take__5a147420.py"
        );
        let b = include_str!(
            "../../tests/fixtures/code/sample_duplicate_expression_abstracts_the_key_a_helper_would_take__82bd8bd9.py"
        );
        let c = include_str!(
            "../../tests/fixtures/code/sample_duplicate_expression_abstracts_the_key_a_helper_would_take__b2228ce5.py"
        );
        let f = scan_corpus(&[("a.py", a), ("b.py", b), ("c.py", c)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "duplicate-expression").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(hits[0].message.contains("read_text"), "{}", hits[0].message);
        assert!(hits[0].message.contains("one"), "{}", hits[0].message);
    }

    #[test]
    fn shared_construction_names_the_constructor_to_add() {
        let geo = include_str!("../../tests/fixtures/code/sample_shared_construction_geo.py");
        let parse = include_str!("../../tests/fixtures/code/sample_shared_construction_parse.py");
        let f = scan_corpus(&[("pkg/geo.py", geo), ("pkg/parse.py", parse)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "shared-construction").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        let message = &hits[0].message;
        assert!(message.contains("named constructor on `GeoPoint`"), "{message}");
        assert!(message.contains("from_values"), "{message}");
        assert!(message.contains("2 places"), "{message}");
        assert!(
            message.contains("pkg/geo.py") && message.contains("pkg/parse.py"),
            "{message}"
        );
    }

    #[test]
    fn shared_construction_ignores_typed_values_single_sites_one_function_and_foreign_classes() {
        let direct = include_str!("../../tests/fixtures/code/sample_shared_construction_direct.py");
        let f = scan_corpus(&[("pkg/geo.py", direct)]);
        assert!(
            !f.iter().any(|x| x.kind == "shared-construction"),
            "typed values: {f:?}"
        );
        let single = include_str!("../../tests/fixtures/code/sample_shared_construction_single.py");
        let f = scan_corpus(&[("pkg/geo.py", single)]);
        assert!(!f.iter().any(|x| x.kind == "shared-construction"), "one site: {f:?}");
        let one_fn = include_str!("../../tests/fixtures/code/sample_shared_construction_one_function.py");
        let f = scan_corpus(&[("pkg/geo.py", one_fn)]);
        assert!(
            !f.iter().any(|x| x.kind == "shared-construction"),
            "one function: {f:?}"
        );
        let third = include_str!("../../tests/fixtures/code/sample_shared_construction_third_party.py");
        let f = scan_corpus(&[("pkg/a.py", third), ("pkg/b.py", third)]);
        assert!(
            !f.iter().any(|x| x.kind == "shared-construction"),
            "foreign class: {f:?}"
        );
    }

    /// A synthetic module with `n` classes; `with_methods` of them carry a
    /// method, the rest are records.
    fn several_classes_source(n: usize, with_methods: usize) -> String {
        let mut src = String::new();
        for i in 0..n {
            src.push_str(&format!("class Widget{i}:\n"));
            if i < with_methods {
                src.push_str(&format!("    def run_{i}(self, value):\n        return value + {i}\n"));
            } else {
                src.push_str(&format!("    size: int = {i}\n"));
            }
            src.push('\n');
        }
        src
    }

    /// A synthetic module with `n` free functions and `classes` classes.
    fn definitions_source(n: usize, classes: usize) -> String {
        let mut src = String::new();
        for i in 0..classes {
            src.push_str(&format!("class Holder{i}:\n    pass\n\n"));
        }
        for i in 0..n {
            src.push_str(&format!("def step_{i}(value):\n    return value + {i}\n\n"));
        }
        src
    }

    #[test]
    fn several_definitions_fires_at_three_and_names_the_classes() {
        let src = several_classes_source(3, 3);
        let f = scan_corpus(&[("pkg/widgets.py", &src)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "several-definitions").collect();
        assert_eq!(
            hits.len(),
            1,
            "{:?}",
            hits.iter().map(|h| &h.message).collect::<Vec<_>>()
        );
        let msg = &hits[0].message;
        assert!(
            msg.contains("holds 3 top-level definitions, 3 of them classes"),
            "{msg}"
        );
        assert!(msg.contains("Widget0") && msg.contains("Widget1"), "{msg}");
        assert!(msg.contains("package"), "{msg}");
        assert!(msg.contains("one file per class"), "{msg}");
        assert!(msg.contains("re-exported from __init__.py"), "{msg}");
        assert_eq!(hits[0].file, "pkg/widgets.py");
        assert_eq!(hits[0].line, 0);
        assert!(!f.iter().any(|x| x.kind == "many-definitions"), "{f:?}");
    }

    #[test]
    fn several_definitions_passes_two_definitions() {
        // a class and the record it returns is a pair, not a module
        let src = several_classes_source(2, 2);
        let f = scan_corpus(&[("pkg/widget.py", &src)]);
        assert!(!f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
        let two_functions = definitions_source(2, 0);
        let f = scan_corpus(&[("pkg/steps.py", &two_functions)]);
        assert!(!f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
    }

    #[test]
    fn several_definitions_asks_for_classes_when_functions_dominate() {
        let src = definitions_source(4, 1);
        let f = scan_corpus(&[("pkg/steps.py", &src)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "several-definitions").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        let msg = &hits[0].message;
        assert!(
            msg.contains("holds 5 top-level definitions, 4 of them functions"),
            "{msg}"
        );
        assert!(msg.contains("owns no state"), "{msg}");
        assert!(msg.contains("becomes its fields"), "{msg}");
        assert!(msg.contains("the functions its methods"), "{msg}");
        assert!(!msg.contains("package"), "{msg}");
        assert!(!msg.contains("A script is not different"), "{msg}");
    }

    #[test]
    fn several_definitions_still_asks_for_classes_when_one_class_is_outnumbered() {
        // one behaviour class + its record, then five free functions: the
        // functions are strictly more than the two class equivalents, so the
        // functions arm speaks. The tie is only one class + its helpers.
        let src = include_str!("../../tests/fixtures/code/sample_several_definitions_outnumbered_class.py");
        let f = scan_corpus(&[("pkg/markers.py", src)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "several-definitions").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(hits[0].message.contains("5 of them functions"), "{}", hits[0].message);
    }

    #[test]
    fn several_definitions_reports_a_class_and_its_records() {
        // more than two definitions fires, whether the siblings are records
        // or acting classes: only Rust class equivalents are grouped
        let src = several_classes_source(3, 1);
        let f = scan_corpus(&[("pkg/widget.py", &src)]);
        assert!(f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
        let records = several_classes_source(4, 0);
        let f = scan_corpus(&[("pkg/records.py", &records)]);
        assert!(f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
    }

    #[test]
    fn several_definitions_reports_a_class_and_its_constructor_record() {
        // the constructor still does not make the record behavioural; the
        // three top-level definitions still fire
        let src = include_str!("../../tests/fixtures/code/sample_several_definitions_record_constructor.py");
        let f = scan_corpus(&[("pkg/markers.py", src)]);
        assert!(f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
        // a second class that acts on its own values is a second concern
        let src = include_str!("../../tests/fixtures/code/sample_several_definitions_second_behaviour.py");
        let f = scan_corpus(&[("pkg/markers.py", src)]);
        assert!(f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
    }

    #[test]
    fn several_definitions_reports_a_class_its_records_and_its_helpers() {
        // more than two definitions fires; the types arm names the class when
        // neither side is the bulk
        let src = include_str!("../../tests/fixtures/code/sample_several_definitions_records_and_helpers.py");
        let f = scan_corpus(&[("pkg/batch_row.py", src)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "several-definitions").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(hits[0].message.contains("2 of them classes"), "{}", hits[0].message);
        // the same two helpers with the class gone are a bag of functions
        let src = include_str!("../../tests/fixtures/code/sample_several_definitions_helpers_only.py");
        let f = scan_corpus(&[("pkg/rows.py", src)]);
        assert!(f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
    }

    #[test]
    fn several_definitions_exempts_private_helpers() {
        // one class and its private parsers are one concept: the helpers are
        // the module's own, not a second concern
        let src = include_str!("../../tests/fixtures/code/sample_several_definitions_private_helpers.py");
        let f = scan_corpus(&[("pkg/school.py", src)]);
        assert!(!f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
        // the exemption is the private name's: public helpers still count
        let src = include_str!("../../tests/fixtures/code/sample_several_definitions_helpers_only.py");
        let f = scan_corpus(&[("pkg/rows.py", src)]);
        assert!(f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
        // a module of private helpers is one implementation, not a bag of concepts
        let src = include_str!("../../tests/fixtures/code/sample_several_definitions_private_helpers_only.py");
        let f = scan_corpus(&[("pkg/rows.py", src)]);
        assert!(!f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
    }

    #[test]
    fn several_definitions_treats_a_module_dunder_as_a_protocol_hook() {
        // a dunder is registered with the interpreter, not a private helper
        // and not a concept the module owns
        let src = include_str!("../../tests/fixtures/code/sample_several_definitions_dunder_protocol.py");
        let parsed = parse_module(src).unwrap();
        let shape = checks::ModuleShape::python(src, &parsed.syntax().body);
        assert_eq!(shape.functions, 0);
        assert_eq!(shape.framework_functions, 2);
        assert_eq!(shape.definitions(), 1);
        let f = scan_corpus(&[("pkg/registry.py", src)]);
        assert!(!f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
    }

    #[test]
    fn several_definitions_counts_a_module_dunder_that_is_not_a_hook() {
        // only __getattr__/__dir__ are interpreted at module scope; __str__ and
        // __repr__ are names nothing calls, so they stay definitions
        let src = include_str!("../../tests/fixtures/code/sample_several_definitions_dunder_not_a_hook.py");
        let parsed = parse_module(src).unwrap();
        let shape = checks::ModuleShape::python(src, &parsed.syntax().body);
        assert_eq!(shape.functions, 2);
        assert_eq!(shape.framework_functions, 0);
        assert_eq!(shape.definitions(), 3);
        let f = scan_corpus(&[("pkg/registry.py", src)]);
        assert!(f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
    }

    #[test]
    fn several_definitions_reports_the_types_when_types_and_functions_tie() {
        // two classes with behaviour and two helpers: the helpers belong to
        // the classes, so the types arm names them — the functions arm would
        // tell the reader to make the helpers a class instead
        let src = include_str!("../../tests/fixtures/code/sample_several_definitions_tie.py");
        let f = scan_corpus(&[("pkg/marks.py", src)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "several-definitions").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(hits[0].message.contains("2 of them classes"), "{}", hits[0].message);
        assert!(!hits[0].message.contains("2 of them functions"), "{}", hits[0].message);
    }

    #[test]
    fn several_definitions_ignores_nested_classes_and_methods() {
        // one top-level class holding nested ones: the nested classes belong
        // to it, and its methods are the class's, not module definitions
        let src = include_str!(
            "../../tests/fixtures/code/sample_several_definitions_ignores_nested_classes_and_methods__c150302c.py"
        );
        let f = scan_corpus(&[("pkg/outer.py", src)]);
        assert!(!f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
    }

    #[test]
    fn several_definitions_answers_the_script_objection_under_a_nested_scripts_dir() {
        // a script's steps build one program state wherever the script lives —
        // `src/scripts/` is not a different kind of file
        let src = definitions_source(4, 0);
        let f = scan_corpus(&[("src/scripts/steps.py", &src)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "several-definitions").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(
            hits[0].message.contains("A script is not different"),
            "{}",
            hits[0].message
        );
    }

    #[test]
    fn many_definitions_passes_a_long_data_module() {
        // 40 constants and a lookup table: values have no owner and no
        // behaviour, so a data module is not a module full of concepts
        let mut src = String::new();
        for i in 0..40 {
            src.push_str(&format!("LIMIT_{i} = {i}\n"));
        }
        src.push_str("TABLE = {\"a\": 1, \"b\": 2}\n");
        let f = scan_corpus(&[("pkg/constants.py", &src)]);
        assert!(
            !f.iter()
                .any(|x| x.kind == "many-definitions" || x.kind == "several-definitions"),
            "{f:?}"
        );
    }

    #[test]
    fn several_definitions_passes_a_generated_module() {
        let src = format!(
            "# generated by tools/gen_models.py — do not edit\n{}",
            several_classes_source(6, 6)
        );
        let f = scan_corpus(&[("pkg/models.py", &src)]);
        assert!(!f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
    }

    #[test]
    fn a_rust_module_of_function_groups_is_not_told_it_holds_zero_types() {
        // a receiver group plus free functions, and no types of its own. The
        // types arm used to fire anyway and print "0 of them types ()" — the
        // functions arm names the group instead.
        let mut src = String::new();
        for i in 0..9 {
            src.push_str(&format!("pub fn over_expr_{i}(e: &Expr) -> u32 {{\n    {i}\n}}\n\n"));
        }
        for i in 0..4 {
            src.push_str(&format!(
                "pub fn free_{i}(text: &str) -> usize {{\n    text.len() + {i}\n}}\n\n"
            ));
        }
        let rs = rustscan::RustScan::scan(&src, "crate/src/steps.rs", true);
        let scan = Pipeline::rustscan_to_filescan_ref(&rs, "crate/src/steps.rs");
        let f = checks::ModuleShape::findings(&[("crate/src/steps.rs".to_string(), scan.module_shape.clone())]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "several-definitions").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        let msg = &hits[0].message;
        assert!(!msg.contains("0 of them types"), "{msg}");
        assert!(msg.contains("1 function group"), "{msg}");
        assert!(msg.contains("The functions group by the type they take"), "{msg}");
    }

    #[test]
    fn several_definitions_reports_a_rust_type_whose_impl_only_builds_it() {
        // Marker's impl builds markers, so Marker is a record; the third
        // top-level definition still fires
        let src = include_str!("../../tests/fixtures/code/sample_several_definitions_rust_record_constructor.rs");
        let rs = rustscan::RustScan::scan(src, "crate/src/suppressions.rs", true);
        let scan = Pipeline::rustscan_to_filescan_ref(&rs, "crate/src/suppressions.rs");
        let f = checks::ModuleShape::findings(&[("crate/src/suppressions.rs".to_string(), scan.module_shape.clone())]);
        assert!(f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
        // two types whose impls act on their own values are two concerns
        let src = include_str!("../../tests/fixtures/code/sample_several_definitions_rust_two_types.rs");
        let rs = rustscan::RustScan::scan(src, "crate/src/suppressions.rs", true);
        let scan = Pipeline::rustscan_to_filescan_ref(&rs, "crate/src/suppressions.rs");
        let f = checks::ModuleShape::findings(&[("crate/src/suppressions.rs".to_string(), scan.module_shape.clone())]);
        assert!(f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
    }

    #[test]
    fn several_definitions_types_arm_names_the_receiver_groups() {
        // one behavioural type, a foreign receiver group (Expr), and a free
        // function: the types arm speaks, so its message must account for the
        // group as a class equivalent — otherwise the reader cannot check the
        // definition count (#119)
        let src = include_str!("../../tests/fixtures/code/sample_several_definitions_types_arm_names_groups.rs");
        let rs = rustscan::RustScan::scan(src, "crate/src/markers.rs", true);
        let scan = Pipeline::rustscan_to_filescan_ref(&rs, "crate/src/markers.rs");
        let f = checks::ModuleShape::findings(&[("crate/src/markers.rs".to_string(), scan.module_shape.clone())]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "several-definitions").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        let msg = &hits[0].message;
        assert!(msg.contains("The functions group by the type they take"), "{msg}");
        assert!(msg.contains("Expr"), "{msg}");
    }

    #[test]
    fn the_two_rungs_never_fire_together() {
        // ten definitions is the top of the strict rung
        let ten = definitions_source(10, 0);
        let f = scan_corpus(&[("pkg/ten.py", &ten)]);
        assert!(f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "many-definitions"), "{f:?}");
        // eleven is the bottom of the loose rung
        let eleven = definitions_source(11, 0);
        let f = scan_corpus(&[("pkg/eleven.py", &eleven)]);
        assert!(f.iter().any(|x| x.kind == "many-definitions"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
        let msg = &f.iter().find(|x| x.kind == "many-definitions").unwrap().message;
        assert!(
            msg.contains("holds 11 top-level definitions, 11 of them functions"),
            "{msg}"
        );
    }

    #[test]
    fn many_definitions_keeps_the_class_advice_for_classes() {
        let src = several_classes_source(11, 11);
        let f = scan_corpus(&[("pkg/widgets.py", &src)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "many-definitions").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(hits[0].message.contains("11 of them classes"), "{}", hits[0].message);
        assert!(hits[0].message.contains("package"), "{}", hits[0].message);
    }

    #[test]
    fn functions_arm_answers_the_script_objection() {
        // a script's functions build one program state: the message says so
        let src = definitions_source(4, 0);
        let f = scan_corpus(&[("scripts/gen.py", &src)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "several-definitions").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(
            hits[0].message.contains("A script is not different"),
            "{}",
            hits[0].message
        );
        // the same module outside scripts/tools/bin does not get the clause
        let f = scan_corpus(&[("pkg/gen.py", &src)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "several-definitions").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(
            !hits[0].message.contains("A script is not different"),
            "{}",
            hits[0].message
        );
    }

    #[test]
    fn a_framework_contract_module_is_exempt() {
        // every function carries a registration decorator: the framework is
        // the caller, so the module holds no definitions the reader must name
        let mut src = String::new();
        for i in 0..31 {
            src.push_str(&format!(
                "@router.get(\"/v1/thing/{i}\")\nasync def get_thing_{i}(request):\n    return request + {i}\n\n"
            ));
        }
        let f = scan_corpus(&[("pkg/routes.py", &src)]);
        assert!(!f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "many-definitions"), "{f:?}");
        // a module that adds owned functions to its routes still reports them,
        // and the message names the framework entry points it is not counting
        src.push_str(&definitions_source(11, 0));
        let f = scan_corpus(&[("pkg/routes.py", &src)]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "many-definitions").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(
            hits[0].message.contains("11 top-level definitions"),
            "{}",
            hits[0].message
        );
        assert!(
            hits[0]
                .message
                .contains("31 more functions registered with a framework"),
            "{}",
            hits[0].message
        );
    }

    #[test]
    fn rust_types_arm_asks_for_a_directory_of_modules() {
        // 31 structs, no fns: the types arm, and the action is a module split
        let mut src = String::new();
        for i in 0..31 {
            src.push_str(&format!("pub struct Widget{i} {{ size: u32 }}\n\nimpl Widget{i} {{\n    pub fn run(&self) -> u32 {{\n        self.size\n    }}\n}}\n\n"));
        }
        let rs = rustscan::RustScan::scan(&src, "crate/src/widgets.rs", true);
        let scan = Pipeline::rustscan_to_filescan_ref(&rs, "crate/src/widgets.rs");
        let f = checks::ModuleShape::findings(&[("crate/src/widgets.rs".to_string(), scan.module_shape.clone())]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "many-definitions").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        let msg = &hits[0].message;
        assert!(msg.contains("31 of them types"), "{msg}");
        assert!(msg.contains("directory of modules"), "{msg}");
        assert!(msg.contains("one file per type"), "{msg}");
        assert!(!msg.contains("__init__.py"), "{msg}");
    }

    #[test]
    fn rust_functions_arm_splits_the_module_at_the_loose_rung() {
        // 11+ free functions: the module is a grab-bag, so the action is the split
        let mut src = String::new();
        for i in 0..11 {
            src.push_str(&format!(
                "pub fn step_{i}(value: u32) -> u32 {{\n    value + {i}\n}}\n\n"
            ));
        }
        let rs = rustscan::RustScan::scan(&src, "crate/src/steps.rs", true);
        let scan = Pipeline::rustscan_to_filescan_ref(&rs, "crate/src/steps.rs");
        let f = checks::ModuleShape::findings(&[("crate/src/steps.rs".to_string(), scan.module_shape.clone())]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "many-definitions").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        let msg = &hits[0].message;
        assert!(
            msg.contains("holds 11 top-level definitions (11 free functions)"),
            "{msg}"
        );
        assert!(msg.contains("split the module by concern"), "{msg}");
        assert!(msg.contains("one module per family"), "{msg}");
        assert!(msg.contains("`&mut` borrow would conflict"), "{msg}");
        assert!(!msg.contains("directory of modules"), "{msg}");
        assert!(!msg.contains("A script is not different"), "{msg}");
    }

    #[test]
    fn rust_is_judged_at_the_same_rungs_as_python() {
        // four free functions with no receiver: the strict rung fires for Rust
        // too, and its advice is the Rust one (give the functions a type)
        let mut src = String::new();
        for i in 0..4 {
            src.push_str(&format!(
                "pub fn step_{i}(value: u32) -> u32 {{\n    value + {i}\n}}\n\n"
            ));
        }
        let rs = rustscan::RustScan::scan(&src, "crate/src/steps.rs", true);
        let scan = Pipeline::rustscan_to_filescan_ref(&rs, "crate/src/steps.rs");
        let f = checks::ModuleShape::findings(&[("crate/src/steps.rs".to_string(), scan.module_shape.clone())]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "several-definitions").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(
            hits[0]
                .message
                .contains("give them the type they operate on as an impl"),
            "{}",
            hits[0].message
        );
        // and eleven free functions fire the loose rung, with the split advice
        let mut wide = String::new();
        for i in 0..11 {
            wide.push_str(&format!(
                "pub fn step_{i}(value: u32) -> u32 {{\n    value + {i}\n}}\n\n"
            ));
        }
        let rs = rustscan::RustScan::scan(&wide, "crate/src/wide.rs", true);
        let scan = Pipeline::rustscan_to_filescan_ref(&rs, "crate/src/wide.rs");
        let f = checks::ModuleShape::findings(&[("crate/src/wide.rs".to_string(), scan.module_shape.clone())]);
        let hits: Vec<&Finding> = f.iter().filter(|x| x.kind == "many-definitions").collect();
        assert_eq!(hits.len(), 1, "{f:?}");
        assert!(
            hits[0].message.contains("split the module by concern"),
            "{}",
            hits[0].message
        );
    }

    #[test]
    fn rust_type_and_its_functions_count_as_one_definition() {
        // a struct and 12 functions taking it: one class equivalent, so the
        // module holds one definition — not thirteen
        let mut src = String::new();
        for i in 0..12 {
            src.push_str(&format!(
                "pub fn step_{i}(state: &mut State) -> u32 {{\n    state.count + {i}\n}}\n\n"
            ));
        }
        src.push_str("pub struct State {\n    pub count: u32,\n}\n");
        let rs = rustscan::RustScan::scan(&src, "crate/src/steps.rs", true);
        let scan = Pipeline::rustscan_to_filescan_ref(&rs, "crate/src/steps.rs");
        assert_eq!(scan.module_shape.definitions(), 1, "{:?}", scan.module_shape);
        let f = checks::ModuleShape::findings(&[("crate/src/steps.rs".to_string(), scan.module_shape.clone())]);
        assert!(f.is_empty(), "{f:?}");
        // the same functions with no receiver are 31 definitions, and report
        let mut loose = String::new();
        for i in 0..31 {
            loose.push_str(&format!(
                "pub fn step_{i}(value: u32) -> u32 {{\n    value + {i}\n}}\n\n"
            ));
        }
        let rs = rustscan::RustScan::scan(&loose, "crate/src/loose.rs", true);
        let scan = Pipeline::rustscan_to_filescan_ref(&rs, "crate/src/loose.rs");
        assert_eq!(scan.module_shape.definitions(), 31);
        let f = checks::ModuleShape::findings(&[("crate/src/loose.rs".to_string(), scan.module_shape.clone())]);
        assert!(f.iter().any(|x| x.kind == "many-definitions"), "{f:?}");
    }

    #[test]
    fn rust_records_still_count_as_definitions() {
        // one type with an impl plus two plain records is three definitions;
        // only a Rust class equivalent (type + impl/functions) is grouped
        let mut src = String::new();
        for i in 0..2 {
            src.push_str(&format!("pub struct Row{i} {{ pub size: u32 }}\n\n"));
        }
        src.push_str(include_str!("../../tests/fixtures/code/sample_rows__a49042a0.py"));
        let rs = rustscan::RustScan::scan(&src, "crate/src/table.rs", true);
        let scan = Pipeline::rustscan_to_filescan_ref(&rs, "crate/src/table.rs");
        let f = checks::ModuleShape::findings(&[("crate/src/table.rs".to_string(), scan.module_shape.clone())]);
        assert!(f.iter().any(|x| x.kind == "several-definitions"), "{f:?}");
    }

    #[test]
    fn module_shape_reads_only_the_module_body() {
        let src = include_str!("../../tests/fixtures/code/sample_module_shape_reads_only_the_module_body__10bd4179.py");
        let parsed = parse_module(src).unwrap();
        let shape = checks::ModuleShape::python(src, &parsed.syntax().body);
        assert_eq!(shape.definitions(), 2); // the class and the function; not the method or the constant
        assert_eq!(shape.classes.len(), 1);
        assert_eq!(shape.classes[0].name, "Widget");
        assert_eq!(shape.classes[0].methods, 1);
        assert_eq!(shape.functions, 1);
        assert!(!shape.generated);
    }

    #[test]
    fn conditional_polymorphism_dispatch_detected() {
        let src = include_str!("../../tests/fixtures/rust/conditional_polymorphism_dispatch_detected__01.py");
        let f = scan_src(src);
        assert!(f.iter().any(|x| x.kind == "conditional-polymorphism"));
        let ok = scan_src(include_str!(
            "../../tests/fixtures/rust/conditional_polymorphism_dispatch_detected__02.py"
        ));
        assert!(!ok.iter().any(|x| x.kind == "conditional-polymorphism")); // mixed keys
    }

    #[test]
    fn special_case_skips_fail_fast_raise_guards() {
        // review log §2.6: `person is None -> raise KeyError` guards are NOT
        // special-case candidates — the absence is an error, no object can
        // replace it (the fix suggestion would mask the error)
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/special_case_skips_fail_fast_raise_guards__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "special-case"), "{:?}", f);
        // control: repeated VALUE-style null handling still fires
        let ok = scan_src(include_str!(
            "../../tests/fixtures/rust/special_case_skips_fail_fast_raise_guards__02.py"
        ));
        assert!(ok.iter().any(|x| x.kind == "special-case"));
        // an ERROR-return guard (mine is None -> 401 JSONResponse) is a
        // boundary, not a special case — a NullObject would hide the 401
        let guard = scan_src(include_str!(
            "../../tests/fixtures/rust/special_case_skips_fail_fast_raise_guards__03.py"
        ));
        assert!(!guard.iter().any(|x| x.kind == "special-case"), "{:?}", guard);
    }

    #[test]
    fn long_param_list_skips_trivial_framework_stub() {
        // review: urllib's redirect_request override — 6 protocol params, a
        // one-statement stub; a parameter object would BREAK the override.
        // A trivial stub is a protocol placeholder, not a param-list smell.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/long_param_list_skips_trivial_framework_stub__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "long-param-list"), "{:?}", f);
        // control: a real 6-param fn with a body still fires
        let ok = scan_src(include_str!(
            "../../tests/fixtures/rust/long_param_list_skips_trivial_framework_stub__02.py"
        ));
        assert!(ok.iter().any(|x| x.kind == "long-param-list"), "{:?}", ok);
    }

    #[test]
    fn python_fn_shape_classifies_dispatch_and_rules() {
        // the shape classifier behind the complexity message: a chain of
        // ifs over the same selector is a dispatch chain; a battery of ifs
        // appending to the same list is a rules battery
        let dispatch = include_str!("../../tests/fixtures/rust/python_fn_shape_classifies_dispatch_and_rules__01.py");
        let (shape, detail) = FnShape::python_fn_shape(&first_py_fn_body(dispatch));
        assert_eq!((shape, detail.as_str()), ("dispatch", "sel"));
        let rules = include_str!("../../tests/fixtures/rust/python_fn_shape_classifies_dispatch_and_rules__02.py");
        let (shape, detail) = FnShape::python_fn_shape(&first_py_fn_body(rules));
        assert_eq!((shape, detail.as_str()), ("rules", "out"));
        let plain = include_str!("../../tests/fixtures/rust/python_fn_shape_classifies_dispatch_and_rules__03.py");
        let (shape, _) = FnShape::python_fn_shape(&first_py_fn_body(plain));
        assert_eq!(shape, "plain");
        // the OFFER equals the FIX: a battery the rule-table fix cannot
        // apply to (a check with a nested if — not a single append) is NOT
        // routed to rule-table; it is "plain" so the message offers
        // extract-method instead (no false directive)
        let unfixable = include_str!("../../tests/fixtures/rust/python_fn_shape_classifies_dispatch_and_rules__04.py");
        let (shape, _) = FnShape::python_fn_shape(&first_py_fn_body(unfixable));
        assert_eq!(shape, "plain", "an unhoistable battery must not be offered rule-table");
        // a dispatch with an elif is not the fixable shape
        let unfixable_d =
            include_str!("../../tests/fixtures/rust/python_fn_shape_classifies_dispatch_and_rules__05.py");
        let (shape, _) = FnShape::python_fn_shape(&first_py_fn_body(unfixable_d));
        assert_eq!(shape, "plain", "an elif dispatch must not be offered dispatch-registry");
    }

    #[test]
    fn special_case_repeated_none_checks() {
        let src = include_str!("../../tests/fixtures/rust/special_case_repeated_none_checks__01.py");
        let f = scan_src(src);
        assert!(f.iter().any(|x| x.kind == "special-case"));
        let ok = scan_src(include_str!(
            "../../tests/fixtures/rust/special_case_repeated_none_checks__02.py"
        ));
        assert!(!ok.iter().any(|x| x.kind == "special-case"));
    }

    #[test]
    fn special_case_nested_function_checks_are_not_added_to_the_parent() {
        // the reviewer's question: a nested def INSIDE an if body — the outer
        // counter walks if bodies but must stop at the def, and the scope walk
        // counts the inner checks once. 2 + 2 is not "4 repeated checks".
        let src = include_str!("../../tests/fixtures/code/sample_special_case_nested_function_checks_are_not_added_to_the_parent__bfdc638a.py");
        let f = scan_src(src);
        assert!(!f.iter().any(|x| x.kind == "special-case"), "{f:?}");
    }

    #[test]
    fn special_case_counts_within_one_function_only() {
        // the same local name in two functions is two unrelated values: two
        // checks in each is not "3 repeated checks on one value" (a file-wide
        // count fired on common names like `found` and `callee`)
        let split =
            include_str!("../../tests/fixtures/code/sample_special_case_counts_within_one_function_only__cfa68b87.py");
        let f = scan_src(split);
        assert!(!f.iter().any(|x| x.kind == "special-case"), "{f:?}");
        // three checks on ONE value in one function still fires
        let repeated =
            include_str!("../../tests/fixtures/code/sample_special_case_counts_within_one_function_only__1388ac62.py");
        let hit = scan_src(repeated);
        assert!(hit
            .iter()
            .any(|x| x.kind == "special-case" && x.message.contains("3 repeated")));
    }

    #[test]
    fn middle_man_delegation_detected() {
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/middle_man_delegation_detected__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "middle-man"));
        let ok = scan_src(include_str!(
            "../../tests/fixtures/rust/middle_man_delegation_detected__02.py"
        ));
        assert!(!ok.iter().any(|x| x.kind == "middle-man"));
    }

    #[test]
    fn middle_man_resolves_a_target_in_another_file() {
        // one class per file is the house convention, so the target of a
        // delegation usually lives in ANOTHER file — a file-local index would
        // read it as outside the codebase and silently drop the finding
        let f = scan_corpus(&[
            (
                "inner.py",
                include_str!(
                    "../../tests/fixtures/code/sample_middle_man_resolves_a_target_in_another_file__0e8ba5f5.py"
                ),
            ),
            (
                "wrapper.py",
                include_str!(
                    "../../tests/fixtures/code/sample_middle_man_resolves_a_target_in_another_file__961a8812.py"
                ),
            ),
        ]);
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "middle-man").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert_eq!(r[0].file, "wrapper.py");
        assert_eq!(r[0].line, 5);
        // and the library case stays silent: no class of that name anywhere
        let g = scan_corpus(&[(
            "boundary.py",
            include_str!("../../tests/fixtures/code/sample_middle_man_resolves_a_target_in_another_file__42080549.py"),
        )]);
        assert!(!g.iter().any(|x| x.kind == "middle-man"), "{g:?}");
    }

    #[test]
    fn middle_man_single_buffer_is_conservative() {
        // the LSP's typing path sees one buffer and no repo-wide pass, so a
        // target in another file is invisible there: the rule stays silent
        // rather than guessing, and fires once the target is in the buffer
        let wrapper =
            include_str!("../../tests/fixtures/code/sample_middle_man_resolves_a_target_in_another_file__961a8812.py");
        let alone = Pipeline::scan(wrapper, "wrapper.py", false);
        assert!(
            !alone.findings.iter().any(|f| f.kind == "middle-man"),
            "{:?}",
            alone.findings
        );
        let together = format!("class Inner:\n    def render(self) -> str:\n        return \"x\"\n\n\n{wrapper}");
        let both = Pipeline::scan(&together, "wrapper.py", false);
        assert!(
            both.findings.iter().any(|f| f.kind == "middle-man"),
            "{:?}",
            both.findings
        );
    }

    #[test]
    fn middle_man_exempts_the_encapsulation_shapes() {
        // Issue #44: four shapes where "call the target directly" is not
        // advice anyone can take — the delegated member is a library's method,
        // the method is an interface member, the call fixes an argument the
        // target defaults, or the attribute has no type evidence at all.
        for src in [
            include_str!("../../tests/fixtures/rust/middle_man_library_target_passes__01.py"),
            include_str!("../../tests/fixtures/rust/middle_man_protocol_conformance_passes__01.py"),
            include_str!("../../tests/fixtures/rust/middle_man_policy_argument_passes__01.py"),
            include_str!("../../tests/fixtures/rust/middle_man_unknown_target_passes__01.py"),
        ] {
            let f = scan_src(src);
            assert!(!f.iter().any(|x| x.kind == "middle-man"), "{f:?}");
        }
    }

    #[test]
    fn middle_man_keeps_the_delegation_this_codebase_can_answer() {
        // the rule's own case: the target class is here, the call adds nothing
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/middle_man_real_delegation_fires__01.py"
        ));
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "middle-man").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert!(r[0].message.contains("Remove Middle Man"), "{}", r[0].message);
        // a name collision with an interface member is not conformance: this
        // wrapper does not accept the interface's parameter
        let g = scan_src(include_str!(
            "../../tests/fixtures/rust/middle_man_interface_arity_mismatch_fires__01.py"
        ));
        assert!(g.iter().any(|x| x.kind == "middle-man"), "{g:?}");
    }

    #[test]
    fn middle_man_reads_the_metaclass_spelling_of_an_interface() {
        // `class X(ABC)` and `class X(metaclass=ABCMeta)` declare the same
        // thing, so the interface exemption must read both — otherwise the
        // wrapper of a conforming implementation is told to remove the
        // middle man (the pre-fix reading: 1 finding).
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/middle_man_metaclass_interface_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "middle-man"), "{f:?}");
        // a metaclass that is not ABCMeta declares no interface: the same
        // shape still fires
        let g = scan_src(include_str!(
            "../../tests/fixtures/rust/middle_man_non_interface_metaclass_fires__01.py"
        ));
        assert!(g.iter().any(|x| x.kind == "middle-man"), "{g:?}");
    }

    #[test]
    fn middle_man_interface_evidence_lives_with_the_class_it_serves() {
        // Issue #65: the exemption is the class's OWN contract. A Protocol in
        // the wrapper's own module is that module's seam (the #44 shape, and
        // #52's "the interface lives with the class it serves"); a Protocol in
        // an unrelated module that nothing names says nothing about this
        // class, so the real delegation fires again.
        let f = scan_corpus(&[
            (
                "case2_adapter.py",
                include_str!("../../tests/fixtures/rust/middle_man_protocol_conformance_passes__01.py"),
            ),
            (
                "case4_real_middle_man.py",
                include_str!("../../tests/fixtures/rust/middle_man_real_delegation_fires__01.py"),
            ),
        ]);
        let r: Vec<&Finding> = f.iter().filter(|x| x.kind == "middle-man").collect();
        assert_eq!(r.len(), 1, "{f:?}");
        assert_eq!(r[0].file, "case4_real_middle_man.py");
    }

    #[test]
    fn middle_man_interface_named_as_a_base_exempts_across_modules() {
        // the other evidence the class owns: it names the interface. That is
        // a statement about the class, wherever the interface is declared.
        let f = scan_corpus(&[
            (
                "protocols.py",
                include_str!("../../tests/fixtures/code/sample_middle_man_interface_named_as_a_base_exempts_across_modules__198f2a68.py"),
            ),
            (
                "adapter.py",
                include_str!("../../tests/fixtures/code/sample_middle_man_interface_named_as_a_base_exempts_across_modules__29f5b30c.py"),
            ),
        ]);
        assert!(!f.iter().any(|x| x.kind == "middle-man"), "{f:?}");
    }

    #[test]
    fn middle_man_skips_the_language_invoked_operators() {
        // `__add__` is invoked by `a + b`: no call site exists where "call the
        // target directly" can be taken, so the operator is the class's
        // contract, not a forwarding layer.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/middle_man_dunder_delegation_passes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "middle-man"), "{f:?}");
        // the exemption is the operator naming, not the helper: the same
        // helper called from an ordinary method is still a delegation
        let g = scan_src(include_str!(
            "../../tests/fixtures/code/sample_middle_man_skips_the_language_invoked_operators__31985a5e.py"
        ));
        assert!(g.iter().any(|x| x.kind == "middle-man"), "{g:?}");
    }

    #[test]
    fn unused_setter_detected() {
        let f = scan_src(include_str!("../../tests/fixtures/rust/unused_setter_detected__01.py"));
        assert!(f.iter().any(|x| x.kind == "unused-setter"));
        let ok = scan_src(include_str!("../../tests/fixtures/rust/unused_setter_detected__02.py"));
        assert!(!ok.iter().any(|x| x.kind == "unused-setter"));
    }

    #[test]
    fn setter_test_only_reference_gets_seam_message_never_referenced_gets_delete() {
        // a setter referenced only from tests is DEAD code wearing a harness —
        // the message must teach that and name the imminent-caller escape; a
        // setter with zero references anywhere gets the delete message
        let f = scan_corpus(&[
            ("app.py", include_str!("../../tests/fixtures/rust/setter_test_only_reference_gets_seam_message_never_referenced_gets_delete__01.py")),
            ("tests/unit/test_app.py", include_str!("../../tests/fixtures/rust/setter_test_only_reference_gets_seam_message_never_referenced_gets_delete__02.py")),
        ]);
        let seam: Vec<&Finding> = f
            .iter()
            .filter(|x| x.kind == "unused-setter" && x.message.contains("set_scheduler"))
            .collect();
        assert_eq!(seam.len(), 1, "{f:?}");
        assert!(
            seam[0].message.contains("referenced only from tests"),
            "seam message must name the test-only status: {}",
            seam[0].message
        );
        assert!(
            seam[0].message.contains("write the calling code"),
            "imminent-caller escape must be named: {}",
            seam[0].message
        );
        let dead: Vec<&Finding> = f
            .iter()
            .filter(|x| x.kind == "unused-setter" && x.message.contains("set_cache"))
            .collect();
        assert_eq!(dead.len(), 1, "{f:?}");
        assert!(dead[0].message.contains("never referenced"), "{}", dead[0].message);
    }

    #[test]
    fn loop_pipeline_detected() {
        let f = scan_src(include_str!("../../tests/fixtures/rust/loop_pipeline_detected__01.py"));
        assert!(f.iter().any(|x| x.kind == "loop-pipeline"));
        let ok = scan_src(include_str!("../../tests/fixtures/rust/loop_pipeline_detected__02.py"));
        assert!(!ok.iter().any(|x| x.kind == "loop-pipeline"));
    }

    #[test]
    fn loop_pipeline_skips_async_functions() {
        // Phase 3: an async function's loops carry no loop-pipeline finding —
        // the recipe's comprehension cannot prove the awaits belong inside an
        // async comprehension. The sync pure-build shape still fires.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/loop_pipeline_async_emits_nothing__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "loop-pipeline"), "{f:?}");
        let sync = scan_src(include_str!(
            "../../tests/fixtures/code/sample_loop_pipeline_skips_async_functions__b06cc3e1.py"
        ));
        assert!(sync.iter().any(|x| x.kind == "loop-pipeline"), "{sync:?}");
    }

    #[test]
    fn loop_pipeline_gate_excludes_mutation_of_existing_collections() {
        // P6: the pipeline gate checks the target's binding — a loop that
        // mutates a parameter (fill), a name bound earlier with content
        // (apply), or a module name bound before the function (update)
        // builds nothing new and emits NOTHING. The pure-build shape (a
        // fresh name or one created empty in the statement before the
        // loop) still fires.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/loop_mutation_of_existing_collection_emits_nothing__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "loop-pipeline"), "{f:?}");
        let build = scan_src(include_str!("../../tests/fixtures/rust/loop_pipeline_detected__01.py"));
        assert!(build.iter().any(|x| x.kind == "loop-pipeline"), "{build:?}");
    }

    #[test]
    fn python_loop_pipeline_needs_a_plain_fresh_target_without_else() {
        // issue #120: a chained receiver has no target, a for-else is not a
        // comprehension, and a loop that reads the collection it grows is a
        // traversal. The module-level group-by also pins the MODULE walker:
        // its target is not a plain name, so it must not fire either.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/loop_pipeline_python_own_build__01.py"
        ));
        let pipes: Vec<&Finding> = f.iter().filter(|x| x.kind == "loop-pipeline").collect();
        assert_eq!(pipes.len(), 1, "{f:?}");
        assert_eq!(pipes[0].function, "plain_build", "{f:?}");
        assert!(pipes[0].message.contains("fix:"), "{}", pipes[0].message);
    }

    #[test]
    fn loop_dict_build_subscript_store_still_fires() {
        // P5: the pure dict-build shape (eval_context.py:42 —
        // `result[k] = f(v)` in one per-item pass) IS a comprehension in
        // disguise — loop-pipeline still fires.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/loop_dict_build_subscript_store_is_found__01.py"
        ));
        let p: Vec<&Finding> = f.iter().filter(|x| x.kind == "loop-pipeline").collect();
        assert_eq!(p.len(), 1, "{f:?}");
        assert!(p[0].message.contains("use a comprehension"), "{}", p[0].message);
        assert!(!f.iter().any(|x| x.kind == "loop-hoist"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "mutating-loop"), "{f:?}");
    }

    #[test]
    fn loop_pump_and_polling_shapes_emit_nothing() {
        // P5: a PriorityQueue drain (pop/append/requeue) and a polling
        // sleep-schedule are not comprehension shapes — no loop finding at
        // all, no fold advice, no sequence verdict.
        let pump = scan_src(include_str!(
            "../../tests/fixtures/rust/loop_pump_priority_queue_drain_emits_nothing__01.py"
        ));
        assert!(!pump.iter().any(|x| x.kind == "loop-pipeline"), "{pump:?}");
        assert!(!pump.iter().any(|x| x.kind == "loop-hoist"), "{pump:?}");
        assert!(!pump.iter().any(|x| x.kind == "mutating-loop"), "{pump:?}");
        let poll = scan_src(include_str!(
            "../../tests/fixtures/rust/loop_polling_sleep_emits_nothing__01.py"
        ));
        assert!(!poll.iter().any(|x| x.kind == "loop-pipeline"), "{poll:?}");
        assert!(!poll.iter().any(|x| x.kind == "loop-hoist"), "{poll:?}");
        assert!(!poll.iter().any(|x| x.kind == "mutating-loop"), "{poll:?}");
        assert!(!poll.iter().any(|x| x.kind == "loop-sequence"), "{poll:?}");
    }

    #[test]
    fn loop_shared_accumulator_passes_each_fire_pipeline_no_sequence_verdict() {
        // P5: the sequence judgement is gone — a loop either reduces to a
        // comprehension (loop-pipeline) or emits nothing. Two `chunks +=
        // [p]` loops are each a comprehension shape and still fire as
        // pipelines; there is no loop-sequence verdict on top.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/loop_sequence_shared_accumulator_is_a_pipeline__01.py"
        ));
        let pipes: Vec<&Finding> = f.iter().filter(|x| x.kind == "loop-pipeline").collect();
        assert_eq!(pipes.len(), 2, "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "loop-sequence"), "{f:?}");
    }

    #[test]
    fn loop_sequences_emit_only_the_pipeline_shape() {
        // P5: independent appended loops each fire loop-pipeline; the
        // sequence extraction advice is gone, and a fold rebind (latest-
        // wins with per-iteration temps) emits nothing at all.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/loop_sequence_independent_passes_get_extraction_advice__01.py"
        ));
        assert_eq!(f.iter().filter(|x| x.kind == "loop-pipeline").count(), 2, "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "loop-sequence"), "{f:?}");
        let t = scan_src(include_str!(
            "../../tests/fixtures/rust/loop_sequence_independent_passes_get_extraction_advice__02.py"
        ));
        assert!(!t.iter().any(|x| x.kind == "loop-pipeline"), "{t:?}");
        assert!(!t.iter().any(|x| x.kind == "loop-hoist"), "{t:?}");
        assert!(!t.iter().any(|x| x.kind == "mutating-loop"), "{t:?}");
    }
    #[test]
    fn multi_accumulator_loops_emit_nothing() {
        // P5: a loop writing three accumulators is stateful, not a
        // comprehension — no mutating-loop verdict and no fold advice; the
        // single reduction emits nothing too.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/mutating_loop_requires_two_or_more_surviving_state_writes__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "mutating-loop"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "loop-pipeline"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "loop-hoist"), "{f:?}");
        let ok = scan_src(include_str!(
            "../../tests/fixtures/rust/mutating_loop_requires_two_or_more_surviving_state_writes__02.py"
        ));
        assert!(!ok.iter().any(|x| x.kind == "mutating-loop"), "{ok:?}");
        assert!(!ok.iter().any(|x| x.kind == "loop-hoist"), "{ok:?}");
        assert!(!ok.iter().any(|x| x.kind == "loop-pipeline"), "{ok:?}");
    }
    #[test]
    fn loop_feed_chain_loops_each_fire_pipeline_no_sequence_verdict() {
        // P5: the second pass consumes the first pass's output — each loop
        // is itself a comprehension shape and fires as pipeline; the feed
        // chain verdict is gone.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/loop_sequence_feed_chain_is_a_pipeline__01.py"
        ));
        assert_eq!(f.iter().filter(|x| x.kind == "loop-pipeline").count(), 2, "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "loop-sequence"), "{f:?}");
    }

    #[test]
    fn nested_loop_is_one_pass_not_a_sequence() {
        // a nested loop is a single pass with a cartesian product, not a
        // sequence of passes
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/nested_loop_is_one_pass_not_a_sequence__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "loop-sequence"), "{f:?}");
        // two separate single-loop functions are not a sequence either
        let g = scan_src(include_str!(
            "../../tests/fixtures/rust/nested_loop_is_one_pass_not_a_sequence__02.py"
        ));
        assert!(!g.iter().any(|x| x.kind == "loop-sequence"), "{g:?}");
    }

    #[test]
    fn module_level_loops_get_only_the_single_loop_pipeline_shape() {
        // module-level loops get the pipeline shape; the mutating/sequence
        // variants stay function-scoped (no enclosing function context)
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/module_level_loops_get_only_the_single_loop_pipeline_shape__01.py"
        ));
        assert!(f.iter().any(|x| x.kind == "loop-pipeline"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "loop-sequence"), "{f:?}");
        let g = scan_src(include_str!(
            "../../tests/fixtures/rust/module_level_loops_get_only_the_single_loop_pipeline_shape__02.py"
        ));
        assert!(!g.iter().any(|x| x.kind == "mutating-loop"), "{g:?}");
        assert!(!g.iter().any(|x| x.kind == "loop-sequence"), "{g:?}");
    }

    #[test]
    fn non_pipeline_loop_bodies_emit_nothing() {
        // P5: a long per-item calculation feeding one accumulator is NOT a
        // comprehension shape — no hoist advice, no fold; only the pure
        // append loop still fires, as loop-pipeline.
        let f = scan_src(include_str!(
            "../../tests/fixtures/rust/loop_hoist_fires_on_single_mutation_long_body_loops__01.py"
        ));
        assert!(!f.iter().any(|x| x.kind == "loop-hoist"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "loop-pipeline"), "{f:?}");
        let g = scan_src(include_str!(
            "../../tests/fixtures/rust/loop_hoist_fires_on_single_mutation_long_body_loops__02.py"
        ));
        assert!(!g.iter().any(|x| x.kind == "loop-hoist"), "{g:?}");
        assert!(!g.iter().any(|x| x.kind == "loop-pipeline"), "{g:?}");
        let p = scan_src(include_str!(
            "../../tests/fixtures/rust/loop_hoist_fires_on_single_mutation_long_body_loops__03.py"
        ));
        assert!(!p.iter().any(|x| x.kind == "loop-hoist"), "{p:?}");
        assert!(p.iter().any(|x| x.kind == "loop-pipeline"), "{p:?}");
    }

    #[test]
    fn multi_statement_accumulator_loop_emits_nothing() {
        // P5: the old B7 hoist shape — a multi-statement body feeding one
        // member accumulator — is not a comprehension; nothing emits (the
        // member-name render belonged to the removed hoist advice).
        let src = include_str!(
            "../../tests/fixtures/code/sample_multi_statement_accumulator_loop_emits_nothing__3a8dcbef.py"
        );
        let f = scan_src(src);
        assert!(!f.iter().any(|x| x.kind == "loop-hoist"), "{f:?}");
        assert!(!f.iter().any(|x| x.kind == "loop-pipeline"), "{f:?}");
    }

    #[test]
    fn latent_visitor_detected_and_claims_chains() {
        // two operations over the same element family — the visitor shape
        let src = include_str!("../../tests/fixtures/rust/latent_visitor_detected_and_claims_chains__01.py");
        let f = scan_src(src);
        assert!(f.iter().any(|x| x.kind == "latent-visitor"), "expected latent-visitor");
        // the claimed chains must NOT also be polymorphism — one ruling per chain
        assert!(
            !f.iter().any(|x| x.kind == "conditional-polymorphism"),
            "claimed chains must be exempt from conditional-polymorphism"
        );
    }

    #[test]
    fn single_dispatch_stays_polymorphism() {
        // ONE operation over the family (4-arm chain) — polymorphic methods,
        // not a visitor (a single 2-arm type check is idiomatic, no finding)
        let src = include_str!("../../tests/fixtures/rust/single_dispatch_stays_polymorphism__01.py");
        let f = scan_src(src);
        assert!(f.iter().any(|x| x.kind == "conditional-polymorphism"));
        assert!(!f.iter().any(|x| x.kind == "latent-visitor"));
    }

    #[test]
    fn value_dispatch_is_not_a_visitor() {
        // x == 1/2/3/4 is value dispatch, not a type-tag visitor
        let src = include_str!("../../tests/fixtures/rust/value_dispatch_is_not_a_visitor__01.py");
        let f = scan_src(src);
        assert!(f.iter().any(|x| x.kind == "conditional-polymorphism"));
        assert!(!f.iter().any(|x| x.kind == "latent-visitor"));
    }

    #[test]
    fn implemented_visitor_is_not_flagged() {
        // the real visitor: accept on the elements + visit_* on the visitor —
        // no dispatch chain remains, so nothing new fires (no thrash)
        let src = include_str!("../../tests/fixtures/rust/implemented_visitor_is_not_flagged__01.py");
        let f = scan_src(src);
        assert!(!f.iter().any(|x| x.kind == "latent-visitor"));
        assert!(!f.iter().any(|x| x.kind == "conditional-polymorphism"));
    }

    #[test]
    fn registry_is_complete() {
        // every emitted kind has a final_kind arm OR is deliberately standard
        for &k in crate::FAMILY_KINDS {
            let display = crate::final_kind(k);
            assert!(
                display != "standard" || crate::STANDARD_KINDS.contains(&k),
                "kind '{k}' has no final_kind arm and is not in STANDARD_KINDS — register it"
            );
        }
        // the standard list is a strict subset of the family list
        for &k in crate::STANDARD_KINDS {
            assert!(
                crate::FAMILY_KINDS.contains(&k),
                "standard kind '{k}' is not in FAMILY_KINDS"
            );
        }
        // no kind is both named and standard
        let mut named = Vec::new();
        for &k in crate::FAMILY_KINDS {
            if crate::final_kind(k) != "standard" {
                named.push(k);
            }
        }
        for &k in crate::STANDARD_KINDS {
            assert!(
                !named.contains(&k),
                "kind '{k}' has a final_kind arm and is also STANDARD_KINDS — pick one"
            );
        }
    }
}
