//! x86-64 machine-code encoder: just the instructions the Flash code generator needs.
//!
//! Registers are numbered as in the ISA (rax = 0 … r15 = 15; xmm0 … xmm15 share the numbering
//! space of their own class). Every emitter appends bytes to `Asm::buf`; jumps use labels that
//! are patched by `bind`/`finish`.

pub type Reg = u8;

pub const RAX: Reg = 0;
pub const RCX: Reg = 1;
pub const RDX: Reg = 2;
pub const RBX: Reg = 3;
pub const RSP: Reg = 4;
pub const RBP: Reg = 5;
pub const RSI: Reg = 6;
pub const RDI: Reg = 7;
pub const R8: Reg = 8;
pub const R9: Reg = 9;
pub const R10: Reg = 10;
pub const R12: Reg = 12;
pub const R13: Reg = 13;
pub const R14: Reg = 14;
pub const R15: Reg = 15;

/// Operand size of an integer instruction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sz {
    B,
    W,
    D,
    Q,
}

/// A memory operand `[base + disp]`; `base == None` means the absolute 32-bit address `disp`.
#[derive(Clone, Copy, Debug)]
pub struct Mem {
    pub base: Option<Reg>,
    pub disp: i32,
}

impl Mem {
    pub fn at(base: Reg, disp: i32) -> Mem {
        Mem { base: Some(base), disp }
    }
    pub fn abs(addr: u32) -> Mem {
        Mem { base: None, disp: addr as i32 }
    }
}

/// Register-or-memory operand.
#[derive(Clone, Copy, Debug)]
pub enum Rm {
    R(Reg),
    M(Mem),
}

/// ALU operations sharing the classic `op r/m, r` encoding; the value is the `/digit` of the
/// immediate form.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Alu {
    Add = 0,
    Or = 1,
    And = 4,
    Sub = 5,
    Xor = 6,
    Cmp = 7,
}

/// Condition codes (the low nibble of `jcc`/`setcc`/`cmovcc`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cc {
    O = 0,
    No = 1,
    B = 2,
    Ae = 3,
    E = 4,
    Ne = 5,
    Be = 6,
    A = 7,
    S = 8,
    Ns = 9,
    P = 10,
    Np = 11,
    L = 12,
    Ge = 13,
    Le = 14,
    G = 15,
}

impl Cc {
    pub fn negate(self) -> Cc {
        use Cc::*;
        match self {
            O => No,
            No => O,
            B => Ae,
            Ae => B,
            E => Ne,
            Ne => E,
            Be => A,
            A => Be,
            S => Ns,
            Ns => S,
            P => Np,
            Np => P,
            L => Ge,
            Ge => L,
            Le => G,
            G => Le,
        }
    }
}

pub type Label = usize;

#[derive(Default)]
pub struct Asm {
    pub buf: Vec<u8>,
    /// Label → bound offset.
    labels: Vec<Option<usize>>,
    /// (offset of the rel32 field, label)
    fixups: Vec<(usize, Label)>,
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
        debug_assert!(self.labels[l].is_none(), "label bound twice");
        self.labels[l] = Some(self.buf.len());
    }

    /// Patch all label references. Every referenced label must be bound.
    pub fn resolve_labels(&mut self) -> Result<(), String> {
        for &(at, l) in &self.fixups {
            let target = self.labels[l].ok_or("internal error: jump to an unbound label")?;
            let rel = target as i64 - (at as i64 + 4);
            self.buf[at..at + 4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
        self.fixups.clear();
        Ok(())
    }

    fn b(&mut self, x: u8) {
        self.buf.push(x);
    }
    fn d(&mut self, x: i32) {
        self.buf.extend_from_slice(&x.to_le_bytes());
    }

    /// Emit `[prefix] [REX] opcode… modrm [sib] [disp]` for `reg` and `rm`.
    /// `byte_regs`: an 8-bit register operand is involved (forces REX so that sil/dil/… are used).
    #[allow(clippy::too_many_arguments)]
    fn op(&mut self, prefix: u8, w: bool, opcode: &[u8], reg: u8, rm: Rm, byte_regs: bool) {
        if prefix != 0 {
            self.b(prefix);
        }
        let mut rex = 0x40u8;
        if w {
            rex |= 8;
        }
        if reg >= 8 {
            rex |= 4;
        }
        let rm_needs_ext = match rm {
            Rm::R(r) => r >= 8,
            Rm::M(m) => m.base.is_some_and(|b| b >= 8),
        };
        if rm_needs_ext {
            rex |= 1;
        }
        // A REX prefix is always harmless and is required to address sil/dil/spl/bpl.
        if rex != 0x40 || byte_regs {
            self.b(rex);
        }
        self.buf.extend_from_slice(opcode);
        let reg3 = reg & 7;
        match rm {
            Rm::R(r) => self.b(0xC0 | (reg3 << 3) | (r & 7)),
            Rm::M(m) => match m.base {
                None => {
                    // mod=00 rm=100 + SIB(base=101, index=100): absolute disp32.
                    self.b((reg3 << 3) | 4);
                    self.b(0x25);
                    self.d(m.disp);
                }
                Some(base) => {
                    let b3 = base & 7;
                    let (mode, disp8) = if m.disp == 0 && b3 != 5 {
                        (0u8, false)
                    } else if (-128..=127).contains(&m.disp) {
                        (1u8, true)
                    } else {
                        (2u8, false)
                    };
                    self.b((mode << 6) | (reg3 << 3) | if b3 == 4 { 4 } else { b3 });
                    if b3 == 4 {
                        self.b(0x24); // SIB: base=rsp/r12, no index
                    }
                    if mode == 1 {
                        debug_assert!(disp8);
                        self.b(m.disp as i8 as u8);
                    } else if mode == 2 {
                        self.d(m.disp);
                    }
                }
            },
        }
    }

    fn size_parts(sz: Sz) -> (u8, bool) {
        match sz {
            Sz::W => (0x66, false),
            Sz::Q => (0, true),
            _ => (0, false),
        }
    }

    // ------------------------------------------------------------------ moves
    /// `mov dst, src` (register to register); 32-bit form is used for `Sz::D` (zero-extends).
    pub fn mov_rr(&mut self, sz: Sz, dst: Reg, src: Reg) {
        if dst == src && sz == Sz::Q {
            return;
        }
        let (p, w) = Self::size_parts(sz);
        self.op(p, w, &[0x8B], dst, Rm::R(src), false);
    }

    /// Load a 64-bit constant using the shortest encoding.
    pub fn mov_ri(&mut self, dst: Reg, val: i64) {
        if val == 0 {
            // xor r32, r32 (clobbers flags; callers that need flags use mov_ri_keep_flags)
            self.op(0, false, &[0x33], dst, Rm::R(dst), false);
        } else {
            self.mov_ri_keep_flags(dst, val);
        }
    }

    pub fn mov_ri_keep_flags(&mut self, dst: Reg, val: i64) {
        if (0..=u32::MAX as i64).contains(&val) {
            // mov r32, imm32 (zero-extends)
            if dst >= 8 {
                self.b(0x41);
            }
            self.b(0xB8 + (dst & 7));
            self.d(val as u32 as i32);
        } else if (i32::MIN as i64..0).contains(&val) {
            // mov r/m64, simm32
            self.op(0, true, &[0xC7], 0, Rm::R(dst), false);
            self.d(val as i32);
        } else {
            self.b(0x48 | if dst >= 8 { 1 } else { 0 });
            self.b(0xB8 + (dst & 7));
            self.buf.extend_from_slice(&val.to_le_bytes());
        }
    }

    /// `mov r32, imm32` with a placeholder; returns the offset of the immediate for patching.
    pub fn mov_r32_imm_patch(&mut self, dst: Reg) -> usize {
        if dst >= 8 {
            self.b(0x41);
        }
        self.b(0xB8 + (dst & 7));
        let at = self.pos();
        self.d(0);
        at
    }

    /// Zero-extending load of `sz` bytes.
    pub fn load(&mut self, sz: Sz, dst: Reg, m: Mem) {
        match sz {
            Sz::B => self.op(0, false, &[0x0F, 0xB6], dst, Rm::M(m), false),
            Sz::W => self.op(0, false, &[0x0F, 0xB7], dst, Rm::M(m), false),
            Sz::D => self.op(0, false, &[0x8B], dst, Rm::M(m), false),
            Sz::Q => self.op(0, true, &[0x8B], dst, Rm::M(m), false),
        }
    }

    pub fn store(&mut self, sz: Sz, m: Mem, src: Reg) {
        match sz {
            Sz::B => self.op(0, false, &[0x88], src, Rm::M(m), true),
            Sz::W => self.op(0x66, false, &[0x89], src, Rm::M(m), false),
            Sz::D => self.op(0, false, &[0x89], src, Rm::M(m), false),
            Sz::Q => self.op(0, true, &[0x89], src, Rm::M(m), false),
        }
    }

    /// Store a sign-extended 32-bit immediate (`sz` = Q) or an immediate of the given width.
    pub fn store_imm(&mut self, sz: Sz, m: Mem, val: i32) {
        match sz {
            Sz::B => {
                self.op(0, false, &[0xC6], 0, Rm::M(m), false);
                self.b(val as u8);
            }
            Sz::W => {
                self.op(0x66, false, &[0xC7], 0, Rm::M(m), false);
                self.buf.extend_from_slice(&(val as i16).to_le_bytes());
            }
            Sz::D => {
                self.op(0, false, &[0xC7], 0, Rm::M(m), false);
                self.d(val);
            }
            Sz::Q => {
                self.op(0, true, &[0xC7], 0, Rm::M(m), false);
                self.d(val);
            }
        }
    }

    pub fn lea(&mut self, dst: Reg, m: Mem) {
        self.op(0, true, &[0x8D], dst, Rm::M(m), false);
    }

    /// `lea dst, [a + b]`
    pub fn lea_rr(&mut self, dst: Reg, a: Reg, b: Reg) {
        // SIB with base=a, index=b, scale=1. `b` must not be rsp.
        let mut rex = 0x48u8;
        if dst >= 8 {
            rex |= 4;
        }
        if b >= 8 {
            rex |= 2;
        }
        if a >= 8 {
            rex |= 1;
        }
        self.b(rex);
        self.b(0x8D);
        let a3 = a & 7;
        if a3 == 5 {
            self.b(0x44 | ((dst & 7) << 3));
            self.b(((b & 7) << 3) | a3);
            self.b(0);
        } else {
            self.b(0x04 | ((dst & 7) << 3));
            self.b(((b & 7) << 3) | a3);
        }
    }

    /// `movzx dst32, src(sz)` (zero-extend a narrow register).
    pub fn movzx(&mut self, from: Sz, dst: Reg, src: Reg) {
        match from {
            Sz::B => self.op(0, false, &[0x0F, 0xB6], dst, Rm::R(src), true),
            Sz::W => self.op(0, false, &[0x0F, 0xB7], dst, Rm::R(src), false),
            Sz::D => self.op(0, false, &[0x8B], dst, Rm::R(src), false),
            Sz::Q => self.mov_rr(Sz::Q, dst, src),
        }
    }

    /// Sign-extend a narrow register to 64 bits.
    pub fn movsx(&mut self, from: Sz, dst: Reg, src: Reg) {
        match from {
            Sz::B => self.op(0, true, &[0x0F, 0xBE], dst, Rm::R(src), true),
            Sz::W => self.op(0, true, &[0x0F, 0xBF], dst, Rm::R(src), false),
            Sz::D => self.op(0, true, &[0x63], dst, Rm::R(src), false),
            Sz::Q => self.mov_rr(Sz::Q, dst, src),
        }
    }

    // -------------------------------------------------------------------- ALU
    pub fn alu_rr(&mut self, op: Alu, sz: Sz, dst: Reg, src: Reg) {
        let (p, w) = Self::size_parts(sz);
        let opc = ((op as u8) << 3) | if sz == Sz::B { 0x02 } else { 0x03 };
        self.op(p, w, &[opc], dst, Rm::R(src), sz == Sz::B);
    }

    pub fn alu_rm(&mut self, op: Alu, sz: Sz, dst: Reg, m: Mem) {
        let (p, w) = Self::size_parts(sz);
        let opc = ((op as u8) << 3) | if sz == Sz::B { 0x02 } else { 0x03 };
        self.op(p, w, &[opc], dst, Rm::M(m), sz == Sz::B);
    }

    pub fn alu_ri(&mut self, op: Alu, sz: Sz, dst: Reg, imm: i32) {
        self.alu_rmi(op, sz, Rm::R(dst), imm);
    }

    pub fn alu_mi(&mut self, op: Alu, sz: Sz, m: Mem, imm: i32) {
        self.alu_rmi(op, sz, Rm::M(m), imm);
    }

    fn alu_rmi(&mut self, op: Alu, sz: Sz, rm: Rm, imm: i32) {
        let (p, w) = Self::size_parts(sz);
        if sz == Sz::B {
            self.op(p, w, &[0x80], op as u8, rm, true);
            self.b(imm as u8);
        } else if (-128..=127).contains(&imm) {
            self.op(p, w, &[0x83], op as u8, rm, false);
            self.b(imm as i8 as u8);
        } else {
            self.op(p, w, &[0x81], op as u8, rm, false);
            if sz == Sz::W {
                self.buf.extend_from_slice(&(imm as i16).to_le_bytes());
            } else {
                self.d(imm);
            }
        }
    }

    pub fn test_rr(&mut self, sz: Sz, a: Reg, b: Reg) {
        let (p, w) = Self::size_parts(sz);
        self.op(p, w, &[if sz == Sz::B { 0x84 } else { 0x85 }], b, Rm::R(a), sz == Sz::B);
    }

    pub fn imul_rr(&mut self, sz: Sz, dst: Reg, src: Reg) {
        let (p, w) = Self::size_parts(if sz == Sz::Q { Sz::Q } else { Sz::D });
        self.op(p, w, &[0x0F, 0xAF], dst, Rm::R(src), false);
    }

    pub fn imul_rm(&mut self, sz: Sz, dst: Reg, m: Mem) {
        let (p, w) = Self::size_parts(if sz == Sz::Q { Sz::Q } else { Sz::D });
        self.op(p, w, &[0x0F, 0xAF], dst, Rm::M(m), false);
    }

    pub fn imul_rri(&mut self, sz: Sz, dst: Reg, src: Reg, imm: i32) {
        let (p, w) = Self::size_parts(if sz == Sz::Q { Sz::Q } else { Sz::D });
        if (-128..=127).contains(&imm) {
            self.op(p, w, &[0x6B], dst, Rm::R(src), false);
            self.b(imm as i8 as u8);
        } else {
            self.op(p, w, &[0x69], dst, Rm::R(src), false);
            self.d(imm);
        }
    }

    pub fn neg(&mut self, sz: Sz, r: Reg) {
        let (p, w) = Self::size_parts(sz);
        self.op(p, w, &[if sz == Sz::B { 0xF6 } else { 0xF7 }], 3, Rm::R(r), sz == Sz::B);
    }

    pub fn not(&mut self, sz: Sz, r: Reg) {
        let (p, w) = Self::size_parts(sz);
        self.op(p, w, &[if sz == Sz::B { 0xF6 } else { 0xF7 }], 2, Rm::R(r), sz == Sz::B);
    }

    /// Shift by `cl` (`ext`: 4 = shl, 5 = shr, 7 = sar).
    pub fn shift_cl(&mut self, ext: u8, sz: Sz, r: Reg) {
        let (p, w) = Self::size_parts(sz);
        self.op(p, w, &[0xD3], ext, Rm::R(r), false);
    }

    pub fn shift_i(&mut self, ext: u8, sz: Sz, r: Reg, n: u8) {
        let (p, w) = Self::size_parts(sz);
        self.op(p, w, &[0xC1], ext, Rm::R(r), false);
        self.b(n);
    }

    /// Sign-extend rax into rdx:rax (64-bit `cqo`) or eax into edx:eax (`cdq`).
    pub fn cqo(&mut self, sz: Sz) {
        if sz == Sz::Q {
            self.b(0x48);
        }
        self.b(0x99);
    }

    /// Signed (`signed`) or unsigned divide of rdx:rax by `r`.
    pub fn div(&mut self, sz: Sz, r: Reg, signed: bool) {
        let (p, w) = Self::size_parts(sz);
        self.op(p, w, &[0xF7], if signed { 7 } else { 6 }, Rm::R(r), false);
    }

    /// One-operand multiply: rdx:rax = rax * r (signed `imul` or unsigned `mul`).
    pub fn mul_wide(&mut self, r: Reg, signed: bool) {
        self.op(0, true, &[0xF7], if signed { 5 } else { 4 }, Rm::R(r), false);
    }

    pub fn setcc(&mut self, cc: Cc, r: Reg) {
        self.op(0, false, &[0x0F, 0x90 | cc as u8], 0, Rm::R(r), true);
    }

    pub fn cmov(&mut self, cc: Cc, dst: Reg, src: Reg) {
        self.op(0, true, &[0x0F, 0x40 | cc as u8], dst, Rm::R(src), false);
    }

    pub fn cmov_m(&mut self, cc: Cc, dst: Reg, m: Mem) {
        self.op(0, true, &[0x0F, 0x40 | cc as u8], dst, Rm::M(m), false);
    }

    // ------------------------------------------------------------ control flow
    pub fn jmp(&mut self, l: Label) {
        self.b(0xE9);
        self.fixups.push((self.pos(), l));
        self.d(0);
    }

    pub fn jcc(&mut self, cc: Cc, l: Label) {
        self.b(0x0F);
        self.b(0x80 | cc as u8);
        self.fixups.push((self.pos(), l));
        self.d(0);
    }

    /// `call rel32`; returns the offset of the displacement for external patching.
    pub fn call_patch(&mut self) -> usize {
        self.b(0xE8);
        let at = self.pos();
        self.d(0);
        at
    }

    pub fn ret(&mut self) {
        self.b(0xC3);
    }

    pub fn push(&mut self, r: Reg) {
        if r >= 8 {
            self.b(0x41);
        }
        self.b(0x50 + (r & 7));
    }

    pub fn pop(&mut self, r: Reg) {
        if r >= 8 {
            self.b(0x41);
        }
        self.b(0x58 + (r & 7));
    }

    pub fn syscall(&mut self) {
        self.b(0x0F);
        self.b(0x05);
    }

    pub fn ud2(&mut self) {
        self.b(0x0F);
        self.b(0x0B);
    }

    // -------------------------------------------------------------------- SSE2
    fn sse(&mut self, prefix: u8, w: bool, opcode: u8, reg: u8, rm: Rm) {
        self.op(prefix, w, &[0x0F, opcode], reg, rm, false);
    }

    pub fn movsd_rr(&mut self, dst: Reg, src: Reg) {
        if dst != src {
            self.sse(0xF2, false, 0x10, dst, Rm::R(src));
        }
    }
    pub fn movsd_load(&mut self, dst: Reg, m: Mem) {
        self.sse(0xF2, false, 0x10, dst, Rm::M(m));
    }
    pub fn movsd_store(&mut self, m: Mem, src: Reg) {
        self.sse(0xF2, false, 0x11, src, Rm::M(m));
    }
    /// addsd/mulsd/subsd/divsd/sqrtsd by opcode (0x58, 0x59, 0x5C, 0x5E, 0x51).
    pub fn sd_op(&mut self, opcode: u8, dst: Reg, src: Reg) {
        self.sse(0xF2, false, opcode, dst, Rm::R(src));
    }
    pub fn sd_op_m(&mut self, opcode: u8, dst: Reg, m: Mem) {
        self.sse(0xF2, false, opcode, dst, Rm::M(m));
    }
    pub fn ucomisd(&mut self, a: Reg, b: Reg) {
        self.sse(0x66, false, 0x2E, a, Rm::R(b));
    }
    pub fn ucomisd_m(&mut self, a: Reg, m: Mem) {
        self.sse(0x66, false, 0x2E, a, Rm::M(m));
    }
    pub fn xorpd_m(&mut self, dst: Reg, m: Mem) {
        self.sse(0x66, false, 0x57, dst, Rm::M(m));
    }
    pub fn andpd_m(&mut self, dst: Reg, m: Mem) {
        self.sse(0x66, false, 0x54, dst, Rm::M(m));
    }
    pub fn orpd(&mut self, dst: Reg, src: Reg) {
        self.sse(0x66, false, 0x56, dst, Rm::R(src));
    }
    pub fn xorpd(&mut self, dst: Reg, src: Reg) {
        self.sse(0x66, false, 0x57, dst, Rm::R(src));
    }
    /// movq xmm, r64
    pub fn movq_xr(&mut self, dst: Reg, src: Reg) {
        self.sse(0x66, true, 0x6E, dst, Rm::R(src));
    }
    /// movq r64, xmm
    pub fn movq_rx(&mut self, dst: Reg, src: Reg) {
        self.sse(0x66, true, 0x7E, src, Rm::R(dst));
    }
    /// cvtsi2sd xmm, r64
    pub fn cvtsi2sd(&mut self, dst: Reg, src: Reg) {
        self.sse(0xF2, true, 0x2A, dst, Rm::R(src));
    }
    /// cvttsd2si r64, xmm
    pub fn cvttsd2si(&mut self, dst: Reg, src: Reg) {
        self.sse(0xF2, true, 0x2C, dst, Rm::R(src));
    }

    pub fn align_to(&mut self, n: usize, fill: u8) {
        while self.buf.len() % n != 0 {
            self.b(fill);
        }
    }
}
