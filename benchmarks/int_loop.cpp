#include <cstdint>
#include <cstdio>
int main() {
    int64_t total = 0;
    for (int64_t i = 0; i < 400000000; i++) {
        total += (i * i) % 7 + (i & 3);
    }
    printf("%lld\n", (long long)total);
}
