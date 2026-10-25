//! Minimal static Mach-O 64-bit executable writer (macOS, x86-64).
//!
//! The executable has no dynamic linker and no libraries: it starts at an `LC_UNIXTHREAD` entry
//! (the same mechanism as the C runtime's `start` before dyld existed) and talks to the kernel
//! with BSD system calls. `__PAGEZERO` is only 4 MiB so that code and data live at low
//! addresses that fit the 32-bit absolute operands the code generator uses.

use super::elf::{layout, Image, BASE};

const MH_MAGIC_64: u32 = 0xFEED_FACF;
const CPU_TYPE_X86_64: u32 = 0x0100_0007;
const CPU_SUBTYPE_X86_64_ALL: u32 = 3;
const MH_EXECUTE: u32 = 2;
const MH_NOUNDEFS: u32 = 1;
const LC_SEGMENT_64: u32 = 0x19;
const LC_UNIXTHREAD: u32 = 0x5;
const X86_THREAD_STATE64: u32 = 4;
const X86_THREAD_STATE64_COUNT: u32 = 42;
const S_ZEROFILL: u32 = 1;
const S_REGULAR_CODE: u32 = 0x8000_0400; // S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS
const VM_PROT_R: i32 = 1;
const VM_PROT_W: i32 = 2;
const VM_PROT_X: i32 = 4;
const PAGE: u64 = 0x1000;

fn put32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn put64(v: &mut Vec<u8>, x: u64) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn name16(v: &mut Vec<u8>, s: &str) {
    let mut b = [0u8; 16];
    b[..s.len()].copy_from_slice(s.as_bytes());
    v.extend_from_slice(&b);
}
fn align(x: u64, a: u64) -> u64 {
    x.div_ceil(a) * a
}

#[allow(clippy::too_many_arguments)]
fn segment(v: &mut Vec<u8>, name: &str, vmaddr: u64, vmsize: u64, fileoff: u64, filesize: u64, prot: i32, nsects: u32) {
    put32(v, LC_SEGMENT_64);
    put32(v, 72 + 80 * nsects);
    name16(v, name);
    put64(v, vmaddr);
    put64(v, vmsize);
    put64(v, fileoff);
    put64(v, filesize);
    put32(v, prot as u32); // maxprot
    put32(v, prot as u32); // initprot
    put32(v, nsects);
    put32(v, 0);
}

#[allow(clippy::too_many_arguments)]
fn section(v: &mut Vec<u8>, sect: &str, seg: &str, addr: u64, size: u64, offset: u32, align_pow2: u32, flags: u32) {
    name16(v, sect);
    name16(v, seg);
    put64(v, addr);
    put64(v, size);
    put32(v, offset);
    put32(v, align_pow2);
    put32(v, 0); // reloff
    put32(v, 0); // nreloc
    put32(v, flags);
    put32(v, 0);
    put32(v, 0);
    put32(v, 0);
}

pub fn write(img: &Image) -> Vec<u8> {
    // Header + load commands, then page-aligned text, rodata and data, mirroring the ELF layout:
    // the first page holds the headers, so everything keeps the same virtual addresses.
    let l = layout(img.text.len() as u64, img.rodata.len() as u64, img.data.len() as u64);
    let text_off = l.text - BASE;
    let ro_off = l.rodata - BASE;
    let data_off = l.data - BASE;
    let data_filesz = align(img.data.len() as u64, 16);
    let data_vmsize = align(data_filesz + img.bss, PAGE);
    let file_data_len = align(img.data.len() as u64, PAGE).max(PAGE);

    let mut cmds: Vec<u8> = vec![];
    // __PAGEZERO: no access to the first 4 MiB
    segment(&mut cmds, "__PAGEZERO", 0, BASE, 0, 0, 0, 0);
    // __TEXT: headers, code and constants (read + execute)
    let text_seg_end = align(ro_off + img.rodata.len() as u64, PAGE);
    segment(&mut cmds, "__TEXT", BASE, text_seg_end, 0, text_seg_end, VM_PROT_R | VM_PROT_X, 2);
    section(&mut cmds, "__text", "__TEXT", l.text, img.text.len() as u64, text_off as u32, 4, S_REGULAR_CODE);
    section(&mut cmds, "__const", "__TEXT", l.rodata, img.rodata.len() as u64, ro_off as u32, 4, 0);
    // __DATA: initialised data and zero-filled memory
    segment(&mut cmds, "__DATA", l.data, data_vmsize, data_off, file_data_len, VM_PROT_R | VM_PROT_W, 2);
    section(&mut cmds, "__data", "__DATA", l.data, img.data.len() as u64, data_off as u32, 4, 0);
    section(&mut cmds, "__bss", "__DATA", l.data + data_filesz, img.bss, 0, 4, S_ZEROFILL);
    // LC_UNIXTHREAD: initial register state; only rip is set (the kernel provides the stack).
    put32(&mut cmds, LC_UNIXTHREAD);
    put32(&mut cmds, 16 + 21 * 8);
    put32(&mut cmds, X86_THREAD_STATE64);
    put32(&mut cmds, X86_THREAD_STATE64_COUNT);
    for r in 0..21 {
        put64(&mut cmds, if r == 16 { img.entry } else { 0 }); // rip is the 17th register
    }
    let ncmds = 4; // __PAGEZERO, __TEXT, __DATA, LC_UNIXTHREAD

    let mut out: Vec<u8> = vec![];
    put32(&mut out, MH_MAGIC_64);
    put32(&mut out, CPU_TYPE_X86_64);
    put32(&mut out, CPU_SUBTYPE_X86_64_ALL);
    put32(&mut out, MH_EXECUTE);
    put32(&mut out, ncmds);
    put32(&mut out, cmds.len() as u32);
    put32(&mut out, MH_NOUNDEFS);
    put32(&mut out, 0);
    out.extend_from_slice(&cmds);
    assert!(out.len() as u64 <= text_off, "Mach-O load commands do not fit in the first page");
    out.resize(text_off as usize, 0);
    out.extend_from_slice(img.text);
    out.resize(ro_off as usize, 0);
    out.extend_from_slice(img.rodata);
    out.resize(text_seg_end as usize, 0);
    assert_eq!(out.len() as u64, data_off);
    out.extend_from_slice(img.data);
    out.resize((data_off + file_data_len) as usize, 0);
    out
}
