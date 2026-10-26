//! Native back end: IR → x86-64 machine code → static ELF executable, with no external
//! compiler, assembler, linker or C library involved.
//!
//! Programs call no libc: the runtime library (strings, lists, memory, I/O) is itself written in
//! Flash (see `runtime/*.flash`), compiled by the same pipeline, and talks to the Linux kernel
//! through raw system calls.

mod addrfold;
mod a64;
mod a64gen;
mod asm;
mod codegen;
mod codesign;
mod magic;
mod elf;
mod macho;
mod pe;
pub mod link;
mod regalloc;

use crate::ir::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux = 0,
    MacOs = 1,
    Windows = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86_64 = 0,
    Aarch64 = 1,
}

/// What the generated executable runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub os: Os,
    pub arch: Arch,
}

const SUPPORTED: &str = "x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu, x86_64-apple-darwin, aarch64-apple-darwin, x86_64-pc-windows-msvc";

impl Target {
    /// The machine the compiler runs on, if the native back end supports it.
    pub fn host() -> Option<Target> {
        let arch = if cfg!(target_arch = "x86_64") {
            Arch::X86_64
        } else if cfg!(target_arch = "aarch64") {
            Arch::Aarch64
        } else {
            return None;
        };
        let os = if cfg!(target_os = "linux") {
            Os::Linux
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(target_os = "windows") && arch == Arch::X86_64 {
            Os::Windows
        } else {
            return None;
        };
        Some(Target { os, arch })
    }

    pub fn parse(triple: &str) -> Result<Target, String> {
        let t = triple.to_ascii_lowercase();
        let bad = || format!("the native back end cannot generate code for '{}' (supported: {})", triple, SUPPORTED);
        let arch = if t.starts_with("x86_64") || t.starts_with("amd64") {
            Arch::X86_64
        } else if t.starts_with("aarch64") || t.starts_with("arm64") {
            Arch::Aarch64
        } else {
            return Err(bad());
        };
        let os = if t.contains("linux") {
            Os::Linux
        } else if t.contains("windows") && arch == Arch::X86_64 {
            Os::Windows
        } else if t.contains("darwin") || t.contains("apple") || t.contains("macos") {
            Os::MacOs
        } else {
            return Err(bad());
        };
        Ok(Target { os, arch })
    }

    /// (image base, page size) of the executables written for this target.
    fn image_params(self) -> (u64, u64) {
        match (self.os, self.arch) {
            (Os::Linux, Arch::X86_64) | (Os::MacOs, Arch::X86_64) | (Os::Windows, _) => (elf::BASE, elf::PAGE),
            (Os::Linux, Arch::Aarch64) => (0x40_0000, 0x4000),
            (Os::MacOs, Arch::Aarch64) => (0x1_0000_0000, 0x4000),
        }
    }
}

/// Pseudo system-call numbers (native back end only; Linux). They are handled by the code
/// generators, not the kernel, and use the register convention of `Callee::Syscall`.
pub mod pseudo {
    pub const BASE: i64 = 0x7fff_0000;
    /// (address, expected, new) -> previous value; atomic compare-and-swap
    pub const CAS: i64 = BASE + 1;
    /// (address, new) -> previous value; atomic exchange
    pub const XCHG: i64 = BASE + 2;
    /// (address, delta) -> previous value; atomic add
    pub const FADD: i64 = BASE + 3;
    /// Start a thread. x86-64: (flags, stack, ptid, ctid, tls, arg); AArch64: (flags, stack, ptid, tls, ctid, arg).
    /// The child calls `THREAD_ENTRY(arg)` on the new stack and then exits. Returns the thread id.
    pub const CLONE: i64 = BASE + 4;
    /// Set the thread pointer: `tpidr_el0` on AArch64, the TEB's user pointer (`gs:[0x28]`) on Windows.
    pub const SET_TP: i64 = BASE + 5;
    /// Windows: start a thread running `THREAD_ENTRY(arg)` with the given stack size; returns its handle.
    pub const SPAWN_WIN: i64 = BASE + 6;
    /// macOS (arm64): call libSystem import number `x0` (see `MAC_IMPORTS`) with arguments x1..x5; returns x0.
    pub const MAC_CALL: i64 = BASE + 7;
    /// macOS (arm64): the address of `THREAD_ENTRY`.
    pub const FUNC_ADDR: i64 = BASE + 8;
    /// libSystem functions macOS programs with threads import, in GOT order.
    pub const MAC_IMPORTS: [&str; 4] = ["_pthread_create", "_pthread_join", "_pthread_attr_init", "_pthread_attr_setstacksize"];
    /// The thread-specific-data slot (`tpidrro_el0` + 8 * slot) where macOS threads keep their runtime state.
    pub const MAC_TSD_SLOT: u32 = 700;
    /// Pseudo data-item ids used in address fixups: the macOS GOT, and `FUNC_FLAG | function index`.
    pub const GOT_ID: u32 = u32::MAX;
    pub const FUNC_FLAG: u32 = 0x8000_0000;
    /// Name of the runtime function every new thread starts in.
    pub const THREAD_ENTRY: &str = "fl_thread_entry";
}

/// Name of the function the process entry point calls with the initial stack pointer.
pub const ENTRY_FN: &str = "fl_start";

fn align(x: u64, a: u64) -> u64 {
    x.div_ceil(a) * a
}

/// The machine code of a whole program, before addresses are known.
struct Code {
    text: Vec<u8>,
    starts: Vec<usize>,
    ends: Vec<usize>,
    /// Position of the process entry code inside `text`.
    entry: usize,
    /// 16-byte constants the code refers to (x86-64 only).
    pool: Vec<[u8; 16]>,
    /// (patch position, pool entry): absolute 32-bit address operands (x86-64)
    pool_fix: Vec<(usize, usize)>,
    /// (patch position, data item): absolute 32-bit address operands (x86-64)
    data_fix_abs: Vec<(usize, u32)>,
    /// (position of an `adrp`, data item): PC-relative address pairs (AArch64)
    data_fix_adrp: Vec<(usize, u32)>,
    /// (position of the call instruction, callee): relative calls
    calls: Vec<(usize, u32)>,
    /// (position of a rip-relative operand, import slot): calls through the import table (Windows)
    iat_fix: Vec<(usize, u32)>,
}

fn emit_x86(m: &Module, target: Target, entry_idx: usize) -> Result<Code, String> {
    let mut cg = codegen::Cg::new(m, target);
    // Process entry: pass the initial stack pointer (argc, argv, envp) to the runtime.
    //   mov rdi, rsp ; and rsp, -16 ; call fl_start ; ud2
    cg.a.buf.extend_from_slice(&[0x48, 0x89, 0xE7, 0x48, 0x83, 0xE4, 0xF0]);
    let at = cg.a.call_patch();
    cg.fix.calls.push((at, entry_idx as u32));
    cg.a.ud2();
    let mut starts = vec![0usize; m.funcs.len()];
    let mut ends = vec![0usize; m.funcs.len()];
    for (i, f) in m.funcs.iter().enumerate() {
        let (s, e) = cg.emit_function(f).map_err(|e| format!("in function `{}`: {}", f.name, e))?;
        starts[i] = s;
        ends[i] = e;
    }
    cg.a.resolve_labels()?;
    let codegen::Cg { a, fix, pool, .. } = cg;
    Ok(Code {
        text: a.buf,
        starts,
        ends,
        entry: 0,
        pool: pool.entries,
        pool_fix: fix.pool,
        data_fix_abs: fix.data,
        data_fix_adrp: vec![],
        calls: fix.calls,
        iat_fix: fix.iat,
    })
}

fn emit_a64(m: &Module, target: Target, entry_idx: usize) -> Result<Code, String> {
    let mut cg = a64gen::Cg::new(m, target);
    // Process entry: pass the initial stack pointer (argc, argv, envp) to the runtime.
    //   mov x0, sp ; bl fl_start ; brk #0          (Linux: the kernel's initial stack)
    //   sub x0, x1, #8 ; bl fl_start ; brk #0      (macOS: dyld calls main(argc, argv, ...); argc is the word before argv)
    if target.os == Os::MacOs {
        cg.a.sub_imm(0, 1, 8);
    } else {
        cg.a.mov_sp(0, a64::SP);
    }
    let at = cg.a.bl_patch();
    cg.fix.calls.push((at, entry_idx as u32));
    cg.a.brk(0);
    let mut starts = vec![0usize; m.funcs.len()];
    let mut ends = vec![0usize; m.funcs.len()];
    for (i, f) in m.funcs.iter().enumerate() {
        let (s, e) = cg.emit_function(f).map_err(|e| format!("in function `{}`: {}", f.name, e))?;
        starts[i] = s;
        ends[i] = e;
    }
    cg.a.resolve_labels()?;
    let a64gen::Cg { a, fix, .. } = cg;
    Ok(Code { text: a.buf, starts, ends, entry: 0, pool: vec![], pool_fix: vec![], data_fix_abs: vec![], data_fix_adrp: fix.data, calls: fix.calls, iat_fix: vec![] })
}

/// Compile a fully linked module (no unresolved externs) into an executable file image.
pub fn compile(mut m: Module, target: Target) -> Result<Vec<u8>, String> {
    for f in &mut m.funcs {
        addrfold::run(f);
    }
    let m = &m;
    let entry_idx =
        m.funcs.iter().position(|f| f.name == ENTRY_FN).ok_or_else(|| format!("internal error: the runtime has no `{}` function", ENTRY_FN))?;
    let mut code = match target.arch {
        Arch::X86_64 => emit_x86(m, target, entry_idx)?,
        Arch::Aarch64 => emit_a64(m, target, entry_idx)?,
    };

    // Read-only data: constant pool (16-byte entries), then read-only items.
    let mut rodata: Vec<u8> = vec![];
    let mut pool_off = vec![];
    for e in &code.pool {
        pool_off.push(rodata.len() as u64);
        rodata.extend_from_slice(e);
    }
    // (section, offset): 0 = rodata, 1 = data, 2 = bss
    let mut item_loc: Vec<(u8, u64)> = vec![(0, 0); m.data.len()];
    for (i, d) in m.data.iter().enumerate() {
        if !d.writable {
            rodata.resize(align(rodata.len() as u64, d.align.max(1) as u64) as usize, 0);
            item_loc[i] = (0, rodata.len() as u64);
            rodata.extend_from_slice(&d.bytes);
        }
    }
    // macOS arm64 programs that start threads import pthread functions through a GOT at the very start of the data segment
    let mac_threads = target.os == Os::MacOs && target.arch == Arch::Aarch64 && m.funcs.iter().any(|f| f.name == "fl_thread_spawn");
    let mut data: Vec<u8> = if mac_threads { vec![0; 8 * pseudo::MAC_IMPORTS.len()] } else { vec![] };
    let mut bss: u64 = 0;
    for (i, d) in m.data.iter().enumerate() {
        if d.writable {
            let al = d.align.max(1) as u64;
            if d.bytes.iter().all(|b| *b == 0) {
                bss = align(bss, al);
                item_loc[i] = (2, bss);
                bss += d.bytes.len() as u64;
            } else {
                data.resize(align(data.len() as u64, al) as usize, 0);
                item_loc[i] = (1, data.len() as u64);
                data.extend_from_slice(&d.bytes);
            }
        }
    }
    let (base, page) = target.image_params();
    // Windows: the import tables live at the end of the read-only data. The read-only data starts
    // right after the code, so its address is known before the final layout.
    let mut imports: Option<(pe::ImportBlock, u64)> = None;
    if target.os == Os::Windows {
        let ro_rva = elf::layout(base, page, code.text.len() as u64, 0, 0).rodata - base;
        rodata.resize(align(rodata.len() as u64, 16) as usize, 0);
        let off = rodata.len() as u64;
        let block = pe::import_block(ro_rva + off);
        rodata.extend_from_slice(&block.bytes);
        imports = Some((block, off));
    }
    let lay = elf::layout(base, page, code.text.len() as u64, rodata.len() as u64, data.len() as u64);
    let starts = code.starts.clone();
    let data_addr = |id: u32| -> u64 {
        if id == pseudo::GOT_ID {
            return lay.data;
        }
        if id & pseudo::FUNC_FLAG != 0 {
            return lay.text + starts[(id & !pseudo::FUNC_FLAG) as usize] as u64;
        }
        let (sec, off) = item_loc[id as usize];
        match sec {
            0 => lay.rodata + off,
            1 => lay.data + off,
            _ => lay.bss + off,
        }
    };

    // Patch addresses and calls.
    let text = &mut code.text;
    match target.arch {
        Arch::X86_64 => {
            let patch32 = |text: &mut Vec<u8>, at: usize, v: u32| text[at..at + 4].copy_from_slice(&v.to_le_bytes());
            for (at, callee) in &code.calls {
                let rel = code.starts[*callee as usize] as i64 - (*at as i64 + 4);
                patch32(text, *at, rel as i32 as u32);
            }
            for (at, id) in &code.data_fix_abs {
                let addr = data_addr(*id);
                if addr > u32::MAX as u64 {
                    return Err("internal error: a data address does not fit 32 bits".into());
                }
                patch32(text, *at, addr as u32);
            }
            for (at, idx) in &code.pool_fix {
                patch32(text, *at, (lay.rodata + pool_off[*idx]) as u32);
            }
        }
        Arch::Aarch64 => {
            let word = |text: &Vec<u8>, at: usize| u32::from_le_bytes(text[at..at + 4].try_into().unwrap());
            let put = |text: &mut Vec<u8>, at: usize, w: u32| text[at..at + 4].copy_from_slice(&w.to_le_bytes());
            for (at, callee) in &code.calls {
                let delta = (code.starts[*callee as usize] as i64 - *at as i64) / 4;
                if !(-(1 << 25)..(1 << 25)).contains(&delta) {
                    return Err("a call target is more than 128 MiB away".into());
                }
                let w = word(text, *at) | (delta as u32 & 0x03FF_FFFF);
                put(text, *at, w);
            }
            for (at, id) in &code.data_fix_adrp {
                // adrp rd, page(target) ; add rd, rd, #(target & 0xfff)
                let target_addr = data_addr(*id);
                let pc = lay.text + *at as u64;
                let pages = (target_addr >> 12) as i64 - (pc >> 12) as i64;
                if !(-(1 << 20)..(1 << 20)).contains(&pages) {
                    return Err("a data address is more than 4 GiB from the code".into());
                }
                let w = word(text, *at) | ((pages as u32 & 3) << 29) | (((pages as u32 >> 2) & 0x7FFFF) << 5);
                put(text, *at, w);
                let w2 = word(text, *at + 4) | (((target_addr & 0xFFF) as u32) << 10);
                put(text, *at + 4, w2);
            }
        }
    }

    if let Some((block, off)) = &imports {
        let iat_addr = lay.rodata + off + block.iat_off as u64;
        for (at, slot) in &code.iat_fix {
            if *slot as usize >= pe::IMPORTS.len() {
                return Err(format!("internal error: unknown Windows import number {}", slot));
            }
            let target_addr = iat_addr + 8 * *slot as u64;
            let rel = target_addr as i64 - (lay.text as i64 + *at as i64 + 4);
            code.text[*at..*at + 4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
    }
    let symbols: Vec<elf::Symbol> = m
        .funcs
        .iter()
        .enumerate()
        .map(|(i, f)| elf::Symbol { name: f.name.clone(), addr: lay.text + code.starts[i] as u64, size: (code.ends[i] - code.starts[i]) as u64 })
        .collect();
    let mac_imports: &[&str] = if mac_threads { &pseudo::MAC_IMPORTS } else { &[] };
    let img = elf::Image { text: &code.text, rodata: &rodata, data: &data, bss, entry: lay.text + code.entry as u64, symbols: &symbols, imports: mac_imports };
    if let Some((block, off)) = &imports {
        return Ok(pe::write(&img, &lay, (off + block.dir_off as u64, block.dir_size as u64), (off + block.iat_off as u64, 8 * (pe::IMPORTS.len() as u64 + 1))));
    }
    Ok(match (target.os, target.arch) {
        (Os::Linux, Arch::X86_64) => elf::write(&img, &lay, elf::Machine::X86_64),
        (Os::Linux, Arch::Aarch64) => elf::write(&img, &lay, elf::Machine::Aarch64),
        (Os::MacOs, Arch::X86_64) => macho::write(&img, &lay, macho::Arch::X86_64, "a.out"),
        (Os::MacOs, Arch::Aarch64) => macho::write(&img, &lay, macho::Arch::Arm64, "a.out"),
        (Os::Windows, _) => unreachable!("written above"),
    })
}
