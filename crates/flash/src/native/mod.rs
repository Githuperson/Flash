//! Native back end: IR → x86-64 machine code → static ELF executable, with no external
//! compiler, assembler, linker or C library involved.
//!
//! Programs call no libc: the runtime library (strings, lists, memory, I/O) is itself written in
//! Flash (see `runtime/*.flash`), compiled by the same pipeline, and talks to the Linux kernel
//! through raw system calls.

mod addrfold;
mod asm;
mod codegen;
mod magic;
mod elf;
mod macho;
pub mod link;
mod regalloc;

use crate::ir::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux = 0,
    MacOs = 1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86_64,
}

/// What the generated executable runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub os: Os,
    pub arch: Arch,
}

impl Target {
    /// The machine the compiler runs on, if the native back end supports it.
    pub fn host() -> Option<Target> {
        if cfg!(target_arch = "x86_64") && cfg!(target_os = "linux") {
            Some(Target { os: Os::Linux, arch: Arch::X86_64 })
        } else if cfg!(target_arch = "x86_64") && cfg!(target_os = "macos") {
            Some(Target { os: Os::MacOs, arch: Arch::X86_64 })
        } else {
            None
        }
    }

    pub fn parse(triple: &str) -> Result<Target, String> {
        let t = triple.to_ascii_lowercase();
        let arch = if t.starts_with("x86_64") || t.starts_with("amd64") {
            Arch::X86_64
        } else {
            return Err(format!("the native back end cannot generate code for '{}' (supported: x86_64-unknown-linux-gnu, x86_64-apple-darwin)", triple));
        };
        let os = if t.contains("linux") {
            Os::Linux
        } else if t.contains("darwin") || t.contains("apple") || t.contains("macos") {
            Os::MacOs
        } else {
            return Err(format!("the native back end cannot generate code for '{}' (supported: x86_64-unknown-linux-gnu, x86_64-apple-darwin)", triple));
        };
        Ok(Target { os, arch })
    }
}

/// Name of the function the process entry point calls with the initial stack pointer.
pub const ENTRY_FN: &str = "fl_start";

fn align(x: u64, a: u64) -> u64 {
    x.div_ceil(a) * a
}

/// Compile a fully linked module (no unresolved externs) into an ELF executable image.
pub fn compile(mut m: Module, target: Target) -> Result<Vec<u8>, String> {
    for f in &mut m.funcs {
        addrfold::run(f);
    }
    let m = &m;
    let entry_idx =
        m.funcs.iter().position(|f| f.name == ENTRY_FN).ok_or_else(|| format!("internal error: the runtime has no `{}` function", ENTRY_FN))?;
    let mut cg = codegen::Cg::new(m, target);

    // Process entry: pass the initial stack pointer (argc, argv, envp) to the runtime.
    //   mov rdi, rsp ; and rsp, -16 ; call fl_start ; ud2
    let start_off = 0usize;
    cg.a.buf.extend_from_slice(&[0x48, 0x89, 0xE7, 0x48, 0x83, 0xE4, 0xF0]);
    let at = cg.a.call_patch();
    cg.fix.calls.push((at, entry_idx as u32));
    cg.a.ud2();
    let _ = start_off;

    let mut starts = vec![0usize; m.funcs.len()];
    let mut ends = vec![0usize; m.funcs.len()];
    for (i, f) in m.funcs.iter().enumerate() {
        let (s, e) = cg.emit_function(f).map_err(|e| format!("in function `{}`: {}", f.name, e))?;
        starts[i] = s;
        ends[i] = e;
    }

    cg.a.resolve_labels()?;
    let codegen::Cg { a, fix, pool, .. } = cg;
    let mut text = a.buf;

    // Read-only data: constant pool (16-byte entries), then read-only items.
    let mut rodata: Vec<u8> = vec![];
    let mut pool_off = vec![];
    for e in &pool.entries {
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
    let mut data: Vec<u8> = vec![];
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
    let lay = elf::layout(text.len() as u64, rodata.len() as u64, data.len() as u64);
    let data_addr = |id: u32| -> u64 {
        let (sec, off) = item_loc[id as usize];
        match sec {
            0 => lay.rodata + off,
            1 => lay.data + off,
            _ => lay.bss + off,
        }
    };

    let patch32 = |text: &mut Vec<u8>, at: usize, v: u32| text[at..at + 4].copy_from_slice(&v.to_le_bytes());
    for (at, target) in &fix.calls {
        let rel = starts[*target as usize] as i64 - (*at as i64 + 4);
        patch32(&mut text, *at, rel as i32 as u32);
    }
    for (at, id) in &fix.data {
        patch32(&mut text, *at, data_addr(*id) as u32);
    }
    for (at, idx) in &fix.pool {
        patch32(&mut text, *at, (lay.rodata + pool_off[*idx]) as u32);
    }

    let symbols: Vec<elf::Symbol> = m
        .funcs
        .iter()
        .enumerate()
        .map(|(i, f)| elf::Symbol { name: f.name.clone(), addr: lay.text + starts[i] as u64, size: (ends[i] - starts[i]) as u64 })
        .collect();
    let img = elf::Image { text: &text, rodata: &rodata, data: &data, bss, entry: lay.text + start_off as u64, symbols: &symbols };
    Ok(match target.os {
        Os::Linux => elf::write(&img),
        Os::MacOs => macho::write(&img),
    })
}
