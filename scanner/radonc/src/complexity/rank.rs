//! `cc_rank` — radon's A–F risk band for a complexity score.

/// Rank the complexity score from A to F (radon's `cc_rank`).
///
/// ```text
/// 1 - 5        A (low risk - simple block)
/// 6 - 10       B (low risk - well structured and stable block)
/// 11 - 20      C (moderate risk - slightly complex block)
/// 21 - 30      D (more than moderate risk - more complex block)
/// 31 - 40      E (high risk - complex block, alarming)
/// 41+          F (very high risk - error-prone, unstable block)
/// ```
/// Radon's rank ladder: `ceil(cc / 10)` picks the letter, and F is the
/// last rank (index 5 in the A-F table).
const RANK_STEP: f64 = 10.0;
const LAST_RANK_INDEX: i32 = 5;

pub fn cc_rank(cc: i32) -> char {
    if cc < 0 {
        panic!("Complexity must be a non-negative value");
    }
    let a = (cc as f64 / RANK_STEP).ceil() as i32;
    let a = if a == 0 { 1 } else { a };
    let b = if LAST_RANK_INDEX - cc < 0 { 0 } else { 1 };
    let idx = (a - b).min(LAST_RANK_INDEX);
    (b'A' as i32 + idx) as u8 as char
}
