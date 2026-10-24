#[derive(Clone)]
struct Body { x: f64, y: f64, z: f64, vx: f64, vy: f64, vz: f64, mass: f64 }

fn advance(bodies: &mut Vec<Body>, dt: f64) {
    let n = bodies.len();
    for i in 0..n {
        for j in (i + 1)..n {
            let dx = bodies[i].x - bodies[j].x;
            let dy = bodies[i].y - bodies[j].y;
            let dz = bodies[i].z - bodies[j].z;
            let d2 = dx * dx + dy * dy + dz * dz;
            let mag = dt / (d2 * d2.sqrt());
            let mi = bodies[i].mass;
            let mj = bodies[j].mass;
            bodies[i].vx = bodies[i].vx - dx * mj * mag;
            bodies[i].vy = bodies[i].vy - dy * mj * mag;
            bodies[i].vz = bodies[i].vz - dz * mj * mag;
            bodies[j].vx = bodies[j].vx + dx * mi * mag;
            bodies[j].vy = bodies[j].vy + dy * mi * mag;
            bodies[j].vz = bodies[j].vz + dz * mi * mag;
        }
    }
    for i in 0..n {
        bodies[i].x = bodies[i].x + dt * bodies[i].vx;
        bodies[i].y = bodies[i].y + dt * bodies[i].vy;
        bodies[i].z = bodies[i].z + dt * bodies[i].vz;
    }
}

fn energy(bodies: &Vec<Body>) -> f64 {
    let mut e = 0.0;
    let n = bodies.len();
    for i in 0..n {
        let b = &bodies[i];
        e = e + 0.5 * b.mass * (b.vx * b.vx + b.vy * b.vy + b.vz * b.vz);
        for j in (i + 1)..n {
            let dx = b.x - bodies[j].x;
            let dy = b.y - bodies[j].y;
            let dz = b.z - bodies[j].z;
            e = e - b.mass * bodies[j].mass / (dx * dx + dy * dy + dz * dz).sqrt();
        }
    }
    e
}

fn main() {
    let pi = 3.141592653589793f64;
    let solar = 4.0 * pi * pi;
    let dpy = 365.24f64;
    let mut bodies = vec![
        Body { x: 0.0, y: 0.0, z: 0.0, vx: 0.0, vy: 0.0, vz: 0.0, mass: solar },
        Body { x: 4.84143144246472090, y: -1.16032004402742839, z: -0.103622044471123109, vx: 0.00166007664274403694 * dpy, vy: 0.00769901118419740425 * dpy, vz: -0.0000690460016972063023 * dpy, mass: 0.000954791938424326609 * solar },
        Body { x: 8.34336671824457987, y: 4.12479856412430479, z: -0.403523417114321381, vx: -0.00276742510726862411 * dpy, vy: 0.00499852801234917238 * dpy, vz: 0.0000230417297573763929 * dpy, mass: 0.000285885980666130812 * solar },
        Body { x: 12.8943695621391310, y: -15.1111514016986312, z: -0.223307578892655734, vx: 0.00296460137564761618 * dpy, vy: 0.00237847173959480950 * dpy, vz: -0.0000296589568540237556 * dpy, mass: 0.0000436624404335156298 * solar },
        Body { x: 15.3796971148509165, y: -25.9193146099879641, z: 0.179258772950371181, vx: 0.00268067772490389322 * dpy, vy: 0.00162824170038242295 * dpy, vz: -0.0000951592254519715870 * dpy, mass: 0.0000515138902046611451 * solar },
    ];
    let (mut px, mut py, mut pz) = (0.0, 0.0, 0.0);
    for i in 0..bodies.len() {
        px = px + bodies[i].vx * bodies[i].mass;
        py = py + bodies[i].vy * bodies[i].mass;
        pz = pz + bodies[i].vz * bodies[i].mass;
    }
    bodies[0].vx = -px / solar;
    bodies[0].vy = -py / solar;
    bodies[0].vz = -pz / solar;
    println!("{}", (energy(&bodies) * 1000000000.0) as i64);
    for _ in 0..5000000 {
        advance(&mut bodies, 0.01);
    }
    println!("{}", (energy(&bodies) * 1000000000.0) as i64);
}
