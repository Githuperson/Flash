/* A small C library used by examples/static_link/main.flash through `extern fn`.
 * It is linked statically: no plugin manifest, no dlopen. */
#include <math.h>
#include <stdint.h>

int64_t c_gcd(int64_t a, int64_t b) {
    if (a < 0) a = -a;
    if (b < 0) b = -b;
    while (b != 0) {
        int64_t t = a % b;
        a = b;
        b = t;
    }
    return a;
}

double c_hypot(double x, double y) { return sqrt(x * x + y * y); }

/* A Flash String arrives as (pointer, byte length); it is only borrowed for the call. */
uint64_t c_count_byte(const uint8_t* text, uint64_t len, uint8_t needle) {
    uint64_t n = 0;
    for (uint64_t i = 0; i < len; i++) n += (text[i] == needle);
    return n;
}

int32_t c_sign(int32_t x) { return (x > 0) - (x < 0); }

_Bool c_is_even(int64_t n) { return (n % 2) == 0; }

static int64_t counter = 0;
void c_bump(int64_t by) { counter += by; }
int64_t c_counter(void) { return counter; }
