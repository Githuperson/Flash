//! Magic numbers for division by constants (Hacker's Delight, chapter 10): a division becomes a
//! multiplication by a fixed-point reciprocal plus a few shifts.

/// Signed: `n / d` = high64(n * m) (+ n if d > 0 and m < 0, − n if d < 0 and m > 0) >> s, plus
/// the sign bit. Requires |d| >= 2.
pub fn signed(d: i64) -> (i64, u32) {
    let two63: u64 = 1 << 63;
    let ad = d.unsigned_abs();
    let t = two63 + ((d as u64) >> 63);
    let anc = t - 1 - t % ad;
    let mut p = 63u32;
    let mut q1 = two63 / anc;
    let mut r1 = two63 - q1 * anc;
    let mut q2 = two63 / ad;
    let mut r2 = two63 - q2 * ad;
    loop {
        p += 1;
        q1 = q1.wrapping_mul(2);
        r1 = r1.wrapping_mul(2);
        if r1 >= anc {
            q1 = q1.wrapping_add(1);
            r1 = r1.wrapping_sub(anc);
        }
        q2 = q2.wrapping_mul(2);
        r2 = r2.wrapping_mul(2);
        if r2 >= ad {
            q2 = q2.wrapping_add(1);
            r2 = r2.wrapping_sub(ad);
        }
        let delta = ad - r2;
        if !(q1 < delta || (q1 == delta && r1 == 0)) {
            break;
        }
    }
    let m = q2.wrapping_add(1) as i64;
    (if d < 0 { m.wrapping_neg() } else { m }, p - 64)
}

/// Unsigned: returns (m, add, s). `n / d` = high64(n * m) >> s, or, when `add`,
/// (((n − h) >> 1) + h) >> (s − 1) with h = high64(n * m). Requires d >= 2.
pub fn unsigned(d: u64) -> (u64, bool, u32) {
    let mut add = false;
    let nc = u64::MAX - 0u64.wrapping_sub(d) % d;
    let mut p = 63u32;
    let mut q1 = (1u64 << 63) / nc;
    let mut r1 = (1u64 << 63) - q1 * nc;
    let mut q2 = ((1u64 << 63) - 1) / d;
    let mut r2 = ((1u64 << 63) - 1) - q2 * d;
    loop {
        p += 1;
        if r1 >= nc - r1 {
            q1 = q1.wrapping_mul(2).wrapping_add(1);
            r1 = r1.wrapping_mul(2).wrapping_sub(nc);
        } else {
            q1 = q1.wrapping_mul(2);
            r1 = r1.wrapping_mul(2);
        }
        if r2 + 1 >= d - r2 {
            if q2 >= (1u64 << 63) - 1 {
                add = true;
            }
            q2 = q2.wrapping_mul(2).wrapping_add(1);
            r2 = r2.wrapping_mul(2).wrapping_add(1).wrapping_sub(d);
        } else {
            if q2 >= 1u64 << 63 {
                add = true;
            }
            q2 = q2.wrapping_mul(2);
            r2 = r2.wrapping_mul(2).wrapping_add(1);
        }
        let delta = d - 1 - r2;
        if !(p < 128 && (q1 < delta || (q1 == delta && r1 == 0))) {
            break;
        }
    }
    (q2.wrapping_add(1), add, p - 64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn div_signed(n: i64, d: i64) -> i64 {
        let (m, s) = signed(d);
        let mut h = ((n as i128 * m as i128) >> 64) as i64;
        if d > 0 && m < 0 {
            h = h.wrapping_add(n);
        }
        if d < 0 && m > 0 {
            h = h.wrapping_sub(n);
        }
        h >>= s;
        h.wrapping_add(((h as u64) >> 63) as i64)
    }

    fn div_unsigned(n: u64, d: u64) -> u64 {
        let (m, add, s) = unsigned(d);
        let h = ((n as u128 * m as u128) >> 64) as u64;
        if add {
            (((n - h) >> 1) + h) >> (s - 1)
        } else {
            h >> s
        }
    }

    #[test]
    fn magic_division_matches_hardware_division() {
        let mut x: u64 = 0x1234_5678_9ABC_DEF1;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut divisors: Vec<i64> = vec![3, 5, 6, 7, 9, 10, 11, 12, 13, 25, 100, 641, 1000, 65537, 1_000_000_007, i64::MAX, 3 << 40, -3, -7, -10, -1000, i64::MIN + 1];
        for _ in 0..300 {
            divisors.push((next() % 1_000_000_000) as i64 + 2);
            divisors.push(next() as i64 | 3);
        }
        let edge = [0i64, 1, -1, 2, -2, i64::MAX, i64::MIN, i64::MAX - 1, i64::MIN + 1, 1 << 62, -(1 << 62)];
        for &d in &divisors {
            if d.unsigned_abs() < 2 || d.unsigned_abs().is_power_of_two() {
                continue;
            }
            for &n in edge.iter().chain((0..200).map(|_| next() as i64).collect::<Vec<_>>().iter()) {
                assert_eq!(div_signed(n, d), n.wrapping_div(d), "{} / {}", n, d);
            }
            let du = d as u64;
            if du >= 2 {
                for &n in edge.iter().map(|e| *e as u64).collect::<Vec<_>>().iter().chain((0..200).map(|_| next()).collect::<Vec<_>>().iter()) {
                    assert_eq!(div_unsigned(n, du), n / du, "{} / {} (unsigned)", n, du);
                }
            }
        }
    }
}
