#include <cstdint>
#include <cstdio>
#include <string>
#include <vector>
struct Node {
    int64_t value;
    std::string label;
};
int main() {
    int64_t total = 0;
    for (int i = 0; i < 200000; i++) {
        std::vector<Node> nodes;
        for (int64_t j = 0; j < 20; j++) nodes.push_back(Node{j, "n" + std::to_string(j)});
        for (auto& nd : nodes) total += nd.value + (int64_t)nd.label.size();
    }
    printf("%lld\n", (long long)total);
}
