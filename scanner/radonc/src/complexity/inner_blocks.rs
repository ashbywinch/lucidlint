//! `add_inner_blocks` — radon's lift of closures and inner classes to
//! top-level blocks, named after their owner.

use crate::visitors::Block;

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
