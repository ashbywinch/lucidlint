//! `middle-man`: a class that only forwards to another — the class in the
//! middle adds a hop, not a concept.

use ruff_python_ast::Stmt;

use crate::finding::Finding;
use crate::ScanState;

mod candidate;
mod class;
mod classes;
mod declaration;

use classes::ModuleClasses;

pub use candidate::MiddleManCandidate;
pub use class::MiddleManClass;

/// The middle-man rule: the file-local collection and the repo-wide verdict.
pub struct MiddleMan;

impl MiddleMan {
    /// Remove Middle Man: a method whose body is a single delegation call
    /// (`return self.x.y(...)` / `return self.y(...)`) — it only forwards.
    ///
    /// Not a middle man when the advice cannot be taken (issue #44): the
    /// delegated member is not declared anywhere in the scanned codebase (a
    /// library's method — the wrapper is where that library meets our
    /// interface), the method is a member of an interface the codebase declares
    /// (callers are typed against it), or the call fixes an argument the target
    /// defaults (the policy lives in the named method).
    ///
    /// Runs over the whole repo's candidates and classes: a wrapper's target is
    /// usually a class in another file (one class per file), so a file-local
    /// index would read almost every real target as outside the codebase.
    pub fn findings(candidates: &[MiddleManCandidate], classes: &[MiddleManClass]) -> Vec<Finding> {
        let module = ModuleClasses::merge(classes);
        candidates
            .iter()
            .filter(|cand| cand.fires(&module))
            .map(MiddleManCandidate::finding)
            .collect()
    }

    /// Per-file collection for the repo-wide pass: the file's classes and its
    /// delegation candidates. A single-buffer scan (`--file`, the LSP's typing
    /// path) never runs a repo-wide pass, so it decides here against the file's
    /// own classes — the conservative reading, since a target in another file
    /// cannot be seen from one buffer.
    pub fn collect(state: &mut ScanState, body: &[Stmt], source: &str, repo_wide: bool) {
        let module = ModuleClasses::read(body, state.file);
        let mut candidates = MiddleManCandidate::collect(body, source);
        for c in &mut candidates {
            c.rel = state.file.to_string();
        }
        if !repo_wide {
            let classes: Vec<MiddleManClass> = module.classes.values().cloned().collect();
            state.findings.extend(Self::findings(&candidates, &classes));
        }
        state.middle_man_classes.extend(module.classes.into_values());
        state.middle_man_candidates.extend(candidates);
    }
}
