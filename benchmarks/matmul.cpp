#include <cstdint>
#include <cstdio>
#include <vector>
using Mat = std::vector<std::vector<double>>;
static Mat make(int n, int seed) {
    Mat m;
    for (int i = 0; i < n; i++) {
        std::vector<double> row;
        for (int j = 0; j < n; j++) row.push_back((double)((i * 31 + j * 17 + seed) % 100) / 10.0);
        m.push_back(row);
    }
    return m;
}
static Mat multiply(const Mat& a, const Mat& b, int n) {
    Mat c;
    for (int i = 0; i < n; i++) {
        std::vector<double> row(n, 0.0);
        for (int k = 0; k < n; k++) {
            double aik = a[i][k];
            for (int j = 0; j < n; j++) row[j] = row[j] + aik * b[k][j];
        }
        c.push_back(row);
    }
    return c;
}
int main() {
    int n = 300;
    Mat a = make(n, 1), b = make(n, 2), c = multiply(a, b, n);
    double trace = 0.0;
    for (int i = 0; i < n; i++) trace = trace + c[i][i];
    printf("%lld\n", (long long)(trace * 1000.0));
}
