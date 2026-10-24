//! Table of runtime symbols that compiled programs may import (generated from `rtsigs.rs`; the
//! compiler test-suite checks that every name the compiler can emit is present).
#![allow(clippy::fn_to_numeric_cast)]

use crate::*;

extern "C" fn rt_floor(x: f64) -> f64 {
    x.floor()
}
extern "C" fn rt_ceil(x: f64) -> f64 {
    x.ceil()
}
extern "C" fn rt_trunc(x: f64) -> f64 {
    x.trunc()
}
extern "C" fn rt_sqrt(x: f64) -> f64 {
    x.sqrt()
}

/// Address of the runtime function called `name` (also C library helpers the code generator may call).
pub fn lookup(name: &str) -> Option<usize> {
    Some(match name {
        "fl_alloc" => fl_alloc as *const () as usize,
        "fl_free" => fl_free as *const () as usize,
        "fl_str_concat" => fl_str_concat as *const () as usize,
        "fl_str_eq" => fl_str_eq as *const () as usize,
        "fl_str_cmp" => fl_str_cmp as *const () as usize,
        "fl_str_from_int" => fl_str_from_int as *const () as usize,
        "fl_str_from_uint" => fl_str_from_uint as *const () as usize,
        "fl_str_from_float" => fl_str_from_float as *const () as usize,
        "fl_str_from_bool" => fl_str_from_bool as *const () as usize,
        "fl_str_from_none" => fl_str_from_none as *const () as usize,
        "fl_str_clone" => fl_str_clone as *const () as usize,
        "fl_str_drop" => fl_str_drop as *const () as usize,
        "fl_print_int" => fl_print_int as *const () as usize,
        "fl_print_uint" => fl_print_uint as *const () as usize,
        "fl_print_float" => fl_print_float as *const () as usize,
        "fl_print_bool" => fl_print_bool as *const () as usize,
        "fl_print_str" => fl_print_str as *const () as usize,
        "fl_print_none" => fl_print_none as *const () as usize,
        "fl_print_space" => fl_print_space as *const () as usize,
        "fl_print_nl" => fl_print_nl as *const () as usize,
        "fl_list_new" => fl_list_new as *const () as usize,
        "fl_list_push_slot" => fl_list_push_slot as *const () as usize,
        "fl_list_pop_slot" => fl_list_pop_slot as *const () as usize,
        "fl_list_free" => fl_list_free as *const () as usize,
        "fl_list_clone_shallow" => fl_list_clone_shallow as *const () as usize,
        "fl_panic_bounds" => fl_panic_bounds as *const () as usize,
        "fl_panic_divzero" => fl_panic_divzero as *const () as usize,
        "fl_unwrap_fail" => fl_unwrap_fail as *const () as usize,
        "fl_assert_fail" => fl_assert_fail as *const () as usize,
        "fl_fatal_error" => fl_fatal_error as *const () as usize,
        "fl_math_sqrt" => fl_math_sqrt as *const () as usize,
        "fl_math_floor" => fl_math_floor as *const () as usize,
        "fl_math_ceil" => fl_math_ceil as *const () as usize,
        "fl_math_sin" => fl_math_sin as *const () as usize,
        "fl_math_cos" => fl_math_cos as *const () as usize,
        "fl_math_tan" => fl_math_tan as *const () as usize,
        "fl_math_exp" => fl_math_exp as *const () as usize,
        "fl_math_ln" => fl_math_ln as *const () as usize,
        "fl_math_abs" => fl_math_abs as *const () as usize,
        "fl_math_pow" => fl_math_pow as *const () as usize,
        "fl_exit" => fl_exit as *const () as usize,
        "fl_now_ns" => fl_now_ns as *const () as usize,
        "fl_sleep_ms" => fl_sleep_ms as *const () as usize,
        "fl_args" => fl_args as *const () as usize,
        "fl_read_line" => fl_read_line as *const () as usize,
        "fl_str_upper" => fl_str_upper as *const () as usize,
        "fl_str_lower" => fl_str_lower as *const () as usize,
        "fl_str_trim" => fl_str_trim as *const () as usize,
        "fl_str_contains" => fl_str_contains as *const () as usize,
        "fl_str_starts_with" => fl_str_starts_with as *const () as usize,
        "fl_str_ends_with" => fl_str_ends_with as *const () as usize,
        "fl_str_replace" => fl_str_replace as *const () as usize,
        "fl_str_split" => fl_str_split as *const () as usize,
        "fl_str_join" => fl_str_join as *const () as usize,
        "fl_str_find" => fl_str_find as *const () as usize,
        "fl_str_substr" => fl_str_substr as *const () as usize,
        "fl_str_byte_at" => fl_str_byte_at as *const () as usize,
        "fl_parse_int" => fl_parse_int as *const () as usize,
        "fl_parse_float" => fl_parse_float as *const () as usize,
        "fl_read_file" => fl_read_file as *const () as usize,
        "fl_write_file" => fl_write_file as *const () as usize,
        "fl_append_file" => fl_append_file as *const () as usize,
        "fl_file_exists" => fl_file_exists as *const () as usize,
        "fl_remove_file" => fl_remove_file as *const () as usize,
        "fl_env_var" => fl_env_var as *const () as usize,
        "fl_path_join" => fl_path_join as *const () as usize,
        "fl_path_filename" => fl_path_filename as *const () as usize,
        "fl_path_parent" => fl_path_parent as *const () as usize,
        "fl_path_ext" => fl_path_ext as *const () as usize,
        "fl_result_error" => fl_result_error as *const () as usize,
        "fl_plugin_load" => fl_plugin_load as *const () as usize,
        "fl_plugin_call" => fl_plugin_call as *const () as usize,
        "floor" => rt_floor as *const () as usize,
        "ceil" => rt_ceil as *const () as usize,
        "trunc" => rt_trunc as *const () as usize,
        "sqrt" => rt_sqrt as *const () as usize,
        "memcpy" => libc::memcpy as *const () as usize,
        "memmove" => libc::memmove as *const () as usize,
        "memset" => libc::memset as *const () as usize,
        "memcmp" => libc::memcmp as *const () as usize,
        _ => return None,
    })
}
