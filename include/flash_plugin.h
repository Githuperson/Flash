/*
 * Flash native plugin ABI, version 1  (see PLUGIN_ABI.md)
 *
 * A Flash plugin is a shared library (.so / .dylib / .dll) that exports ONE symbol,
 * `flash_plugin_entry_v1`, returning a pointer to a static FlashPluginInfo describing the
 * plugin and its functions. Everything crossing the boundary is plain C data:
 *   - fixed-width integers, double, bool (as 0/1 in a 64-bit slot)
 *   - UTF-8 strings as (pointer, length) pairs - never NUL-terminated, never std::string
 *   - opaque handles as 64-bit integers
 *   - status codes (0 = success, non-zero = failure with a message in the return value)
 * No exception and no Rust panic may cross this boundary.
 *
 * OWNERSHIP
 *   - Arguments are BORROWED by the plugin for the duration of the call only.
 *     Copy anything you want to keep.
 *   - A string RETURNED by the plugin (result or error message) is allocated by the plugin.
 *     The host copies it and then calls FlashPluginInfo::free_buf(ptr, len) exactly once.
 */
#ifndef FLASH_PLUGIN_H
#define FLASH_PLUGIN_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define FLASH_ABI_VERSION 1u

#if defined(_WIN32)
#define FLASH_EXPORT __declspec(dllexport)
#else
#define FLASH_EXPORT __attribute__((visibility("default")))
#endif

enum FlashTag {
    FLASH_T_NONE = 0,
    FLASH_T_BOOL = 1, /* value in `a` (0 or 1) */
    FLASH_T_I64 = 2,  /* signed integers of any width, widened to 64 bits, in `a` */
    FLASH_T_U64 = 3,  /* unsigned integers of any width / opaque handles, in `a` */
    FLASH_T_F64 = 4,  /* IEEE double: bit pattern in `a` (use flash_get_f64 / flash_set_f64) */
    FLASH_T_STR = 5   /* UTF-8 bytes: pointer in `a`, length in `b` */
};

/* 24 bytes. Layout is part of the ABI. */
typedef struct FlashValue {
    uint32_t tag;
    uint32_t reserved;
    uint64_t a;
    uint64_t b;
} FlashValue;

typedef int32_t (*FlashFn)(const FlashValue* args, uint32_t nargs, FlashValue* ret);

typedef struct FlashFuncInfo {
    const char* name;           /* NUL-terminated */
    uint32_t nparams;
    uint32_t ret_tag;           /* FlashTag of the successful result */
    const uint32_t* param_tags; /* nparams entries */
    FlashFn func;
} FlashFuncInfo;

typedef struct FlashPluginInfo {
    uint32_t abi_version;  /* must equal FLASH_ABI_VERSION */
    uint32_t struct_size;  /* sizeof(FlashPluginInfo) */
    const char* name;      /* must match the manifest */
    const char* version;
    uint32_t nfuncs;
    const FlashFuncInfo* funcs;
    void (*free_buf)(uint8_t* ptr, uint64_t len);
} FlashPluginInfo;

FLASH_EXPORT const FlashPluginInfo* flash_plugin_entry_v1(void);

/* ---- small inline helpers (C and C++) ---- */
#include <string.h>

static inline double flash_get_f64(const FlashValue* v) {
    double d;
    memcpy(&d, &v->a, sizeof d);
    return d;
}
static inline void flash_set_f64(FlashValue* r, double d) {
    r->tag = FLASH_T_F64;
    memcpy(&r->a, &d, sizeof d);
    r->b = 0;
}
static inline void flash_set_i64(FlashValue* r, int64_t x) {
    r->tag = FLASH_T_I64;
    r->a = (uint64_t)x;
    r->b = 0;
}
static inline void flash_set_u64(FlashValue* r, uint64_t x) {
    r->tag = FLASH_T_U64;
    r->a = x;
    r->b = 0;
}
static inline void flash_set_bool(FlashValue* r, int x) {
    r->tag = FLASH_T_BOOL;
    r->a = x ? 1 : 0;
    r->b = 0;
}

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* FLASH_PLUGIN_H */
