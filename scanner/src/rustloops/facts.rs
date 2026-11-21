//! The loop's fact model and its classification. The walk builds the facts
//! once per site; every verdict is a pure function of them.

use super::HOIST_MIN_STMTS;

/// What kind of loop the site is. Only `For` can be a collection build.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LoopKind {
    For,
    While,
    Loop,
}

/// One loop's facts — computed once at the site, so every verdict reads the
/// same model instead of re-deriving it from the AST.
#[derive(Clone)]
pub(super) struct LoopFacts {
    pub(super) line: usize,
    pub(super) kind: LoopKind,
    /// The surviving names the body writes, minus the loop's own bindings.
    pub(super) mutated: Vec<String>,
    /// The surviving names the iterable/condition reads.
    pub(super) reads: Vec<String>,
    /// The surviving names the condition removes from (a worklist drain).
    pub(super) worklist: Vec<String>,
    /// The collection a `for` body builds, when it is one push/indexed store.
    pub(super) build: Option<PipelineBuild>,
    /// The flattened body statement count.
    pub(super) statements: usize,
    /// The body grows the collection the condition indexes (a cursor walk).
    pub(super) grows: bool,
}

/// How a pipeline-shaped body collects its value.
#[derive(Clone)]
pub(crate) enum PipelineBuild {
    /// `acc.push(item)` — one item joined per pass.
    Push(String),
    /// `acc[i] = v` — the position is the data.
    Indexed(String),
}

/// What a loop IS — the one classification each verdict is read from.
pub(super) enum LoopShape {
    /// The condition consumes a worklist, or a cursor indexes a collection the
    /// body grows: the loop is the traversal, and its writes are the walk's
    /// state, not outputs. No stateful recipe applies (#109, #115).
    Traversal,
    /// A `for` body that only builds a collection.
    Pipeline(PipelineBuild),
    /// Two or more surviving mutations.
    Stateful(Vec<String>),
    /// One surviving mutation and a body long enough to hoist.
    Hoist { name: String, statements: usize },
    /// Nothing the family can name.
    Quiet,
}

impl LoopFacts {
    /// Classify the loop. The traversal check comes first and is not
    /// conditioned on the body writing the drained name: a walk that only
    /// reads the worklist and accumulates a result elsewhere is still the
    /// traversal.
    pub(super) fn shape(&self) -> LoopShape {
        if !self.worklist.is_empty() || self.grows {
            return LoopShape::Traversal;
        }
        if self.kind == LoopKind::For {
            if let Some(build) = &self.build {
                return LoopShape::Pipeline(build.clone());
            }
        }
        if self.mutated.len() >= 2 {
            return LoopShape::Stateful(self.mutated.clone());
        }
        if self.mutated.len() == 1 && self.statements >= HOIST_MIN_STMTS {
            return LoopShape::Hoist {
                name: self.mutated[0].clone(),
                statements: self.statements,
            };
        }
        LoopShape::Quiet
    }
}
