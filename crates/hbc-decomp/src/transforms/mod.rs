pub mod arrays;
pub mod chain_access;
mod class_patterns;
mod cleanup;
mod codegen;
pub mod data_flow;
pub mod default_params;
pub mod destructuring;
pub mod exports;
mod generator;
mod hoist_closures;
mod inline;
pub mod logic_patterns;
pub mod logic_simplify;
mod module_hoist;
mod name_inference;
pub mod objects;
mod optimize;
mod patterns;
mod propagate;
mod simplify;
pub mod spread_rest;
pub mod ssa;
pub mod ternary_returns;
mod var_kind;
pub mod var_naming;
pub mod worklet_source;

pub use chain_access::optimize_chain_access;
pub use class_patterns::detect_class_patterns;
pub use cleanup::advanced::cleanup_advanced;
pub use cleanup::cleanup_statements;
pub use codegen::{body_hole, Codegen, CodegenOptions, BODY_HOLE};
pub use default_params::transform_default_params;
pub use destructuring::{
    detect_destructuring, detect_iterator_destructuring, reconstruct_babel_array_destructuring,
    reconstruct_v98_array_destructuring,
};
pub use generator::{
    cleanup_generator_comments, detect_generator_patterns, has_generator_patterns,
    reconstruct_generator_v98, simplify_state_machine, try_reconstruct_generator_v98,
};
pub use hoist_closures::hoist_repeated_closures;
pub use inline::names_used_by_descendants;
pub use inline::{
    cleanup_noise, eliminate_immutable_aliases, extra_writes_from_nested_bodies,
    fold_array_literals, fold_object_literals, inline_expressions, inline_named_variables,
    inline_named_variables_keeping, insert_declarations, insert_declarations_with_extra_writes,
    insert_declarations_with_outer, insert_declarations_with_slots, make_sanitized_names_distinct,
    rename_reserved_words, simplify_arguments_copy, strip_hermes_this,
};
pub use logic_simplify::simplify_logic_advanced;
pub use module_hoist::hoist_module_loaders;
pub use name_inference::infer_names;
pub use objects::{fold_slot_index_fills, transform_object_literals};
mod collapse_registry;
pub use collapse_registry::collapse_metro_registry;
pub use logic_patterns::transform_logic;
pub use optimize::{
    eliminate_dead_stores, fold_builtin_guards, optimize_statements, remove_dead_temp_bindings,
    remove_dead_temp_bindings_keeping, repair_switch_clobbers,
};
pub use patterns::{
    convert_while_true_loops, detect_for_in_loops, detect_for_of_loops, detect_legacy_for_of,
    detect_patterns, detect_short_circuit_logic, fold_guarded_loops, reconstruct_jsx,
};
pub use propagate::{propagate, propagate_copies, resolve_global_reads, PropagationConfig};
pub use simplify::{simplify_expr, simplify_statements, simplify_stmt};
pub use spread_rest::transform_spread_rest;
pub use ssa::transform_to_ssa;
pub use ternary_returns::optimize_ternary_returns;
pub use var_kind::promote_const_bindings;
pub use var_naming::{
    infer_variable_names, inherit_ancestor_closure_names, rename_closure_variables,
    rename_closure_variables_cross_function, rename_closures_from_definitions, sync_capture_names,
};
pub use worklet_source::collect_worklet_sources;
