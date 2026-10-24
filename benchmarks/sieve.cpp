#include <cstdio>
#include <vector>
int main() {
    long n = 20000000;
    std::vector<bool> dummy;  // (kept out of the hot path; std::vector<char> is the fair analogue of List[Bool])
    std::vector<char> is_prime(n + 1, 1);
    is_prime[0] = 0;
    is_prime[1] = 0;
    for (long i = 2; i * i <= n; i++) {
        if (is_prime[i]) {
            for (long j = i * i; j <= n; j += i) is_prime[j] = 0;
        }
    }
    long count = 0;
    for (long k = 0; k <= n; k++)
        if (is_prime[k]) count++;
    printf("%ld\n", count);
}
