#![allow(dead_code)]
//! AArch64 machine-code encoder: the instruction forms the Flash code generator needs.
//!
//! Registers are numbered 0..=30; 31 means `sp` or `xzr` depending on the instruction (the
//! constants `SP` and `XZR` make call sites readable). Floating-point registers are `d0..d31`
//! (the 64-bit view of `v0..v31`). Every emitter appends one 32-bit little-endian word; branch
//! targets are labels patched by `resolve_labels`. The encodings are checked against `llvm-mc` by
//! the unit tests in this file.

pub type Reg = u8;

pub const SP: Reg = 31;
pub const XZR: Reg = 31;
pub const LR: Reg = 30;
pub const FP: Reg = 29;

/// Condition codes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cond {
    Eq = 0,
    Ne = 1,
    Hs = 2,
    Lo = 3,
    Mi = 4,
    Pl = 5,
    Vs = 6,
    Vc = 7,
    Hi = 8,
    Ls = 9,
    Ge = 10,
    Lt = 11,
    Gt = 12,
    Le = 13,
}

impl Cond {
    pub fn invert(self) -> Cond {
        use Cond::*;
        match self {
            Eq => Ne,
            Ne => Eq,
            Hs => Lo,
            Lo => Hs,
            Mi => Pl,
            Pl => Mi,
            Vs => Vc,
            Vc => Vs,
            Hi => Ls,
            Ls => Hi,
            Ge => Lt,
            Lt => Ge,
            Gt => Le,
            Le => Gt,
        }
    }
}

/// Shift applied to the second operand of a register instruction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shift {
    Lsl = 0,
    Lsr = 1,
    Asr = 2,
}

/// Width of a memory access.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Width {
    B,
    H,
    W,
    X,
}

impl Width {
    fn log2(self) -> u32 {
        match self {
            Width::B => 0,
            Width::H => 1,
            Width::W => 2,
            Width::X => 3,
        }
    }
}

pub type Label = usize;

#[derive(Clone, Copy)]
enum FixKind {
    /// `b` / `bl`: 26-bit word offset
    B26,
    /// `b.cond` / `cbz` / `cbnz`: 19-bit word offset at bit 5
    Imm19,
}

#[derive(Default)]
pub struct Asm {
    pub buf: Vec<u8>,
    labels: Vec<Option<usize>>,
    fixups: Vec<(usize, Label, FixKind)>,
}

impl Asm {
    pub fn new() -> Asm {
        Asm::default()
    }

    pub fn pos(&self) -> usize {
        self.buf.len()
    }

    pub fn new_label(&mut self) -> Label {
        self.labels.push(None);
        self.labels.len() - 1
    }

    pub fn bind(&mut self, l: Label) {
        debug_assert!(self.labels[l].is_none());
        self.labels[l] = Some(self.buf.len());
    }

    pub fn align_to(&mut self, n: usize) {
        while self.buf.len() % n != 0 {
            self.word(0xD503_201F); // nop
        }
    }

    pub fn word(&mut self, w: u32) {
        self.buf.extend_from_slice(&w.to_le_bytes());
    }

    pub fn patch_word(&mut self, at: usize, w: u32) {
        self.buf[at..at + 4].copy_from_slice(&w.to_le_bytes());
    }

    pub fn read_word(&self, at: usize) -> u32 {
        u32::from_le_bytes(self.buf[at..at + 4].try_into().unwrap())
    }

    /// Patch all label references; fails if a displacement does not fit its field.
    pub fn resolve_labels(&mut self) -> Result<(), String> {
        for i in 0..self.fixups.len() {
            let (at, l, kind) = self.fixups[i];
            let target = self.labels[l].ok_or("internal error: branch to an unbound label")?;
            let delta = (target as i64 - at as i64) / 4;
            let w = self.read_word(at);
            let patched = match kind {
                FixKind::B26 => {
                    if !(-(1 << 25)..(1 << 25)).contains(&delta) {
                        return Err("a branch target is more than 128 MiB away".into());
                    }
                    w | (delta as u32 & 0x03FF_FFFF)
                }
                FixKind::Imm19 => {
                    if !(-(1 << 18)..(1 << 18)).contains(&delta) {
                        return Err("a function is too large for conditional branches (over 1 MiB)".into());
                    }
                    w | ((delta as u32 & 0x7_FFFF) << 5)
                }
            };
            self.patch_word(at, patched);
        }
        self.fixups.clear();
        Ok(())
    }

    // ------------------------------------------------------------- moves and constants
    /// `mov xd, xn` (register copy)
    pub fn mov(&mut self, rd: Reg, rn: Reg) {
        self.word(0xAA00_03E0 | (rn as u32) << 16 | rd as u32);
    }

    /// `mov wd, wn`: copy the low 32 bits and clear the upper half.
    pub fn mov32(&mut self, rd: Reg, rn: Reg) {
        self.word(0x2A00_03E0 | (rn as u32) << 16 | rd as u32);
    }

    /// `mov xd, sp` / `mov sp, xn`
    pub fn mov_sp(&mut self, rd: Reg, rn: Reg) {
        self.add_imm(rd, rn, 0);
    }

    pub fn movz(&mut self, rd: Reg, imm16: u16, hw: u32) {
        self.word(0xD280_0000 | hw << 21 | (imm16 as u32) << 5 | rd as u32);
    }

    pub fn movk(&mut self, rd: Reg, imm16: u16, hw: u32) {
        self.word(0xF280_0000 | hw << 21 | (imm16 as u32) << 5 | rd as u32);
    }

    pub fn movn(&mut self, rd: Reg, imm16: u16, hw: u32) {
        self.word(0x9280_0000 | hw << 21 | (imm16 as u32) << 5 | rd as u32);
    }

    /// Load a 64-bit constant with the shortest sequence of `movz`/`movn`/`movk`.
    pub fn mov_imm(&mut self, rd: Reg, v: i64) {
        let u = v as u64;
        let halves = [(u & 0xFFFF) as u16, (u >> 16 & 0xFFFF) as u16, (u >> 32 & 0xFFFF) as u16, (u >> 48 & 0xFFFF) as u16];
        let zeros = halves.iter().filter(|h| **h == 0).count();
        let ones = halves.iter().filter(|h| **h == 0xFFFF).count();
        if ones > zeros {
            // start from all ones: movn, then patch the halves that are not 0xFFFF
            let first = halves.iter().position(|h| *h != 0xFFFF);
            match first {
                None => self.movn(rd, 0, 0),
                Some(i) => {
                    self.movn(rd, !halves[i], i as u32);
                    for (j, h) in halves.iter().enumerate() {
                        if j != i && *h != 0xFFFF {
                            self.movk(rd, *h, j as u32);
                        }
                    }
                }
            }
        } else {
            let first = halves.iter().position(|h| *h != 0);
            match first {
                None => self.movz(rd, 0, 0),
                Some(i) => {
                    self.movz(rd, halves[i], i as u32);
                    for (j, h) in halves.iter().enumerate() {
                        if j != i && *h != 0 {
                            self.movk(rd, *h, j as u32);
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------- integer arithmetic
    fn addsub_imm(&mut self, base: u32, rd: Reg, rn: Reg, imm: u32) {
        debug_assert!(imm < 4096 || (imm & 0xFFF == 0 && imm >> 12 < 4096));
        let (sh, v) = if imm < 4096 { (0, imm) } else { (1, imm >> 12) };
        self.word(base | sh << 22 | v << 10 | (rn as u32) << 5 | rd as u32);
    }

    pub fn add_imm(&mut self, rd: Reg, rn: Reg, imm: u32) {
        self.addsub_imm(0x9100_0000, rd, rn, imm);
    }
    pub fn sub_imm(&mut self, rd: Reg, rn: Reg, imm: u32) {
        self.addsub_imm(0xD100_0000, rd, rn, imm);
    }
    /// `cmp xn, #imm` (subs xzr, xn, #imm)
    pub fn cmp_imm(&mut self, rn: Reg, imm: u32) {
        self.addsub_imm(0xF100_0000, XZR, rn, imm);
    }
    /// `cmn xn, #imm` (adds xzr, xn, #imm): compare with the negative of `imm`
    pub fn cmn_imm(&mut self, rn: Reg, imm: u32) {
        self.addsub_imm(0xB100_0000, XZR, rn, imm);
    }
    /// `cmp wn, #imm`
    pub fn cmp32_imm(&mut self, rn: Reg, imm: u32) {
        self.addsub_imm(0x7100_0000, XZR, rn, imm);
    }

    fn rrr(&mut self, base: u32, rd: Reg, rn: Reg, rm: Reg) {
        self.word(base | (rm as u32) << 16 | (rn as u32) << 5 | rd as u32);
    }

    pub fn add(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.rrr(0x8B00_0000, rd, rn, rm);
    }
    pub fn sub(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.rrr(0xCB00_0000, rd, rn, rm);
    }
    /// `add xd, xn, xm, lsl #amount`
    pub fn add_lsl(&mut self, rd: Reg, rn: Reg, rm: Reg, amount: u32) {
        self.word(0x8B00_0000 | (rm as u32) << 16 | amount << 10 | (rn as u32) << 5 | rd as u32);
    }
    pub fn add32(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.rrr(0x0B00_0000, rd, rn, rm);
    }
    pub fn sub32(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.rrr(0x4B00_0000, rd, rn, rm);
    }
    /// `cmp xn, xm`
    pub fn cmp(&mut self, rn: Reg, rm: Reg) {
        self.rrr(0xEB00_0000, XZR, rn, rm);
    }
    /// `cmp wn, wm`
    pub fn cmp32(&mut self, rn: Reg, rm: Reg) {
        self.rrr(0x6B00_0000, XZR, rn, rm);
    }
    /// `neg xd, xm`
    pub fn neg(&mut self, rd: Reg, rm: Reg) {
        self.rrr(0xCB00_0000, rd, XZR, rm);
    }
    pub fn neg32(&mut self, rd: Reg, rm: Reg) {
        self.rrr(0x4B00_0000, rd, XZR, rm);
    }
    /// `negs xd, xm` (sets the flags like `subs xd, xzr, xm`)
    pub fn negs(&mut self, rd: Reg, rm: Reg) {
        self.rrr(0xEB00_0000, rd, XZR, rm);
    }

    pub fn and(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.rrr(0x8A00_0000, rd, rn, rm);
    }
    pub fn orr(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.rrr(0xAA00_0000, rd, rn, rm);
    }
    pub fn eor(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.rrr(0xCA00_0000, rd, rn, rm);
    }
    /// `tst xn, xm`
    pub fn tst(&mut self, rn: Reg, rm: Reg) {
        self.rrr(0xEA00_0000, XZR, rn, rm);
    }
    /// `mvn xd, xm`
    pub fn mvn(&mut self, rd: Reg, rm: Reg) {
        self.rrr(0xAA20_0000, rd, XZR, rm);
    }
    pub fn mvn32(&mut self, rd: Reg, rm: Reg) {
        self.rrr(0x2A20_0000, rd, XZR, rm);
    }

    fn logical_imm(&mut self, base: u32, rd: Reg, rn: Reg, value: u64) -> bool {
        match encode_bitmask(value) {
            Some((n, immr, imms)) => {
                self.word(base | n << 22 | immr << 16 | imms << 10 | (rn as u32) << 5 | rd as u32);
                true
            }
            None => false,
        }
    }
    /// `and xd, xn, #value` if the constant is encodable as a bitmask immediate.
    pub fn and_imm(&mut self, rd: Reg, rn: Reg, value: u64) -> bool {
        self.logical_imm(0x9200_0000, rd, rn, value)
    }
    pub fn orr_imm(&mut self, rd: Reg, rn: Reg, value: u64) -> bool {
        self.logical_imm(0xB200_0000, rd, rn, value)
    }
    pub fn eor_imm(&mut self, rd: Reg, rn: Reg, value: u64) -> bool {
        self.logical_imm(0xD200_0000, rd, rn, value)
    }

    pub fn mul(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.word(0x9B00_7C00 | (rm as u32) << 16 | (rn as u32) << 5 | rd as u32);
    }
    pub fn mul32(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.word(0x1B00_7C00 | (rm as u32) << 16 | (rn as u32) << 5 | rd as u32);
    }
    /// `msub xd, xn, xm, xa` = xa - xn * xm
    pub fn msub(&mut self, rd: Reg, rn: Reg, rm: Reg, ra: Reg) {
        self.word(0x9B00_8000 | (rm as u32) << 16 | (ra as u32) << 10 | (rn as u32) << 5 | rd as u32);
    }
    pub fn smulh(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.rrr(0x9B40_7C00, rd, rn, rm);
    }
    pub fn umulh(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.rrr(0x9BC0_7C00, rd, rn, rm);
    }
    pub fn sdiv(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.rrr(0x9AC0_0C00, rd, rn, rm);
    }
    pub fn udiv(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.rrr(0x9AC0_0800, rd, rn, rm);
    }

    pub fn lslv(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.rrr(0x9AC0_2000, rd, rn, rm);
    }
    pub fn lsrv(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.rrr(0x9AC0_2400, rd, rn, rm);
    }
    pub fn asrv(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.rrr(0x9AC0_2800, rd, rn, rm);
    }
    pub fn lslv32(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.rrr(0x1AC0_2000, rd, rn, rm);
    }
    pub fn lsrv32(&mut self, rd: Reg, rn: Reg, rm: Reg) {
        self.rrr(0x1AC0_2400, rd, rn, rm);
    }

    /// `lsl xd, xn, #n`
    pub fn lsl_imm(&mut self, rd: Reg, rn: Reg, n: u32) {
        debug_assert!((1..64).contains(&n));
        self.ubfm(rd, rn, (64 - n) % 64, 63 - n);
    }
    pub fn lsr_imm(&mut self, rd: Reg, rn: Reg, n: u32) {
        debug_assert!((1..64).contains(&n));
        self.ubfm(rd, rn, n, 63);
    }
    pub fn asr_imm(&mut self, rd: Reg, rn: Reg, n: u32) {
        debug_assert!((1..64).contains(&n));
        self.sbfm(rd, rn, n, 63);
    }
    /// `lsl wd, wn, #n` (32-bit; clears the upper half)
    pub fn lsl32_imm(&mut self, rd: Reg, rn: Reg, n: u32) {
        debug_assert!((1..32).contains(&n));
        self.word(0x5300_0000 | ((32 - n) % 32) << 16 | (31 - n) << 10 | (rn as u32) << 5 | rd as u32);
    }
    pub fn lsr32_imm(&mut self, rd: Reg, rn: Reg, n: u32) {
        debug_assert!((1..32).contains(&n));
        self.word(0x5300_0000 | n << 16 | 31 << 10 | (rn as u32) << 5 | rd as u32);
    }

    pub fn ubfm(&mut self, rd: Reg, rn: Reg, immr: u32, imms: u32) {
        self.word(0xD340_0000 | immr << 16 | imms << 10 | (rn as u32) << 5 | rd as u32);
    }
    pub fn sbfm(&mut self, rd: Reg, rn: Reg, immr: u32, imms: u32) {
        self.word(0x9340_0000 | immr << 16 | imms << 10 | (rn as u32) << 5 | rd as u32);
    }
    /// Zero-extend the low `bits` (8, 16 or 32) of `rn` into `rd`.
    pub fn uxt(&mut self, bits: u32, rd: Reg, rn: Reg) {
        match bits {
            8 => self.word(0x5300_1C00 | (rn as u32) << 5 | rd as u32),
            16 => self.word(0x5300_3C00 | (rn as u32) << 5 | rd as u32),
            _ => self.mov32(rd, rn),
        }
    }
    /// Sign-extend the low `bits` (8, 16 or 32) of `rn` into the 64-bit `rd`.
    pub fn sxt(&mut self, bits: u32, rd: Reg, rn: Reg) {
        self.sbfm(rd, rn, 0, bits - 1);
    }

    // ------------------------------------------------------------- conditional select
    pub fn csel(&mut self, rd: Reg, rn: Reg, rm: Reg, c: Cond) {
        self.word(0x9A80_0000 | (rm as u32) << 16 | (c as u32) << 12 | (rn as u32) << 5 | rd as u32);
    }
    /// `cset xd, cond`
    pub fn cset(&mut self, rd: Reg, c: Cond) {
        // csinc xd, xzr, xzr, !cond
        self.word(0x9A9F_07E0 | (c.invert() as u32) << 12 | rd as u32);
    }
    /// `cneg xd, xn, cond`
    pub fn cneg(&mut self, rd: Reg, rn: Reg, c: Cond) {
        // csneg xd, xn, xn, !cond
        self.word(0xDA80_0400 | (rn as u32) << 16 | (c.invert() as u32) << 12 | (rn as u32) << 5 | rd as u32);
    }

    // ------------------------------------------------------------- memory
    fn mem_imm(&mut self, scaled: u32, unscaled: u32, w: Width, rt: Reg, rn: Reg, off: i32) -> bool {
        let sh = w.log2() as i32;
        if off >= 0 && off % (1 << sh) == 0 && (off >> sh) < 4096 {
            self.word(scaled | ((off >> sh) as u32) << 10 | (rn as u32) << 5 | rt as u32);
            true
        } else if (-256..256).contains(&off) {
            self.word(unscaled | ((off as u32) & 0x1FF) << 12 | (rn as u32) << 5 | rt as u32);
            true
        } else {
            false
        }
    }

    /// Zero-extending load of the given width from `[rn, #off]`. Returns false if `off` is out of range.
    pub fn ldr(&mut self, w: Width, rt: Reg, rn: Reg, off: i32) -> bool {
        let (s, u) = match w {
            Width::B => (0x3940_0000, 0x3840_0000),
            Width::H => (0x7940_0000, 0x7840_0000),
            Width::W => (0xB940_0000, 0xB840_0000),
            Width::X => (0xF940_0000, 0xF840_0000),
        };
        self.mem_imm(s, u, w, rt, rn, off)
    }
    pub fn str(&mut self, w: Width, rt: Reg, rn: Reg, off: i32) -> bool {
        let (s, u) = match w {
            Width::B => (0x3900_0000, 0x3800_0000),
            Width::H => (0x7900_0000, 0x7800_0000),
            Width::W => (0xB900_0000, 0xB800_0000),
            Width::X => (0xF900_0000, 0xF800_0000),
        };
        self.mem_imm(s, u, w, rt, rn, off)
    }
    pub fn ldr_d(&mut self, dt: Reg, rn: Reg, off: i32) -> bool {
        self.mem_imm(0xFD40_0000, 0xFC40_0000, Width::X, dt, rn, off)
    }
    pub fn str_d(&mut self, dt: Reg, rn: Reg, off: i32) -> bool {
        self.mem_imm(0xFD00_0000, 0xFC00_0000, Width::X, dt, rn, off)
    }

    /// `ldr rt, [rn, rm, lsl #log2(width)]` (scaled register offset) when `scaled`, else `[rn, rm]`.
    pub fn ldr_reg(&mut self, w: Width, rt: Reg, rn: Reg, rm: Reg, scaled: bool) {
        let base = match w {
            Width::B => 0x3860_7800,
            Width::H => 0x7860_7800,
            Width::W => 0xB860_7800,
            Width::X => 0xF860_7800,
        };
        self.word(Self::reg_offset(base, scaled, rt, rn, rm));
    }
    pub fn str_reg(&mut self, w: Width, rt: Reg, rn: Reg, rm: Reg, scaled: bool) {
        let base = match w {
            Width::B => 0x3820_7800,
            Width::H => 0x7820_7800,
            Width::W => 0xB820_7800,
            Width::X => 0xF820_7800,
        };
        self.word(Self::reg_offset(base, scaled, rt, rn, rm));
    }
    pub fn ldr_d_reg(&mut self, dt: Reg, rn: Reg, rm: Reg, scaled: bool) {
        self.word(Self::reg_offset(0xFC60_7800, scaled, dt, rn, rm));
    }
    pub fn str_d_reg(&mut self, dt: Reg, rn: Reg, rm: Reg, scaled: bool) {
        self.word(Self::reg_offset(0xFC20_7800, scaled, dt, rn, rm));
    }
    fn reg_offset(base: u32, scaled: bool, rt: Reg, rn: Reg, rm: Reg) -> u32 {
        // option = LSL (0b011) at bits 15:13; S (bit 12) selects the scaled form
        let b = if scaled { base } else { base & !0x1000 };
        b | (rm as u32) << 16 | (rn as u32) << 5 | rt as u32
    }

    /// `stp xt1, xt2, [rn, #off]` / `ldp` (signed offset, multiple of 8 in -512..=504)
    pub fn stp(&mut self, rt1: Reg, rt2: Reg, rn: Reg, off: i32) {
        debug_assert!(off % 8 == 0 && (-512..=504).contains(&off));
        self.word(0xA900_0000 | (((off / 8) as u32) & 0x7F) << 15 | (rt2 as u32) << 10 | (rn as u32) << 5 | rt1 as u32);
    }
    pub fn ldp(&mut self, rt1: Reg, rt2: Reg, rn: Reg, off: i32) {
        debug_assert!(off % 8 == 0 && (-512..=504).contains(&off));
        self.word(0xA940_0000 | (((off / 8) as u32) & 0x7F) << 15 | (rt2 as u32) << 10 | (rn as u32) << 5 | rt1 as u32);
    }

    // ------------------------------------------------------------- branches
    pub fn b(&mut self, l: Label) {
        self.fixups.push((self.buf.len(), l, FixKind::B26));
        self.word(0x1400_0000);
    }
    pub fn b_cond(&mut self, c: Cond, l: Label) {
        self.fixups.push((self.buf.len(), l, FixKind::Imm19));
        self.word(0x5400_0000 | c as u32);
    }
    pub fn cbz(&mut self, rt: Reg, l: Label) {
        self.fixups.push((self.buf.len(), l, FixKind::Imm19));
        self.word(0xB400_0000 | rt as u32);
    }
    pub fn cbnz(&mut self, rt: Reg, l: Label) {
        self.fixups.push((self.buf.len(), l, FixKind::Imm19));
        self.word(0xB500_0000 | rt as u32);
    }
    /// `bl` with a placeholder displacement; returns the instruction offset for external patching.
    pub fn bl_patch(&mut self) -> usize {
        let at = self.buf.len();
        self.word(0x9400_0000);
        at
    }
    pub fn ret(&mut self) {
        self.word(0xD65F_03C0);
    }
    /// `adrp xd, #0` placeholder; returns its offset for patching.
    pub fn adrp_patch(&mut self, rd: Reg) -> usize {
        let at = self.buf.len();
        self.word(0x9000_0000 | rd as u32);
        at
    }
    pub fn svc(&mut self, imm: u16) {
        self.word(0xD400_0001 | (imm as u32) << 5);
    }
    pub fn brk(&mut self, imm: u16) {
        self.word(0xD420_0000 | (imm as u32) << 5);
    }

    /// `add xd, sp, xm` (extended-register form; the shifted-register form cannot name sp)
    pub fn add_sp_reg(&mut self, rd: Reg, rm: Reg) {
        self.word(0x8B20_63E0 | (rm as u32) << 16 | rd as u32);
    }

    // ------------------------------------------------------------- floating point (double)
    fn fff(&mut self, base: u32, rd: Reg, rn: Reg, rm: Reg) {
        self.word(base | (rm as u32) << 16 | (rn as u32) << 5 | rd as u32);
    }
    pub fn fadd(&mut self, d: Reg, n: Reg, m: Reg) {
        self.fff(0x1E60_2800, d, n, m);
    }
    pub fn fsub(&mut self, d: Reg, n: Reg, m: Reg) {
        self.fff(0x1E60_3800, d, n, m);
    }
    pub fn fmul(&mut self, d: Reg, n: Reg, m: Reg) {
        self.fff(0x1E60_0800, d, n, m);
    }
    pub fn fdiv(&mut self, d: Reg, n: Reg, m: Reg) {
        self.fff(0x1E60_1800, d, n, m);
    }
    fn ff(&mut self, base: u32, d: Reg, n: Reg) {
        self.word(base | (n as u32) << 5 | d as u32);
    }
    pub fn fsqrt(&mut self, d: Reg, n: Reg) {
        self.ff(0x1E61_C000, d, n);
    }
    pub fn fneg(&mut self, d: Reg, n: Reg) {
        self.ff(0x1E61_4000, d, n);
    }
    pub fn fabs(&mut self, d: Reg, n: Reg) {
        self.ff(0x1E60_C000, d, n);
    }
    /// round toward minus infinity (floor)
    pub fn frintm(&mut self, d: Reg, n: Reg) {
        self.ff(0x1E65_4000, d, n);
    }
    /// round toward plus infinity (ceil)
    pub fn frintp(&mut self, d: Reg, n: Reg) {
        self.ff(0x1E64_C000, d, n);
    }
    pub fn fmov(&mut self, d: Reg, n: Reg) {
        if d != n {
            self.ff(0x1E60_4000, d, n);
        }
    }
    pub fn fcsel(&mut self, d: Reg, n: Reg, m: Reg, c: Cond) {
        self.word(0x1E60_0C00 | (m as u32) << 16 | (c as u32) << 12 | (n as u32) << 5 | d as u32);
    }
    pub fn fcmp(&mut self, n: Reg, m: Reg) {
        self.word(0x1E60_2000 | (m as u32) << 16 | (n as u32) << 5);
    }
    /// `fmov xd, dn` (move the bits of a double into an integer register)
    pub fn fmov_to_x(&mut self, xd: Reg, dn: Reg) {
        self.word(0x9E66_0000 | (dn as u32) << 5 | xd as u32);
    }
    /// `fmov dd, xn`
    pub fn fmov_from_x(&mut self, dd: Reg, xn: Reg) {
        self.word(0x9E67_0000 | (xn as u32) << 5 | dd as u32);
    }
    pub fn scvtf(&mut self, dd: Reg, xn: Reg) {
        self.word(0x9E62_0000 | (xn as u32) << 5 | dd as u32);
    }
    pub fn ucvtf(&mut self, dd: Reg, xn: Reg) {
        self.word(0x9E63_0000 | (xn as u32) << 5 | dd as u32);
    }
    /// saturating double -> signed 64-bit (NaN becomes 0)
    pub fn fcvtzs(&mut self, xd: Reg, dn: Reg) {
        self.word(0x9E78_0000 | (dn as u32) << 5 | xd as u32);
    }
    pub fn fcvtzu(&mut self, xd: Reg, dn: Reg) {
        self.word(0x9E79_0000 | (dn as u32) << 5 | xd as u32);
    }
}

/// Encode `value` as an AArch64 logical (bitmask) immediate: returns (N, immr, imms).
pub fn encode_bitmask(value: u64) -> Option<(u32, u32, u32)> {
    if value == 0 || value == u64::MAX {
        return None;
    }
    // Find the smallest repeating element size.
    let mut size = 64u32;
    let mut v = value;
    while size > 2 {
        let half = size / 2;
        let mask = (1u64 << half) - 1;
        if (v & mask) == ((v >> half) & mask) {
            size = half;
            v &= mask;
        } else {
            break;
        }
    }
    let mask = if size == 64 { u64::MAX } else { (1u64 << size) - 1 };
    v &= mask;
    // v must be a rotation of a run of ones (within `size` bits).
    let ones = v.count_ones();
    // Find the rotation that puts the ones at the bottom.
    let mut rot = 0u32;
    let mut r = v;
    loop {
        let run = if ones == 64 { u64::MAX } else { (1u64 << ones) - 1 };
        if r == run {
            break;
        }
        // rotate right by 1 within `size` bits
        r = ((r >> 1) | ((r & 1) << (size - 1))) & mask;
        rot += 1;
        if rot >= size {
            return None;
        }
    }
    // `r` is v rotated right by `rot`; immr is the right-rotation that recreates v from the run.
    let immr = (size - rot) % size;
    let n = (size == 64) as u32;
    // imms encodes the element size in its high bits and (ones - 1) in the low bits.
    let imms = ((!(size - 1) << 1) & 0x3F) | (ones - 1);
    Some((n, immr, imms & 0x3F))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    /// Assemble `lines` with llvm-mc and return the 32-bit words, or None if llvm-mc is missing.
    fn llvm_words(lines: &[String]) -> Option<Vec<u32>> {
        let mut child = Command::new("llvm-mc")
            .args(["-triple=aarch64", "-show-encoding"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;
        use std::io::Write;
        child.stdin.take().unwrap().write_all(lines.join("\n").as_bytes()).ok()?;
        let out = child.wait_with_output().ok()?;
        assert!(out.status.success(), "llvm-mc failed: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8_lossy(&out.stdout);
        let mut words = vec![];
        for l in text.lines() {
            if let Some(i) = l.find("encoding: [") {
                let bytes: Vec<u8> = l[i + 11..].trim_end_matches(']').split(',').map(|b| u8::from_str_radix(b.trim().trim_start_matches("0x"), 16).unwrap()).collect();
                words.push(u32::from_le_bytes(bytes[..4].try_into().unwrap()));
            }
        }
        Some(words)
    }

    #[test]
    fn encodings_match_llvm() {
        let mut cases: Vec<(String, u32)> = vec![];
        macro_rules! case {
            ($text:expr, $emit:expr) => {{
                let mut a = Asm::new();
                #[allow(clippy::redundant_closure_call)]
                ($emit)(&mut a);
                cases.push(($text.to_string(), a.read_word(0)));
            }};
        }
        case!("mov x3, x4", |a: &mut Asm| a.mov(3, 4));
        case!("mov w3, w4", |a: &mut Asm| a.mov32(3, 4));
        case!("mov x5, sp", |a: &mut Asm| a.mov_sp(5, SP));
        case!("movz x1, #4660", |a: &mut Asm| a.movz(1, 0x1234, 0));
        case!("movz x1, #4660, lsl #32", |a: &mut Asm| a.movz(1, 0x1234, 2));
        case!("movk x1, #43981, lsl #16", |a: &mut Asm| a.movk(1, 0xABCD, 1));
        case!("movn x1, #4660", |a: &mut Asm| a.movn(1, 0x1234, 0));
        case!("add x1, x2, #100", |a: &mut Asm| a.add_imm(1, 2, 100));
        case!("add x1, x2, #4096", |a: &mut Asm| a.add_imm(1, 2, 4096));
        case!("sub sp, sp, #32", |a: &mut Asm| a.sub_imm(SP, SP, 32));
        case!("cmp x7, #12", |a: &mut Asm| a.cmp_imm(7, 12));
        case!("cmn x7, #12", |a: &mut Asm| a.cmn_imm(7, 12));
        case!("cmp w7, #12", |a: &mut Asm| a.cmp32_imm(7, 12));
        case!("add x1, x2, x3", |a: &mut Asm| a.add(1, 2, 3));
        case!("sub x1, x2, x3", |a: &mut Asm| a.sub(1, 2, 3));
        case!("add x1, x2, x3, lsl #3", |a: &mut Asm| a.add_lsl(1, 2, 3, 3));
        case!("add w1, w2, w3", |a: &mut Asm| a.add32(1, 2, 3));
        case!("sub w1, w2, w3", |a: &mut Asm| a.sub32(1, 2, 3));
        case!("cmp x4, x5", |a: &mut Asm| a.cmp(4, 5));
        case!("cmp w4, w5", |a: &mut Asm| a.cmp32(4, 5));
        case!("neg x1, x2", |a: &mut Asm| a.neg(1, 2));
        case!("neg w1, w2", |a: &mut Asm| a.neg32(1, 2));
        case!("negs x1, x2", |a: &mut Asm| a.negs(1, 2));
        case!("and x1, x2, x3", |a: &mut Asm| a.and(1, 2, 3));
        case!("orr x1, x2, x3", |a: &mut Asm| a.orr(1, 2, 3));
        case!("eor x1, x2, x3", |a: &mut Asm| a.eor(1, 2, 3));
        case!("tst x2, x3", |a: &mut Asm| a.tst(2, 3));
        case!("mvn x1, x2", |a: &mut Asm| a.mvn(1, 2));
        case!("mvn w1, w2", |a: &mut Asm| a.mvn32(1, 2));
        case!("mul x1, x2, x3", |a: &mut Asm| a.mul(1, 2, 3));
        case!("mul w1, w2, w3", |a: &mut Asm| a.mul32(1, 2, 3));
        case!("msub x1, x2, x3, x4", |a: &mut Asm| a.msub(1, 2, 3, 4));
        case!("smulh x1, x2, x3", |a: &mut Asm| a.smulh(1, 2, 3));
        case!("umulh x1, x2, x3", |a: &mut Asm| a.umulh(1, 2, 3));
        case!("sdiv x1, x2, x3", |a: &mut Asm| a.sdiv(1, 2, 3));
        case!("udiv x1, x2, x3", |a: &mut Asm| a.udiv(1, 2, 3));
        case!("lsl x1, x2, x3", |a: &mut Asm| a.lslv(1, 2, 3));
        case!("lsr x1, x2, x3", |a: &mut Asm| a.lsrv(1, 2, 3));
        case!("asr x1, x2, x3", |a: &mut Asm| a.asrv(1, 2, 3));
        case!("lsl w1, w2, w3", |a: &mut Asm| a.lslv32(1, 2, 3));
        case!("lsr w1, w2, w3", |a: &mut Asm| a.lsrv32(1, 2, 3));
        case!("lsl x1, x2, #5", |a: &mut Asm| a.lsl_imm(1, 2, 5));
        case!("lsr x1, x2, #5", |a: &mut Asm| a.lsr_imm(1, 2, 5));
        case!("asr x1, x2, #63", |a: &mut Asm| a.asr_imm(1, 2, 63));
        case!("lsl w1, w2, #5", |a: &mut Asm| a.lsl32_imm(1, 2, 5));
        case!("lsr w1, w2, #5", |a: &mut Asm| a.lsr32_imm(1, 2, 5));
        case!("ubfm x1, x2, #3, #9", |a: &mut Asm| a.ubfm(1, 2, 3, 9));
        case!("sbfm x1, x2, #3, #9", |a: &mut Asm| a.sbfm(1, 2, 3, 9));
        case!("uxtb w1, w2", |a: &mut Asm| a.uxt(8, 1, 2));
        case!("uxth w1, w2", |a: &mut Asm| a.uxt(16, 1, 2));
        case!("sxtb x1, w2", |a: &mut Asm| a.sxt(8, 1, 2));
        case!("sxth x1, w2", |a: &mut Asm| a.sxt(16, 1, 2));
        case!("sxtw x1, w2", |a: &mut Asm| a.sxt(32, 1, 2));
        case!("csel x1, x2, x3, lt", |a: &mut Asm| a.csel(1, 2, 3, Cond::Lt));
        case!("cset x1, ne", |a: &mut Asm| a.cset(1, Cond::Ne));
        case!("cset x1, hi", |a: &mut Asm| a.cset(1, Cond::Hi));
        case!("cneg x1, x2, mi", |a: &mut Asm| a.cneg(1, 2, Cond::Mi));
        case!("ldr x1, [x2, #16]", |a: &mut Asm| assert!(a.ldr(Width::X, 1, 2, 16)));
        case!("ldr w1, [x2, #16]", |a: &mut Asm| assert!(a.ldr(Width::W, 1, 2, 16)));
        case!("ldrh w1, [x2, #6]", |a: &mut Asm| assert!(a.ldr(Width::H, 1, 2, 6)));
        case!("ldrb w1, [x2, #3]", |a: &mut Asm| assert!(a.ldr(Width::B, 1, 2, 3)));
        case!("str x1, [sp, #24]", |a: &mut Asm| assert!(a.str(Width::X, 1, SP, 24)));
        case!("str w1, [x2, #8]", |a: &mut Asm| assert!(a.str(Width::W, 1, 2, 8)));
        case!("strh w1, [x2, #2]", |a: &mut Asm| assert!(a.str(Width::H, 1, 2, 2)));
        case!("strb w1, [x2, #1]", |a: &mut Asm| assert!(a.str(Width::B, 1, 2, 1)));
        case!("ldur x1, [x2, #-8]", |a: &mut Asm| assert!(a.ldr(Width::X, 1, 2, -8)));
        case!("ldur x1, [x2, #3]", |a: &mut Asm| assert!(a.ldr(Width::X, 1, 2, 3)));
        case!("stur w1, [x2, #-4]", |a: &mut Asm| assert!(a.str(Width::W, 1, 2, -4)));
        case!("ldr d1, [x2, #16]", |a: &mut Asm| assert!(a.ldr_d(1, 2, 16)));
        case!("str d1, [x2, #-16]", |a: &mut Asm| assert!(a.str_d(1, 2, -16)));
        case!("ldr x1, [x2, x3, lsl #3]", |a: &mut Asm| a.ldr_reg(Width::X, 1, 2, 3, true));
        case!("ldr x1, [x2, x3]", |a: &mut Asm| a.ldr_reg(Width::X, 1, 2, 3, false));
        case!("ldr w1, [x2, x3, lsl #2]", |a: &mut Asm| a.ldr_reg(Width::W, 1, 2, 3, true));
        case!("ldrh w1, [x2, x3, lsl #1]", |a: &mut Asm| a.ldr_reg(Width::H, 1, 2, 3, true));
        case!("ldrb w1, [x2, x3]", |a: &mut Asm| a.ldr_reg(Width::B, 1, 2, 3, false));
        case!("str x1, [x2, x3, lsl #3]", |a: &mut Asm| a.str_reg(Width::X, 1, 2, 3, true));
        case!("strb w1, [x2, x3]", |a: &mut Asm| a.str_reg(Width::B, 1, 2, 3, false));
        case!("ldr d1, [x2, x3, lsl #3]", |a: &mut Asm| a.ldr_d_reg(1, 2, 3, true));
        case!("str d1, [x2, x3]", |a: &mut Asm| a.str_d_reg(1, 2, 3, false));
        case!("stp x19, x20, [sp, #16]", |a: &mut Asm| a.stp(19, 20, SP, 16));
        case!("ldp x19, x20, [sp, #-16]", |a: &mut Asm| a.ldp(19, 20, SP, -16));
        case!("ret", |a: &mut Asm| a.ret());
        case!("svc #0", |a: &mut Asm| a.svc(0));
        case!("svc #128", |a: &mut Asm| a.svc(0x80));
        case!("brk #1", |a: &mut Asm| a.brk(1));
        case!("fadd d1, d2, d3", |a: &mut Asm| a.fadd(1, 2, 3));
        case!("fsub d1, d2, d3", |a: &mut Asm| a.fsub(1, 2, 3));
        case!("fmul d1, d2, d3", |a: &mut Asm| a.fmul(1, 2, 3));
        case!("fdiv d1, d2, d3", |a: &mut Asm| a.fdiv(1, 2, 3));
        case!("fsqrt d1, d2", |a: &mut Asm| a.fsqrt(1, 2));
        case!("fneg d1, d2", |a: &mut Asm| a.fneg(1, 2));
        case!("fabs d1, d2", |a: &mut Asm| a.fabs(1, 2));
        case!("frintm d1, d2", |a: &mut Asm| a.frintm(1, 2));
        case!("frintp d1, d2", |a: &mut Asm| a.frintp(1, 2));
        case!("fmov d1, d2", |a: &mut Asm| a.fmov(1, 2));
        case!("fcmp d1, d2", |a: &mut Asm| a.fcmp(1, 2));
        case!("fcsel d1, d2, d3, ge", |a: &mut Asm| a.fcsel(1, 2, 3, Cond::Ge));
        case!("add x17, sp, x16", |a: &mut Asm| a.add_sp_reg(17, 16));
        case!("fmov x1, d2", |a: &mut Asm| a.fmov_to_x(1, 2));
        case!("fmov d1, x2", |a: &mut Asm| a.fmov_from_x(1, 2));
        case!("scvtf d1, x2", |a: &mut Asm| a.scvtf(1, 2));
        case!("ucvtf d1, x2", |a: &mut Asm| a.ucvtf(1, 2));
        case!("fcvtzs x1, d2", |a: &mut Asm| a.fcvtzs(1, 2));
        case!("fcvtzu x1, d2", |a: &mut Asm| a.fcvtzu(1, 2));
        // bitmask immediates
        for v in [0xFFu64, 0xFFFF, 0xFFFF_FFFF, 0x7FFF_FFFF_FFFF_FFFF, 0x5555_5555_5555_5555, 0xFF00, 0x00FF_00FF_00FF_00FF, 1, 3, 0x8000_0000_0000_0000, 0xF0F0_F0F0_F0F0_F0F0, 0xFFFF_FFFF_0000_0000, 0x0000_FFFF_FFFF_0000] {
            case!(format!("and x1, x2, #{}", v), |a: &mut Asm| assert!(a.and_imm(1, 2, v)));
        }
        let Some(want) = llvm_words(&cases.iter().map(|c| c.0.clone()).collect::<Vec<_>>()) else {
            eprintln!("note: llvm-mc not installed; skipping the AArch64 encoder check");
            return;
        };
        assert_eq!(want.len(), cases.len());
        for ((text, got), want) in cases.iter().zip(want) {
            assert_eq!(*got, want, "`{}`: encoded {:#010x}, llvm-mc says {:#010x}", text, got, want);
        }
    }

    #[test]
    fn bitmask_immediates_round_trip() {
        // Non-encodable values are rejected, encodable ones decode back to the same value.
        fn decode(n: u32, immr: u32, imms: u32) -> u64 {
            let len = 31 - (((n << 6) | (!imms & 0x3F)).leading_zeros());
            let size = 1u32 << len;
            let levels = size - 1;
            let s = imms & levels;
            let r = immr & levels;
            let welem: u64 = (1u64 << (s + 1)) - 1;
            let rotated = if r == 0 { welem } else { ((welem >> r) | (welem << (size - r))) & (if size == 64 { u64::MAX } else { (1u64 << size) - 1 }) };
            let mut v = 0u64;
            let mut i = 0;
            while i < 64 {
                v |= rotated << i;
                i += size;
            }
            v
        }
        for &v in &[0xFFu64, 0xFF00, 0x00FF_00FF_00FF_00FF, 0x5555_5555_5555_5555, 1, 0x8000_0000_0000_0000, 0xFFFF_FFFF_0000_0000, 0x7FFF_FFFF_FFFF_FFFF, 0x0000_0000_FFFF_FFFE] {
            let (n, r, s) = encode_bitmask(v).unwrap_or_else(|| panic!("{:#x} should be encodable", v));
            assert_eq!(decode(n, r, s), v, "{:#x}", v);
        }
        for &v in &[0u64, u64::MAX, 0x1234, 0xFF0F, 0x0000_0001_0000_0003] {
            assert!(encode_bitmask(v).is_none(), "{:#x} should not be encodable", v);
        }
    }
}
