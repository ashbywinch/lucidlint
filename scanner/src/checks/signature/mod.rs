//! Signature hygiene — one module per rule: a boolean literal passed
//! positionally, a list of same-kind literals, a method detached from its
//! object, an over-long parameter list, and an except that swallows. Each
//! makes the call site unreadable.

mod boolean_arg;
mod detached_method;
mod except;
mod long_param_list;
mod positional_literals;
mod stub_body;

pub use boolean_arg::BooleanArg;
pub use detached_method::DetachedMethod;
pub use except::Except;
pub use long_param_list::LongParamList;
pub use positional_literals::PositionalLiterals;
