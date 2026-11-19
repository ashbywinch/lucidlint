//! The repo-wide families computed from the code-review-graph contract
//! (a versioned, schema-neutral JSON emitted by lucidlint_graph_export.py
//! through the graph tool's own public API — the gate never touches the
//! SQLite schema or the DB location). Plus hotspot (git churn + max CC) and
//! churn-untested (top churn with no test coverage).
//!
//! Finding identity (kind/file/line/function) matches the Python reference
//! exactly; the concern-mix wording in messages is the honest core fact.

mod churn;
mod cohesion;
mod contract;
mod coverage;
mod cycles;
mod folder_mix;
mod graph_paths;
mod hotspot;
mod layer_mix;
mod node;
mod risk;

pub(crate) use churn::ChurnUntested;
pub(crate) use cohesion::Cohesion;
pub use contract::GraphContract;
pub use coverage::CoverageSnapshot;
pub(crate) use cycles::Cycles;
pub(crate) use folder_mix::FolderMix;
pub(crate) use hotspot::Hotspot;
pub(crate) use layer_mix::LayerMix;
pub(crate) use risk::HighRisk;

#[cfg(test)]
mod tests;
