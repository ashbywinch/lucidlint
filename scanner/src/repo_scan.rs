//! The repo-wide scan: every file's scan gathered into the pools the repo-wide
//! rules read, and each rule asked once over them.
//!
//! The expensive part of a repo-wide run is the per-file scan, so this layer is
//! pure and in-memory: the LSP reuses cached scans and re-runs only this. Each
//! family's finding is filed under the file it names and passed through that
//! file's own `lucidlint: ignore` comments.

use std::collections::{HashMap, HashSet};

use crate::checks;
use crate::file_scan::{Definition, FileScan, ImportInfo, Setter, SpentMap, SuppressionsMap, UndeclScan};
use crate::finding::Finding;
use crate::paths::Paths;
use crate::skeleton::SkeletonFn;

/// The names the repo's code reads, split by where they are read. `unused`
/// treats a name reached only from tests differently from one nothing reaches,
/// and only a production reference keeps a definition alive.
struct References {
    /// Read from non-test code.
    production: HashSet<String>,
    /// Read only from test files.
    tests: HashSet<String>,
    /// Production string literals: a name in a string is a CLI dispatch, a
    /// registry key, or a framework hook, so `unused` counts it as a reference.
    strings: Vec<String>,
}

/// What each file's own `lucidlint: ignore` comments leave, and what the
/// per-file pass already spent — the ledger the repo-wide findings pass through.
#[derive(Default)]
struct SuppressionLedger {
    remaining: SuppressionsMap,
    spent: SpentMap,
}

/// The `forwarding` rule's pools: the methods and functions whose body only
/// forwards, each file's module-level methods, and each file's imports (a
/// re-export is the boundary the rule refuses to cross).
#[derive(Default)]
struct Forwarders {
    methods: Vec<checks::ForwarderMethod>,
    functions: Vec<checks::ForwarderFn>,
    repo_methods: Vec<checks::RepoMethod>,
    imports: HashMap<String, Vec<ImportInfo>>,
}

/// Every file's scan, gathered into the pools the repo-wide rules read. Built
/// once per repo-wide run; holding the scans lets the per-file families be
/// resolved without walking the filesystem again.
pub struct RepoScan<'a> {
    /// The per-file scans, in source order — the per-file families' inputs.
    scans: &'a [FileScan],
    /// Each file's repo-relative path, in the same order.
    rels: Vec<String>,
    /// The `duplicate` rule's pool: every function's shape.
    skeletons: Vec<SkeletonFn>,
    /// The `unused` rule's pool: every definition site.
    definitions: Vec<Definition>,
    /// The `unused` rule's second pool: every setter method.
    setters: Vec<Setter>,
    references: References,
    suppressions: SuppressionLedger,
    /// The `duplicate-module` rule's pool: every module's shape.
    modules: Vec<checks::SkeletonModule>,
    forwarders: Forwarders,
}

impl<'a> RepoScan<'a> {
    /// Every finding kind the corpus computes — the families deferred to the
    /// save-time merge, so a per-buffer scan must not judge their markers
    /// stale. A new family is added here and in [`RepoScan::reconcile`]
    /// together.
    pub const FAMILIES: &'static [&'static str] = &[
        "duplicate",
        "duplicate-expression",
        "duplicate-module",
        "forwarding-chain",
        "many-definitions",
        "middle-man",
        "over-abstraction",
        "scattered-instance",
        "several-definitions",
        "shared-path",
        "sibling-value-class",
        "undeclared-attribute",
        "unused",
        "unused-setter",
    ];

    /// Gather the scans into the pools every repo-wide rule reads.
    pub fn of(scans: &'a [FileScan], root: &str) -> Self {
        let rels: Vec<String> = scans.iter().map(|scan| scan.rel_of(root)).collect();
        Self::gather(scans, rels)
    }

    /// The corpus over scans that already carry their repo-relative names —
    /// the in-memory test corpora, whose paths have no root to strip.
    #[cfg(test)]
    pub(crate) fn of_names(scans: &'a [FileScan]) -> Self {
        let rels = scans.iter().map(|scan| scan.file_name.clone()).collect();
        Self::gather(scans, rels)
    }

    /// The corpus over the given per-file rels.
    fn gather(scans: &'a [FileScan], rels: Vec<String>) -> Self {
        let skeletons = scans
            .iter()
            .zip(&rels)
            .flat_map(|(scan, rel)| scan.skeleton_rows(rel))
            .collect();
        let definitions = scans
            .iter()
            .zip(&rels)
            .flat_map(|(scan, rel)| scan.definition_rows(rel))
            .collect();
        let setters = scans
            .iter()
            .zip(&rels)
            .flat_map(|(scan, rel)| scan.setter_rows(rel))
            .collect();
        // what the code reads, split by the files it is read in
        let read_in = |keep: &dyn Fn(&str) -> bool| -> HashSet<String> {
            scans
                .iter()
                .zip(&rels)
                .filter(|(_, rel)| keep(rel))
                .flat_map(|(scan, _)| scan.refs.iter().cloned())
                .collect()
        };
        let references = References {
            production: read_in(&|rel| !Paths::is_test_path(rel))
                .into_iter()
                .chain(scans.iter().flat_map(|scan| scan.decorated.iter().cloned()))
                .collect(),
            tests: read_in(&Paths::is_test_path),
            strings: scans
                .iter()
                .zip(&rels)
                .filter(|(_, rel)| !Paths::is_test_path(rel))
                .flat_map(|(scan, _)| scan.strings.iter().cloned())
                .collect(),
        };
        // what each file's own ignore comments leave, and what they already spent
        let (remaining, spent) = scans
            .iter()
            .zip(&rels)
            .map(|(scan, rel)| {
                (
                    (rel.clone(), scan.supps.clone()),
                    (rel.clone(), scan.supps_spent.clone()),
                )
            })
            .unzip();
        let forwarders = Forwarders {
            methods: scans
                .iter()
                .flat_map(|scan| scan.forwarder_methods.iter().cloned())
                .collect(),
            functions: scans
                .iter()
                .flat_map(|scan| scan.forwarder_fns.iter().cloned())
                .collect(),
            repo_methods: scans
                .iter()
                .flat_map(|scan| scan.repo_methods.iter().cloned())
                .collect(),
            imports: scans
                .iter()
                .zip(&rels)
                .map(|(scan, rel)| (rel.clone(), scan.imports.clone()))
                .collect(),
        };
        Self {
            references,
            suppressions: SuppressionLedger { remaining, spent },
            modules: scans
                .iter()
                .flat_map(|scan| scan.skeleton_modules.iter().cloned())
                .collect(),
            forwarders,
            scans,
            rels,
            skeletons,
            definitions,
            setters,
        }
    }

    /// Every repo-wide finding, each file's suppressions applied, grouped by the
    /// file it is filed under.
    pub fn by_file(&self) -> HashMap<String, Vec<Finding>> {
        Self::grouped(self.findings())
    }

    /// Every repo-wide finding, each file's suppressions applied. Each family is
    /// filtered on its own — a marker's innermost-peel capacity is computed over
    /// the findings of the family it was written for.
    pub fn findings(&self) -> Vec<Finding> {
        let mut findings = Vec::new();
        self.reconcile(&mut findings);
        findings
    }

    /// Append every repo-wide family to `findings`, each through the file's own
    /// suppressions. The caller's vector may already hold the per-file findings:
    /// a marker this pass consumes also clears the stale-suppression finding the
    /// per-file pass left behind, because that verdict was made before this
    /// family could see the finding it names.
    pub fn reconcile(&self, findings: &mut Vec<Finding>) {
        self.add(findings, self.duplicates());
        self.add(findings, checks::DuplicateModule::findings(&self.modules));
        self.add(
            findings,
            checks::Forwarding::findings(
                &self.forwarders.methods,
                &self.forwarders.functions,
                &self.forwarders.repo_methods,
                &self.forwarders.imports,
            ),
        );
        self.add(
            findings,
            checks::Unused::unused_findings(
                &self.definitions,
                &self.references.production,
                &self.references.tests,
                &self.references.strings,
            ),
        );
        self.add(
            findings,
            checks::Unused::unused_setter_findings(&self.setters, &self.references.production, &self.references.tests),
        );
        self.add(
            findings,
            checks::Scattered::scattered_instance_findings(&self.per_file(|scan| &scan.scattered), &self.class_names()),
        );
        self.add(
            findings,
            checks::PathExpression::findings(&self.per_file(|scan| &scan.path_expressions)),
        );
        self.add(
            findings,
            checks::ExpressionOccurrence::findings(&self.per_file(|scan| &scan.duplicate_expressions)),
        );
        self.add(
            findings,
            checks::ConstructionOccurrence::findings(
                &self.per_file(|scan| &scan.shared_constructions),
                &self.class_names(),
            ),
        );
        self.add(
            findings,
            checks::EnvyCandidate::findings(&self.per_file(|scan| &scan.free_envy), &self.class_names()),
        );
        self.add(
            findings,
            checks::ModuleShape::findings(&self.per_file(|scan| &scan.module_shape)),
        );
        self.add(
            findings,
            checks::OverAbstraction::abstraction_findings(&self.class_scans()),
        );
        self.add(findings, self.undeclared_members());
        self.add(findings, self.middle_man());
        self.add(
            findings,
            checks::SiblingValueClass::sibling_value_class_findings(&self.per_file(|scan| &scan.sibling_classes)),
        );
        self.add(findings, self.monkeypatch_findings());
    }

    /// The monkeypatch split: a patch on a module the repo owns is a fail (the
    /// test can inject a seam); a patch on a third-party module is a warning
    /// (no object fake exists without changing the dependency). An object
    /// target — a Name/Attribute, not a string module path — is local and
    /// stays a fail.
    fn monkeypatch_findings(&self) -> Vec<Finding> {
        let mut out = Vec::new();
        for (scan, rel) in self.scans.iter().zip(&self.rels) {
            for site in &scan.patch_sites {
                let third_party = site.target.as_deref().is_some_and(|target| {
                    let root = target.split('.').next().unwrap_or(target);
                    !self.rels.iter().any(|candidate| {
                        candidate == &format!("{root}.py")
                            || candidate.starts_with(&format!("{root}/"))
                            || candidate.contains(&format!("/{root}/"))
                    })
                });
                if third_party {
                    out.push(Finding {
                        logical_start: None,
                        seam_members: Vec::new(),
                        col: 0,
                        file: rel.clone(),
                        line: site.line,
                        function: site.function.clone(),
                        kind: "third-party-patch".into(),
                        severity: "warn".into(),
                        message: format!(
                            "{} at line {} patches '{}', a global in a module the repo does not own — no object fake can be injected without changing the dependency. Use the dependency's seam if it has one; otherwise keep the patch and record why this third-party boundary is the test's subject.",
                            site.desc,
                            site.line,
                            site.target.as_deref().unwrap_or("")
                        ),
                    });
                } else {
                    let target = site
                        .target
                        .as_ref()
                        .map(|target| format!(" patches '{target}'"))
                        .unwrap_or_default();
                    out.push(Finding {
                        logical_start: None,
                        seam_members: Vec::new(),
                        col: 0,
                        file: rel.clone(),
                        line: site.line,
                        function: site.function.clone(),
                        kind: "monkeypatch".into(),
                        severity: "fail".into(),
                        message: format!(
                            "{} at line {}{target} — never monkeypatch global state; inject an object fake (a class implementing the real protocol) via parameter injection or a dependency-injection container — fakes are objects, not functions",
                            site.desc, site.line
                        ),
                    });
                }
            }
        }
        out
    }

    /// The final pass: every finding the gate composed itself — the families
    /// whose inputs the scans do not carry (graph, docs, churn, bulk), plus the
    /// per-file findings — through its file's suppressions, dropping the stale
    /// markers those comments left. The scan-derived families already passed
    /// through [`RepoScan::reconcile`], one family at a time.
    pub fn apply_suppressions(&self, findings: Vec<Finding>) -> Vec<Finding> {
        let mut used_line: HashSet<(usize, String)> = HashSet::new();
        let mut used_file: HashSet<String> = HashSet::new();
        let mut kept = Vec::new();
        for (file, family) in Self::grouped(findings) {
            kept.extend(self.filter_file(&file, family, &mut used_line, &mut used_file));
        }
        Self::drop_used_stale(&mut kept, &used_line, &used_file);
        kept
    }

    /// The `duplicate` rule's findings, compared within a language: a Python
    /// function and a Rust function with the same shape are a port, not a
    /// copy-paste duplicate, so the two pools are never cross-matched.
    fn duplicates(&self) -> Vec<Finding> {
        let (rust, python): (Vec<SkeletonFn>, Vec<_>) = self
            .skeletons
            .iter()
            .cloned()
            .partition(|shape| shape.rel.ends_with(".rs"));
        let mut found = checks::duplicate_findings(&python);
        found.extend(checks::duplicate_findings(&rust));
        found
    }

    /// Apply each file's `lucidlint: ignore` comments to one family's findings
    /// and keep what survives — one family at a time, as the gate does.
    fn add(&self, findings: &mut Vec<Finding>, found: Vec<Finding>) {
        let mut used_line: HashSet<(usize, String)> = HashSet::new();
        let mut used_file: HashSet<String> = HashSet::new();
        // one group per file: innermost-peel capacity is computed over the whole
        // group, and pairs the per-file pass already spent are not spent again
        for (file, family) in Self::grouped(found) {
            findings.extend(self.filter_file(&file, family, &mut used_line, &mut used_file));
        }
        // a suppression this family never used is stale — the ledger must show it
        Self::drop_used_stale(findings, &used_line, &used_file);
    }

    /// One file's findings through that file's suppressions, recording every
    /// marker the pass consumed; pairs the per-file pass spent are not spent
    /// again.
    fn filter_file(
        &self,
        file: &str,
        family: Vec<Finding>,
        used_line: &mut HashSet<(usize, String)>,
        used_file: &mut HashSet<String>,
    ) -> Vec<Finding> {
        let Some(suppressions) = self.suppressions.remaining.get(file) else {
            return family;
        };
        let empty = HashSet::new();
        let spent = self.suppressions.spent.get(file).unwrap_or(&empty);
        crate::suppressions::Suppressions::filter_repo_wide(family, suppressions, used_line, used_file, spent)
    }

    /// Drop the stale-suppression findings for markers the pass just consumed:
    /// the finding they name was exempted, so the marker is not stale.
    fn drop_used_stale(findings: &mut Vec<Finding>, used_line: &HashSet<(usize, String)>, used_file: &HashSet<String>) {
        if used_line.is_empty() && used_file.is_empty() {
            return;
        }
        findings.retain(|finding| {
            if finding.kind != "stale-suppression" {
                return true;
            }
            let signal = crate::suppressions::Suppressions::signal_of_stale(&finding.message);
            !used_file.contains(&signal) && !used_line.contains(&(finding.line, signal))
        });
    }

    /// The findings filed under the file each one names — the report's grouping.
    fn grouped(findings: Vec<Finding>) -> HashMap<String, Vec<Finding>> {
        let mut by_file: HashMap<String, Vec<Finding>> = HashMap::new();
        for finding in findings {
            by_file.entry(finding.file.clone()).or_default().push(finding);
        }
        by_file
    }

    /// One per-file collection, paired with the file it came from — the shape
    /// each per-file family's `findings` takes.
    fn per_file<T: Clone>(&self, of: impl Fn(&FileScan) -> &T) -> Vec<(String, T)> {
        self.scans
            .iter()
            .zip(&self.rels)
            .map(|(scan, rel)| (rel.clone(), of(scan).clone()))
            .collect()
    }

    /// The class names the repo defines — what the scattered-instance rule
    /// resolves an instance against.
    fn class_names(&self) -> HashSet<String> {
        self.definitions
            .iter()
            .filter(|definition| definition.is_class)
            .map(|definition| definition.name.clone())
            .collect()
    }

    /// The `over-abstraction` rule's per-file input: each file's classes and
    /// imports — the index an abstract class's subclasses resolve against.
    fn class_scans(&self) -> Vec<(String, Vec<crate::ClassInfo>, Vec<ImportInfo>)> {
        self.scans
            .iter()
            .zip(&self.rels)
            .map(|(scan, rel)| (rel.clone(), scan.classes.clone(), scan.imports.clone()))
            .collect()
    }

    /// The `undeclared-attribute` rule's per-file input: the classes, imports,
    /// and `self.x = …` sites one file declares.
    fn undeclared_members(&self) -> Vec<Finding> {
        let undeclared: Vec<UndeclScan> = self
            .scans
            .iter()
            .zip(&self.rels)
            .map(|(scan, rel)| UndeclScan {
                rel: rel.clone(),
                classes: scan.classes.clone(),
                imports: scan.imports.clone(),
                self_assigns: scan.self_assigns.clone(),
            })
            .collect();
        checks::LatentState::undeclared(&undeclared)
    }

    /// The `middle-man` rule over the whole repo: the index is the union of every
    /// file's classes — one class per file is the house convention, so a
    /// delegation's target usually lives in another file and a file-local index
    /// would read almost every real target as outside the codebase. A candidate
    /// a single-buffer scan already reported (same file and line) is not
    /// reported twice.
    fn middle_man(&self) -> Vec<Finding> {
        let mut classes: Vec<checks::MiddleManClass> = Vec::new();
        let mut candidates: Vec<checks::MiddleManCandidate> = Vec::new();
        let mut already: HashSet<(String, usize)> = HashSet::new();
        for (scan, rel) in self.scans.iter().zip(&self.rels) {
            classes.extend(scan.middle_man_classes.iter().cloned());
            candidates.extend(scan.middle_man_candidates.iter().cloned());
            for finding in &scan.findings {
                if finding.kind == "middle-man" {
                    already.insert((rel.clone(), finding.line));
                }
            }
        }
        checks::MiddleMan::findings(&candidates, &classes)
            .into_iter()
            .filter(|finding| !already.contains(&(finding.file.clone(), finding.line)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(name: &str, src: &str) -> FileScan {
        crate::Pipeline::scan_source(src, name)
    }

    #[test]
    fn over_abstraction_is_a_corpus_family() {
        let base = scan(
            "a.py",
            include_str!("../../tests/fixtures/rust/abstraction_single_concrete_is_found__01.py"),
        );
        let concrete = scan(
            "b.py",
            include_str!("../../tests/fixtures/rust/abstraction_single_concrete_is_found__02.py"),
        );
        let scans = vec![base, concrete];
        let found = RepoScan::of(&scans, "repo").findings();
        let over: Vec<&Finding> = found
            .iter()
            .filter(|finding| finding.kind == "over-abstraction")
            .collect();
        assert_eq!(over.len(), 1, "{found:?}");
        assert_eq!(over[0].file, "a.py");
    }

    #[test]
    fn corpus_families_are_registered_and_sorted() {
        let families = RepoScan::FAMILIES;
        let mut sorted = families.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, families, "keep FAMILIES sorted and unique");
        for kind in families {
            assert!(crate::FAMILY_KINDS.contains(kind), "'{kind}' is not a registered kind");
        }
    }

    #[test]
    fn reconcile_clears_the_stale_marker_the_per_file_pass_left() {
        // the per-file pass cannot see a repo-wide finding, so an `ignore
        // unused` marker starts life as stale; the corpus consumes it and the
        // verdict must go — the marker is the reason the finding is silent
        let plain = scan("prod.py", include_str!("../../tests/fixtures/code/sample_reconcile_clears_the_stale_marker_the_per_file_pass_left__d3a91676.py"));
        assert!(
            RepoScan::of_names(&[plain])
                .findings()
                .iter()
                .any(|finding| finding.kind == "unused"),
            "the fixture must be an unused definition, or the test proves nothing"
        );
        let scans = vec![scan(
            "prod.py",
            include_str!("../../tests/fixtures/rust/reconcile_clears_the_stale_marker_the_per_file_pass_left__01.py"),
        )];
        let corpus = RepoScan::of_names(&scans);
        let mut findings: Vec<Finding> = scans.iter().flat_map(|one| one.findings.clone()).collect();
        assert!(findings.iter().any(|finding| finding.kind == "stale-suppression"));
        corpus.reconcile(&mut findings);
        assert!(
            !findings.iter().any(|finding| finding.kind == "unused"),
            "the marker suppresses the finding: {findings:?}"
        );
        assert!(
            !findings.iter().any(|finding| finding.kind == "stale-suppression"),
            "{findings:?}"
        );
    }
}
