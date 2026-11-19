//! The ordering a `sorted_results` call asks for.

#[derive(Clone, Copy)]
pub enum Order {
    Score,
    Lines,
    Alpha,
}
