//! Signatures of the runtime-library functions that generated code may call.
//! The implementations live in the `flash-rt` crate (see crates/flash-rt/src/lib.rs).

use crate::ir::Ty;

const I: Ty = Ty::I64;
const F: Ty = Ty::F64;
const B: Ty = Ty::I8;

pub const RT_FUNCS: &[(&str, &[Ty], Option<Ty>)] = &[
    ("fl_alloc", &[I], Some(I)),
    ("fl_free", &[I, I], None),
    ("fl_str_concat", &[I, I], Some(I)),
    ("fl_str_eq", &[I, I], Some(B)),
    ("fl_str_cmp", &[I, I], Some(I)),
    ("fl_str_from_int", &[I], Some(I)),
    ("fl_str_from_uint", &[I], Some(I)),
    ("fl_str_from_float", &[F], Some(I)),
    ("fl_str_from_bool", &[B], Some(I)),
    ("fl_str_from_none", &[], Some(I)),
    ("fl_str_clone", &[I], Some(I)),
    ("fl_str_drop", &[I], None),
    ("fl_print_int", &[I], None),
    ("fl_print_uint", &[I], None),
    ("fl_print_float", &[F], None),
    ("fl_print_bool", &[B], None),
    ("fl_print_str", &[I], None),
    ("fl_print_none", &[], None),
    ("fl_print_space", &[], None),
    ("fl_print_nl", &[], None),
    ("fl_list_new", &[I, I], Some(I)),
    ("fl_list_push_slot", &[I, I], Some(I)),
    ("fl_list_pop_slot", &[I, I], Some(I)),
    ("fl_list_free", &[I, I], None),
    ("fl_list_clone_shallow", &[I, I], Some(I)),
    ("fl_panic_bounds", &[I, I], None),
    ("fl_panic_divzero", &[], None),
    ("fl_unwrap_fail", &[I], None),
    ("fl_assert_fail", &[I], None),
    ("fl_fatal_error", &[I], None),
    ("fl_math_sqrt", &[F], Some(F)),
    ("fl_math_floor", &[F], Some(F)),
    ("fl_math_ceil", &[F], Some(F)),
    ("fl_math_sin", &[F], Some(F)),
    ("fl_math_cos", &[F], Some(F)),
    ("fl_math_tan", &[F], Some(F)),
    ("fl_math_exp", &[F], Some(F)),
    ("fl_math_ln", &[F], Some(F)),
    ("fl_math_abs", &[F], Some(F)),
    ("fl_math_pow", &[F, F], Some(F)),
    ("fl_exit", &[I], None),
    ("fl_now_ns", &[], Some(I)),
    ("fl_sleep_ms", &[I], None),
    ("fl_args", &[], Some(I)),
    ("fl_read_line", &[], Some(I)),
    ("fl_str_upper", &[I], Some(I)),
    ("fl_str_lower", &[I], Some(I)),
    ("fl_str_trim", &[I], Some(I)),
    ("fl_str_contains", &[I, I], Some(B)),
    ("fl_str_starts_with", &[I, I], Some(B)),
    ("fl_str_ends_with", &[I, I], Some(B)),
    ("fl_str_replace", &[I, I, I], Some(I)),
    ("fl_str_split", &[I, I], Some(I)),
    ("fl_str_join", &[I, I], Some(I)),
    ("fl_str_find", &[I, I], Some(I)),
    ("fl_str_substr", &[I, I, I], Some(I)),
    ("fl_str_byte_at", &[I, I], Some(B)),
    ("fl_parse_int", &[I], Some(I)),
    ("fl_parse_float", &[I], Some(I)),
    ("fl_read_file", &[I], Some(I)),
    ("fl_write_file", &[I, I], Some(I)),
    ("fl_append_file", &[I, I], Some(I)),
    ("fl_file_exists", &[I], Some(B)),
    ("fl_remove_file", &[I], Some(I)),
    ("fl_env_var", &[I], Some(I)),
    ("fl_path_join", &[I, I], Some(I)),
    ("fl_path_filename", &[I], Some(I)),
    ("fl_path_parent", &[I], Some(I)),
    ("fl_path_ext", &[I], Some(I)),
    ("fl_result_error", &[I], Some(I)),
    // plugins
    ("fl_plugin_load", &[I, I], None),
    ("fl_plugin_call", &[I, I, I, I], Some(I)),
];

/// Runtime functions that only exist in the native back end's Flash-language runtime (the legacy
/// Rust runtime has no maps or sets).
pub const NATIVE_ONLY_FUNCS: &[(&str, &[Ty], Option<Ty>)] = &[
    ("fl_panic_key", &[], None),
    ("fl_map_new", &[I], Some(I)),
    ("fl_map_find", &[I, I, I], Some(I)),
    ("fl_map_insert", &[I, I, I], Some(I)),
    ("fl_map_remove", &[I, I, I], None),
    ("fl_map_clear", &[I, I], None),
    ("fl_map_free", &[I, I], None),
    ("fl_map_clone_shallow", &[I, I], Some(I)),
    ("fl_map_keys", &[I, I, I], Some(I)),
    ("fl_map_values", &[I, I], Some(I)),
];

pub fn lookup(name: &str) -> Option<(&'static [Ty], Option<Ty>)> {
    RT_FUNCS.iter().chain(NATIVE_ONLY_FUNCS).find(|(n, _, _)| *n == name).map(|(_, p, r)| (*p, *r))
}
