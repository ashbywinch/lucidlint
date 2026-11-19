//! High-level helpers working with cyclomatic complexity — a Rust port of
//! `radon.complexity`. Mirrors the original module's public functions:
//! `cc_rank`, `average_complexity`, `sorted_results`, `add_inner_blocks`,
//! `cc_visit`, `cc_visit_ast`.
//!
//! Each helper is its own module — rank, ordering, average, sorting, the
//! inner-block lift, the visits — and this root re-exports them under the
//! radon names, so the port still reads as one API.

mod average;
mod inner_blocks;
mod order;
mod ordering;
mod rank;
mod sorted;
mod visits;

pub use average::average_complexity;
pub use inner_blocks::add_inner_blocks;
pub use order::Order;
pub use ordering::{alpha, lines, score};
pub use rank::cc_rank;
pub use sorted::sorted_results;
pub use visits::{cc_visit, cc_visit_ast};
