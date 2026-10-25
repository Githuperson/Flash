//! Minimal static Mach-O 64-bit executable writer (macOS, x86-64 and arm64).
//!
//! The executable has no dynamic linker and no libraries: it starts at an `LC_UNIXTHREAD` entry
//! (the same mechanism as the C runtime's `start` before dyld existed) and talks to the kernel
//! with BSD system calls. `__PAGEZERO` is only 4 MiB so that code and data live at low
//! addresses that fit the 32-bit absolute operands the x86-64 code generator uses. arm64
//! executables use a 4 GiB `__PAGEZERO`, 16 KiB pages, PC-relative addressing and an ad-hoc code
//! signature (Apple Silicon refuses unsigned arm64 code).

use super::codesign;
use super::elf::{Image, Layout};

const MH_MAGIC_64: u32 = 0xFEED_FACF;
const CPU_TYPE_X86_64: u32 = 0x0100_0007;
const CPU_SUBTYPE_X86_64_ALL: u32 = 3;
const CPU_TYPE_ARM64: u32 = 0x0100_000C;
const CPU_SUBTYPE_ARM64_ALL: u32 = 0;
const LC_CODE_SIGNATURE: u32 = 0x1D;
const LC_BUILD_VERSION: u32 = 0x32;
const ARM_THREAD_STATE64: u32 = 6;
const ARM_THREAD_STATE64_COUNT: u32 = 68;
const PLATFORM_MACOS: u32 = 1;
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Arm64,
}

pub fn write(img: &Image, l: &Layout, arch: Arch, ident: &str) -> Vec<u8> {
    let page = l.page;
    let text_off = l.text - l.base;
    let ro_off = l.rodata - l.base;
    let data_off = l.data - l.base;
    let data_filesz = align(img.data.len() as u64, 16);
    let data_vmsize = align(data_filesz + img.bss, page);
    let file_data_len = align(img.data.len() as u64, page).max(page);
    let text_seg_end = align(ro_off + img.rodata.len() as u64, page);

    let mut cmds: Vec<u8> = vec![];
    let mut ncmds = 0u32;
    // __PAGEZERO: no access below the image base
    segment(&mut cmds, "__PAGEZERO", 0, l.base, 0, 0, 0, 0);
    ncmds += 1;
    // __TEXT: headers, code and constants (read + execute)
    segment(&mut cmds, "__TEXT", l.base, text_seg_end, 0, text_seg_end, VM_PROT_R | VM_PROT_X, 2);
    section(&mut cmds, "__text", "__TEXT", l.text, img.text.len() as u64, text_off as u32, 4, S_REGULAR_CODE);
    section(&mut cmds, "__const", "__TEXT", l.rodata, img.rodata.len() as u64, ro_off as u32, 4, 0);
    ncmds += 1;
    // __DATA: initialised data and zero-filled memory
    segment(&mut cmds, "__DATA", l.data, data_vmsize, data_off, file_data_len, VM_PROT_R | VM_PROT_W, 2);
    section(&mut cmds, "__data", "__DATA", l.data, img.data.len() as u64, data_off as u32, 4, 0);
    section(&mut cmds, "__bss", "__DATA", l.data + data_filesz, img.bss, 0, 4, S_ZEROFILL);
    ncmds += 1;
    // arm64 needs a signature, which lives in a __LINKEDIT segment at the end of the file.
    let signed_len = (data_off + file_data_len) as usize;
    let sig_size = if arch == Arch::Arm64 { codesign::signature_size(signed_len, ident) as u64 } else { 0 };
    if arch == Arch::Arm64 {
        let linkedit_vm = l.data + data_vmsize;
        segment(&mut cmds, "__LINKEDIT", linkedit_vm, align(sig_size, page), signed_len as u64, sig_size, VM_PROT_R, 0);
        ncmds += 1;
        put32(&mut cmds, LC_CODE_SIGNATURE);
        put32(&mut cmds, 16);
        put32(&mut cmds, signed_len as u32);
        put32(&mut cmds, sig_size as u32);
        ncmds += 1;
        // LC_BUILD_VERSION: macOS, minimum and SDK version 11.0, no tools
        put32(&mut cmds, LC_BUILD_VERSION);
        put32(&mut cmds, 24);
        put32(&mut cmds, PLATFORM_MACOS);
        put32(&mut cmds, 0x000B_0000);
        put32(&mut cmds, 0x000B_0000);
        put32(&mut cmds, 0);
        ncmds += 1;
    }
    // LC_UNIXTHREAD: initial register state; only the program counter is set (the kernel provides the stack).
    put32(&mut cmds, LC_UNIXTHREAD);
    match arch {
        Arch::X86_64 => {
            put32(&mut cmds, 16 + 21 * 8);
            put32(&mut cmds, X86_THREAD_STATE64);
            put32(&mut cmds, X86_THREAD_STATE64_COUNT);
            for r in 0..21 {
                put64(&mut cmds, if r == 16 { img.entry } else { 0 }); // rip is the 17th register
            }
        }
        Arch::Arm64 => {
            put32(&mut cmds, 16 + 33 * 8 + 8);
            put32(&mut cmds, ARM_THREAD_STATE64);
            put32(&mut cmds, ARM_THREAD_STATE64_COUNT);
            for r in 0..33 {
                put64(&mut cmds, if r == 32 { img.entry } else { 0 }); // x0-x28, fp, lr, sp, then pc (index 32)
            }
            put32(&mut cmds, 0); // cpsr
            put32(&mut cmds, 0); // padding
        }
    }
    ncmds += 1;

    let mut out: Vec<u8> = vec![];
    put32(&mut out, MH_MAGIC_64);
    match arch {
        Arch::X86_64 => {
            put32(&mut out, CPU_TYPE_X86_64);
            put32(&mut out, CPU_SUBTYPE_X86_64_ALL);
        }
        Arch::Arm64 => {
            put32(&mut out, CPU_TYPE_ARM64);
            put32(&mut out, CPU_SUBTYPE_ARM64_ALL);
        }
    }
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
    out.resize(signed_len, 0);
    if arch == Arch::Arm64 {
        let sig = codesign::ad_hoc(&out, text_seg_end, ident);
        out.extend_from_slice(&sig);
    }
    out
}
