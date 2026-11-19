//! `average_complexity` — radon's mean block complexity.

use crate::visitors::{get_complexity, Block};

/// Average complexity from blocks — radon's `average_complexity`. 0 for empty.
pub fn average_complexity(blocks: &[Block]) -> f64 {
    if blocks.is_empty() {
        return 0.0;
    }
    blocks.iter().map(get_complexity).sum::<i32>() as f64 / blocks.len() as f64
}
