//! A block is either a function or a class — mirroring radon's `Block` union.

use super::{Class, Function};

/// A block is either a function or a class — mirroring radon's `Block` union.
#[derive(Clone, Debug)]
pub enum Block {
    Function(Function),
    Class(Class),
}

impl Block {
    pub fn name(&self) -> &str {
        match self {
            Block::Function(f) => &f.name,
            Block::Class(c) => &c.name,
        }
    }

    pub fn complexity(&self) -> i32 {
        match self {
            Block::Function(f) => f.complexity,
            Block::Class(c) => c.complexity,
        }
    }

    pub fn lineno(&self) -> i32 {
        match self {
            Block::Function(f) => f.lineno,
            Block::Class(c) => c.lineno,
        }
    }
}
