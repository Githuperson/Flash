// Example Flash plugin written in C++: `use cpp_plugin "geometry"`.
//
// Shows: scalar functions, a string-returning function (std::string stays inside the plugin),
// a fallible function (errors, including C++ exceptions, become Flash Results), and a C++ class
// exposed to Flash through an opaque 64-bit handle with explicit create/use/free wrappers.

#include "flash_plugin.hpp"

#include <cmath>
#include <stdexcept>
#include <string>

namespace {

class Counter {
public:
    explicit Counter(int64_t start) : value_(start) {}
    void add(int64_t n) { value_ += n; }
    int64_t value() const { return value_; }

private:
    int64_t value_;
};

int32_t fn_distance(const FlashValue* a, uint32_t, FlashValue* ret) {
    double dx = flash_get_f64(&a[0]) - flash_get_f64(&a[2]);
    double dy = flash_get_f64(&a[1]) - flash_get_f64(&a[3]);
    flash_set_f64(ret, std::sqrt(dx * dx + dy * dy));
    return 0;
}

int32_t fn_circle_area(const FlashValue* a, uint32_t, FlashValue* ret) {
    double r = flash_get_f64(&a[0]);
    flash_set_f64(ret, 3.141592653589793 * r * r);
    return 0;
}

int32_t fn_clamp(const FlashValue* a, uint32_t, FlashValue* ret) {
    int64_t v = static_cast<int64_t>(a[0].a), lo = static_cast<int64_t>(a[1].a), hi = static_cast<int64_t>(a[2].a);
    flash_set_i64(ret, v < lo ? lo : (v > hi ? hi : v));
    return 0;
}

int32_t fn_describe(const FlashValue* a, uint32_t, FlashValue* ret) {
    FLASH_GUARD(ret, {
        std::string name(flash::get_str(a[0]));
        flash::set_str(ret, "Window '" + name + "' (800x600) created by the C++ geometry plugin");
        return 0;
    })
}

int32_t fn_checked_sqrt(const FlashValue* a, uint32_t, FlashValue* ret) {
    FLASH_GUARD(ret, {
        double x = flash_get_f64(&a[0]);
        if (x < 0) throw std::invalid_argument("cannot take the square root of a negative number");
        flash_set_f64(ret, std::sqrt(x));
        return 0;
    })
}

int32_t fn_always_throws(const FlashValue*, uint32_t, FlashValue* ret) {
    FLASH_GUARD(ret, {
        throw std::runtime_error("deliberate C++ exception");
        return 0;
    })
}

// ---- Counter class behind an opaque handle ----
int32_t fn_counter_new(const FlashValue* a, uint32_t, FlashValue* ret) {
    FLASH_GUARD(ret, {
        flash_set_u64(ret, reinterpret_cast<uint64_t>(new Counter(static_cast<int64_t>(a[0].a))));
        return 0;
    })
}
int32_t fn_counter_add(const FlashValue* a, uint32_t, FlashValue* ret) {
    reinterpret_cast<Counter*>(a[0].a)->add(static_cast<int64_t>(a[1].a));
    ret->tag = FLASH_T_NONE;
    return 0;
}
int32_t fn_counter_value(const FlashValue* a, uint32_t, FlashValue* ret) {
    flash_set_i64(ret, reinterpret_cast<Counter*>(a[0].a)->value());
    return 0;
}
int32_t fn_counter_free(const FlashValue* a, uint32_t, FlashValue* ret) {
    delete reinterpret_cast<Counter*>(a[0].a);
    ret->tag = FLASH_T_NONE;
    return 0;
}

const uint32_t P_F4[] = {FLASH_T_F64, FLASH_T_F64, FLASH_T_F64, FLASH_T_F64};
const uint32_t P_F1[] = {FLASH_T_F64};
const uint32_t P_I3[] = {FLASH_T_I64, FLASH_T_I64, FLASH_T_I64};
const uint32_t P_S1[] = {FLASH_T_STR};
const uint32_t P_I1[] = {FLASH_T_I64};
const uint32_t P_U1[] = {FLASH_T_U64};
const uint32_t P_U1_I1[] = {FLASH_T_U64, FLASH_T_I64};

const FlashFuncInfo FUNCS[] = {
    {"distance", 4, FLASH_T_F64, P_F4, fn_distance},
    {"circle_area", 1, FLASH_T_F64, P_F1, fn_circle_area},
    {"clamp", 3, FLASH_T_I64, P_I3, fn_clamp},
    {"describe", 1, FLASH_T_STR, P_S1, fn_describe},
    {"checked_sqrt", 1, FLASH_T_F64, P_F1, fn_checked_sqrt},
    {"always_throws", 0, FLASH_T_I64, nullptr, fn_always_throws},
    {"counter_new", 1, FLASH_T_U64, P_I1, fn_counter_new},
    {"counter_add", 2, FLASH_T_NONE, P_U1_I1, fn_counter_add},
    {"counter_value", 1, FLASH_T_I64, P_U1, fn_counter_value},
    {"counter_free", 1, FLASH_T_NONE, P_U1, fn_counter_free},
};

const FlashPluginInfo INFO = {
    FLASH_ABI_VERSION, sizeof(FlashPluginInfo), "geometry", "0.1.0",
    sizeof(FUNCS) / sizeof(FUNCS[0]), FUNCS, flash_cpp_free_buf,
};

}  // namespace

extern "C" FLASH_EXPORT const FlashPluginInfo* flash_plugin_entry_v1(void) { return &INFO; }
