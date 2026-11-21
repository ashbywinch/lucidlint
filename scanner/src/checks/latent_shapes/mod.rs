//! The latent-class shape families — one module per rule: a method in exile, an
//! anonymous tuple record, a fixed-arity tuple, and a state-threading assembly
//! site. Each rule owns the walkers that read its shape; the module root only
//! names them.

mod assembly_class;
mod misplaced_method;
mod tuple_record;
mod wide_tuple;

pub use assembly_class::AssemblyClass;
pub use misplaced_method::MisplacedMethod;
pub use tuple_record::TupleRecord;
pub use wide_tuple::WideTuple;
