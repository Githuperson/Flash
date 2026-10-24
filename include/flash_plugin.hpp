// C++ helpers for writing Flash plugins. Thin layer over flash_plugin.h (see PLUGIN_ABI.md).
//
// Rules this header enforces for you:
//   * C++ exceptions never cross the ABI: FLASH_GUARD turns them into an error status.
//   * Returned strings are allocated with new[] and released by flash_cpp_free_buf.
#ifndef FLASH_PLUGIN_HPP
#define FLASH_PLUGIN_HPP

#include "flash_plugin.h"

#include <cstring>
#include <exception>
#include <string>
#include <string_view>

namespace flash {

inline std::string_view get_str(const FlashValue& v) {
    return std::string_view(reinterpret_cast<const char*>(v.a), static_cast<size_t>(v.b));
}

inline void set_str(FlashValue* r, const std::string& s) {
    auto* buf = new uint8_t[s.size() ? s.size() : 1];
    std::memcpy(buf, s.data(), s.size());
    r->tag = FLASH_T_STR;
    r->a = reinterpret_cast<uint64_t>(buf);
    r->b = s.size();
}

}  // namespace flash

extern "C" inline void flash_cpp_free_buf(uint8_t* ptr, uint64_t) { delete[] ptr; }

// Wrap the body of a plugin function: any exception becomes status 2 with its message.
#define FLASH_GUARD(ret, ...)                                              \
    try {                                                                  \
        __VA_ARGS__                                                        \
    } catch (const std::exception& e) {                                    \
        flash::set_str((ret), e.what());                                   \
        return 2;                                                          \
    } catch (...) {                                                        \
        flash::set_str((ret), "unknown C++ exception in plugin");          \
        return 2;                                                          \
    }

#endif  // FLASH_PLUGIN_HPP
