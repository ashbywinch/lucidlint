//! The decision families: an inline compound condition and an inline
//! classification chain are decisions with no name — the fix names them.

mod classification;
mod condition;
mod facts;

pub use classification::UnnamedClassification;
pub use condition::UnnamedCondition;
