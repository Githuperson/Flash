#include <cstdio>
#include <string>
#include <vector>
int main() {
    std::vector<std::string> words;
    for (int i = 0; i < 300000; i++) words.push_back("w" + std::to_string(i));
    std::string text;
    for (size_t i = 0; i < words.size(); i++) {
        if (i > 0) text += " ";
        text += words[i];
    }
    std::vector<std::string> parts;
    size_t start = 0;
    while (true) {
        size_t pos = text.find(' ', start);
        if (pos == std::string::npos) {
            parts.push_back(text.substr(start));
            break;
        }
        parts.push_back(text.substr(start, pos - start));
        start = pos + 1;
    }
    size_t total = 0;
    long sevens = 0;
    for (auto& p : parts) {
        total += p.size();
        if (p.find('7') != std::string::npos) sevens++;
    }
    printf("%zu %zu %zu %ld\n", text.size(), parts.size(), total, sevens);
}
