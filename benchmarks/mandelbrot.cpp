#include <cstdint>
#include <cstdio>
static int64_t mandel(double cx, double cy, int64_t max_iter) {
    double x = 0.0, y = 0.0;
    int64_t i = 0;
    while (i < max_iter && x * x + y * y <= 4.0) {
        double t = x * x - y * y + cx;
        y = 2.0 * x * y + cy;
        x = t;
        i++;
    }
    return i;
}
int main() {
    int64_t size = 700, total = 0;
    for (int64_t py = 0; py < size; py++)
        for (int64_t px = 0; px < size; px++) {
            double cx = (double)px / (double)size * 3.0 - 2.0;
            double cy = (double)py / (double)size * 2.0 - 1.0;
            total += mandel(cx, cy, 300);
        }
    printf("%lld\n", (long long)total);
}
