//! The ordering functions `sorted_results` accepts — mirroring radon's
//! SCORE/LINES/ALPHA.

use crate::visitors::{get_complexity, Block};

/// Ordering functions for `sorted_results`.
pub fn score(b: &Block) -> i32 {
    -get_complexity(b)
}

pub fn lines(b: &Block) -> i32 {
    b.lineno()
}

pub fn alpha(b: &Block) -> String {
    b.name().to_string()
}
