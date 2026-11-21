//! `sorted_results` — radon's block ordering by score, lines or name.

use crate::visitors::{get_complexity, Block};

use super::Order;

/// Sort blocks by complexity (descending) — radon's `sorted_results`.
/// `order` is one of score / lines / alpha.
pub fn sorted_results(blocks: Vec<Block>, order: Order) -> Vec<Block> {
    let mut b = blocks;
    match order {
        Order::Score => b.sort_by_key(|blk| -get_complexity(blk)),
        Order::Lines => b.sort_by_key(|blk| blk.lineno()),
        Order::Alpha => b.sort_by_key(|blk| blk.name().to_string()),
    }
    b
}
