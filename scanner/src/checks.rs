//! The Python check families. Each module owns one finding family — its detection
//! and the message the reader acts on; this root only declares them and re-exports
//! the vocabulary the rest of the crate calls.

mod builtin_shadow;
mod class_facts;
mod closure_cluster;
mod closures;
mod conditional_polymorphism;
mod decisions;
mod delegating_husk;
mod docstring;
mod duplicate_block;
mod duplicate_def;
mod duplicate_expression;
mod duplicate_function;
mod duplicate_module;
mod expr_walk;
mod fakefs;
mod flat_folder;
mod forwarding;
mod function_facts;
mod global_state;
mod guard_clause;
mod latent_shapes;
mod latent_state;
mod latent_visitor;
mod loop_family;
mod magic;
mod middle_man;
mod module_names;
mod module_size;
mod monkeypatch;
mod naming_units;
mod no_assert_test;
mod noqa;
mod over_abstraction;
mod partition;
mod process_class;
mod record_shape;
mod scattered;
mod shared_path;
mod sibling;
mod signature;
mod skipif;
mod special_case;
mod static_husk;
mod type_ignore;
mod unused;

pub use crate::fix_render::{fix_kind_of_message, kind_fix_default};
pub use crate::forwarding::ForwarderFn;
pub use crate::forwarding::ForwarderMethod;
pub use crate::forwarding::RepoMethod;
pub use crate::skeleton::{fn_skeleton, is_duplicate_candidate, skel_children, SkeletonFn, SkeletonModule, Q};
pub use crate::suppressions::{apply_source_suppressions, logical_line_starts, parse_suppressions};
pub use builtin_shadow::shadow_findings;
pub use class_facts::annotation_base_name;
pub(crate) use class_facts::class_attr_names;
pub use closure_cluster::ClosureCluster;
pub use closures::closure_findings;
pub use conditional_polymorphism::conditional_polymorphism_findings;
pub use decisions::{unnamed_classification_findings, unnamed_condition_findings};
pub use delegating_husk::delegating_husk_findings;
pub use docstring::Docstring;
pub use duplicate_block::duplicate_block_findings;
pub use duplicate_def::duplicate_def_findings;
pub use duplicate_expression::{collect_duplicate_expressions, duplicate_expression_findings, ExpressionOccurrence};
pub use duplicate_function::duplicate_findings;
pub use duplicate_module::{collect_skeleton_modules, duplicate_module_findings};
pub(crate) use expr_walk::{all_exprs, push_stmt_children, stmt_exprs, walk_expr_deep};
pub use fakefs::fakefs_findings;
pub use flat_folder::FlatFolder;
pub use forwarding::Forwarding;
#[cfg(test)]
pub(crate) use function_facts::inner_function_count;
pub(crate) use function_facts::{body_refs_name, returned_names};
pub use global_state::{global_state_findings, module_constant_names};
pub use guard_clause::guard_clause_findings;
pub use latent_shapes::{
    assembly_class_findings, misplaced_method_findings, tuple_record_findings, wide_tuple_findings,
};
pub use latent_state::{
    collect_self_assigns, data_clump_findings, duplicate_field_findings, feature_envy_findings, undeclared_findings,
};
pub use latent_visitor::latent_visitor_findings;
pub use loop_family::loop_pipeline_findings;
pub use magic::{
    is_radix_prefix, magic_const_rhs_offsets, magic_exemption_labels, magic_len_guard_offsets,
    magic_table_exempt_offsets,
};
pub use middle_man::{collect_middle_man, middle_man_findings, MiddleManCandidate, MiddleManClass};
pub use module_names::{class_module_findings, strewing_findings, vague_name_findings};
pub use module_size::{ModuleShape, TypeShape};
pub use monkeypatch::Monkeypatch;
pub use naming_units::magic_unit_named_offsets;
pub(crate) use naming_units::name_states_unit;
pub use no_assert_test::no_assert_test_findings;
pub use noqa::noqa_findings;
pub use over_abstraction::{abstraction_findings, collect_classes, collect_imports};
pub use partition::Partition;
pub use process_class::process_class_findings;
pub use record_shape::record_shape_findings;
pub use scattered::{collect_scattered_instances, scattered_instance_findings, ScatteredInstance};
pub use shared_path::{collect_path_expressions, shared_path_findings, PathExpression};
pub use sibling::{collect_sibling_classes, sibling_value_class_findings, SiblingClass};
pub use signature::{
    boolean_arg_findings, detached_method_findings, except_findings, long_param_list_findings,
    positional_literals_findings,
};
pub use skipif::skipif_findings;
pub use special_case::SpecialCase;
pub use static_husk::StaticHusk;
pub use type_ignore::type_ignore_findings;
pub use unused::{collect_setters, unused_findings, unused_setter_findings};
