// C++ code exposed to Flash through a thin extern "C" wrapper (the only supported boundary).
#include <numeric>
#include <stdexcept>
#include <vector>

extern "C" double cpp_mean3(double a, double b, double c) {
    try {
        std::vector<double> v{a, b, c};
        return std::accumulate(v.begin(), v.end(), 0.0) / static_cast<double>(v.size());
    } catch (...) {  // exceptions must never cross the C boundary
        return 0.0;
    }
}
