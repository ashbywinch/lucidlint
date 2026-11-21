//! The finding record and the complexity fact: what one action is, and what the
//! complexity family measured on one function.

use std::collections::HashSet;

use serde::Serialize;

/// One open function scope: its decision count and the names it returns
/// (for the swallow analysis).
pub struct FnScope {
    /// Names this function returns (for the swallow analysis).
    pub returned: HashSet<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Finding {
    pub(crate) file: String,
    pub(crate) line: usize,
    /// 1-based column of the finding's anchor node; 0 = line-level (schema 3).
    /// Populated only where disambiguation needs it (magic-number,
    /// positional-literals, record-shape literals).
    #[serde(default)]
    pub(crate) col: usize,
    pub(crate) function: String,
    pub(crate) kind: String,
    pub(crate) severity: String,
    pub(crate) message: String,
    /// The clump identity for seam grouping (#34): the shared parameter-pair
    /// names (data-clump), the disjoint method-group members (partition),
    /// the functions sharing a leading parameter (strewing). Always emitted
    /// (empty on non-carriers — BQ4); never scraped from message prose.
    pub seam_members: Vec<String>,

    /// The first source line of the LOGICAL line holding this finding — the
    /// `def`/`async def` line for a signature finding, the enclosing
    /// statement's first source line for a wrapped call or split literal.
    /// One line of code can span several source lines, and a marker binds on
    /// the logical line too (Phase 4a). `None` = the finding's own line, the
    /// usual case; internal only (never serialized — the JSON stays schema 4).
    #[serde(skip)]
    pub(crate) logical_start: Option<usize>,
}

/// One function's cyclomatic complexity — radon-equivalent counting.
#[derive(Serialize, Clone)]
pub struct FnCc {
    pub(crate) file: String,
    pub(crate) function: String,
    pub(crate) line: usize,
    pub(crate) cc: u32,
    /// The body's SHAPE for the complexity message routing: "dispatch",
    /// "rules", or "plain" (see python_fn_shape / rust_fn_shape).
    pub shape: &'static str,
    /// The dispatch selector or rule-battery accumulator name.
    pub shape_detail: String,
}
