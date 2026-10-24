#include <cmath>
#include <cstdint>
#include <cstdio>
#include <vector>
struct Body { double x, y, z, vx, vy, vz, mass; };

static void advance(std::vector<Body>& bodies, double dt) {
    size_t n = bodies.size();
    for (size_t i = 0; i < n; i++) {
        for (size_t j = i + 1; j < n; j++) {
            double dx = bodies[i].x - bodies[j].x;
            double dy = bodies[i].y - bodies[j].y;
            double dz = bodies[i].z - bodies[j].z;
            double d2 = dx * dx + dy * dy + dz * dz;
            double mag = dt / (d2 * std::sqrt(d2));
            double mi = bodies[i].mass;
            double mj = bodies[j].mass;
            bodies[i].vx = bodies[i].vx - dx * mj * mag;
            bodies[i].vy = bodies[i].vy - dy * mj * mag;
            bodies[i].vz = bodies[i].vz - dz * mj * mag;
            bodies[j].vx = bodies[j].vx + dx * mi * mag;
            bodies[j].vy = bodies[j].vy + dy * mi * mag;
            bodies[j].vz = bodies[j].vz + dz * mi * mag;
        }
    }
    for (size_t i = 0; i < n; i++) {
        bodies[i].x = bodies[i].x + dt * bodies[i].vx;
        bodies[i].y = bodies[i].y + dt * bodies[i].vy;
        bodies[i].z = bodies[i].z + dt * bodies[i].vz;
    }
}

static double energy(const std::vector<Body>& bodies) {
    double e = 0.0;
    size_t n = bodies.size();
    for (size_t i = 0; i < n; i++) {
        const Body& b = bodies[i];
        e = e + 0.5 * b.mass * (b.vx * b.vx + b.vy * b.vy + b.vz * b.vz);
        for (size_t j = i + 1; j < n; j++) {
            double dx = b.x - bodies[j].x;
            double dy = b.y - bodies[j].y;
            double dz = b.z - bodies[j].z;
            e = e - b.mass * bodies[j].mass / std::sqrt(dx * dx + dy * dy + dz * dz);
        }
    }
    return e;
}

int main() {
    double pi = 3.141592653589793;
    double solar = 4.0 * pi * pi;
    double dpy = 365.24;
    std::vector<Body> bodies = {
        {0.0, 0.0, 0.0, 0.0, 0.0, 0.0, solar},
        {4.84143144246472090, -1.16032004402742839, -0.103622044471123109, 0.00166007664274403694 * dpy, 0.00769901118419740425 * dpy, -0.0000690460016972063023 * dpy, 0.000954791938424326609 * solar},
        {8.34336671824457987, 4.12479856412430479, -0.403523417114321381, -0.00276742510726862411 * dpy, 0.00499852801234917238 * dpy, 0.0000230417297573763929 * dpy, 0.000285885980666130812 * solar},
        {12.8943695621391310, -15.1111514016986312, -0.223307578892655734, 0.00296460137564761618 * dpy, 0.00237847173959480950 * dpy, -0.0000296589568540237556 * dpy, 0.0000436624404335156298 * solar},
        {15.3796971148509165, -25.9193146099879641, 0.179258772950371181, 0.00268067772490389322 * dpy, 0.00162824170038242295 * dpy, -0.0000951592254519715870 * dpy, 0.0000515138902046611451 * solar},
    };
    double px = 0.0, py = 0.0, pz = 0.0;
    for (size_t i = 0; i < bodies.size(); i++) {
        px = px + bodies[i].vx * bodies[i].mass;
        py = py + bodies[i].vy * bodies[i].mass;
        pz = pz + bodies[i].vz * bodies[i].mass;
    }
    bodies[0].vx = -px / solar;
    bodies[0].vy = -py / solar;
    bodies[0].vz = -pz / solar;
    printf("%lld\n", (long long)(energy(bodies) * 1000000000.0));
    for (int s = 0; s < 5000000; s++) advance(bodies, 0.01);
    printf("%lld\n", (long long)(energy(bodies) * 1000000000.0));
}
