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
    /// Gather the scans into the pools every repo-wide rule reads.
    pub fn of(scans: &'a [FileScan], root: &str) -> Self {
        let rels: Vec<String> = scans.iter().map(|scan| scan.rel_of(root)).collect();
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
        self.add(&mut findings, self.duplicates());
        self.add(&mut findings, checks::DuplicateModule::findings(&self.modules));
        self.add(
            &mut findings,
            checks::Forwarding::findings(
                &self.forwarders.methods,
                &self.forwarders.functions,
                &self.forwarders.repo_methods,
                &self.forwarders.imports,
            ),
        );
        self.add(
            &mut findings,
            checks::Unused::unused_findings(
                &self.definitions,
                &self.references.production,
                &self.references.tests,
                &self.references.strings,
            ),
        );
        self.add(
            &mut findings,
            checks::Unused::unused_setter_findings(&self.setters, &self.references.production, &self.references.tests),
        );
        self.add(
            &mut findings,
            checks::Scattered::scattered_instance_findings(&self.per_file(|scan| &scan.scattered), &self.class_names()),
        );
        self.add(
            &mut findings,
            checks::PathExpression::findings(&self.per_file(|scan| &scan.path_expressions)),
        );
        self.add(
            &mut findings,
            checks::ExpressionOccurrence::findings(&self.per_file(|scan| &scan.duplicate_expressions)),
        );
        self.add(
            &mut findings,
            checks::ModuleShape::findings(&self.per_file(|scan| &scan.module_shape)),
        );
        self.add(&mut findings, self.undeclared_members());
        self.add(&mut findings, self.middle_man());
        self.add(
            &mut findings,
            checks::SiblingValueClass::sibling_value_class_findings(&self.per_file(|scan| &scan.sibling_classes)),
        );
        findings
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
            match self.suppressions.remaining.get(&file) {
                Some(suppressions) => {
                    let empty = HashSet::new();
                    let spent = self.suppressions.spent.get(&file).unwrap_or(&empty);
                    findings.extend(crate::suppressions::Suppressions::filter_repo_wide(
                        family,
                        suppressions,
                        &mut used_line,
                        &mut used_file,
                        spent,
                    ));
                }
                None => findings.extend(family),
            }
        }
        if used_line.is_empty() && used_file.is_empty() {
            return;
        }
        // a suppression this family never used is stale — the ledger must show it
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
