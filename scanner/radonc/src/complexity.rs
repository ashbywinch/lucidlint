//! High-level helpers working with cyclomatic complexity — a Rust port of
//! `radon.complexity`. Mirrors the original module's public functions:
//! `cc_rank`, `average_complexity`, `sorted_results`, `add_inner_blocks`,
//! `cc_visit`, `cc_visit_ast`.

use crate::visitors::{Block, ComplexityVisitor};

/// Ordering functions for `sorted_results` — mirroring radon's SCORE/LINES/ALPHA.
pub fn score(b: &Block) -> i32 {
    -crate::visitors::get_complexity(b)
}
pub fn lines(b: &Block) -> i32 {
    b.lineno()
}
pub fn alpha(b: &Block) -> String {
    b.name().to_string()
}

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
pub fn cc_rank(cc: i32) -> char {
    if cc < 0 {
        panic!("Complexity must be a non-negative value");
    }
    let a = (cc as f64 / 10.0).ceil() as i32;
    let a = if a == 0 { 1 } else { a };
    let b = if 5 - cc < 0 { 0 } else { 1 };
    let idx = (a - b).min(5);
    (b'A' as i32 + idx) as u8 as char
}

/// Average complexity from blocks — radon's `average_complexity`. 0 for empty.
pub fn average_complexity(blocks: &[Block]) -> f64 {
    if blocks.is_empty() {
        return 0.0;
    }
    blocks.iter().map(crate::visitors::get_complexity).sum::<i32>() as f64 / blocks.len() as f64
}

/// Sort blocks by complexity (descending) — radon's `sorted_results`.
/// `order` is one of score / lines / alpha.
pub fn sorted_results(blocks: Vec<Block>, order: Order) -> Vec<Block> {
    let mut b = blocks;
    match order {
        Order::Score => b.sort_by_key(|blk| -crate::visitors::get_complexity(blk)),
        Order::Lines => b.sort_by_key(|blk| blk.lineno()),
        Order::Alpha => b.sort_by_key(|blk| blk.name().to_string()),
    }
    b
}

#[derive(Clone, Copy)]
pub enum Order {
    Score,
    Lines,
    Alpha,
}

/// Add closures and inner classes as top-level blocks — radon's `add_inner_blocks`.
pub fn add_inner_blocks(blocks: Vec<Block>) -> Vec<Block> {
    let mut new_blocks = Vec::new();
    let mut stack: Vec<Block> = blocks.into_iter().rev().collect();
    while let Some(block) = stack.pop() {
        let name = block.name().to_string();
        new_blocks.push(block);
        let Some(top) = new_blocks.last() else {
            return new_blocks;
        };
        match top {
            Block::Function(f) => {
                for inner in &f.closures {
                    let mut inner = inner.clone();
                    inner.name = format!("{name}.{}", inner.name);
                    stack.push(Block::Function(inner));
                }
                for inner in &f.inner_classes {
                    let mut inner = inner.clone();
                    inner.name = format!("{name}.{}", inner.name);
                    stack.push(Block::Class(inner));
                }
            }
            Block::Class(_) => {}
        }
    }
    new_blocks
}

/// Visit code and return its blocks — radon's `cc_visit(code)`.
pub fn cc_visit(code: &str) -> Vec<Block> {
    let visitor = ComplexityVisitor::from_code(code);
    visitor.blocks()
}

/// Visit an AST node — radon's `cc_visit_ast(ast_node)`.
pub fn cc_visit_ast(mod_: &ruff_python_ast::ModModule) -> Vec<Block> {
    let visitor = ComplexityVisitor::from_ast(mod_);
    visitor.blocks()
}