//! Minimal Mach-O 64-bit executable writer (macOS, x86-64 and arm64).
//!
//! x86-64: a static executable with no dynamic linker and no libraries. It starts at an
//! `LC_UNIXTHREAD` entry and talks to the kernel with BSD system calls. `__PAGEZERO` is only
//! 4 MiB so that code and data live at low addresses that fit the 32-bit absolute operands the
//! x86-64 code generator uses.
//!
//! arm64: a *dynamic* executable, because current macOS on Apple Silicon kills static ones
//! (measured on a macOS 26 runner: a correctly signed static file is rejected, while a file with
//! `LC_MAIN`, dyld and a libSystem dependency runs). The program still makes raw BSD system calls
//! and never calls into libSystem; dyld only loads the image and jumps to `LC_MAIN`. The code
//! is position independent (PC-relative addressing), so the image is PIE and may slide. The file
//! has a 4 GiB `__PAGEZERO`, 16 KiB pages and an ad-hoc code signature (Apple Silicon refuses
//! unsigned arm64 code).

use super::codesign;
use super::elf::{Image, Layout};

const MH_MAGIC_64: u32 = 0xFEED_FACF;
const CPU_TYPE_X86_64: u32 = 0x0100_0007;
const CPU_SUBTYPE_X86_64_ALL: u32 = 3;
const CPU_TYPE_ARM64: u32 = 0x0100_000C;
const CPU_SUBTYPE_ARM64_ALL: u32 = 0;
const LC_CODE_SIGNATURE: u32 = 0x1D;
const LC_BUILD_VERSION: u32 = 0x32;
const PLATFORM_MACOS: u32 = 1;
const MH_EXECUTE: u32 = 2;
const MH_NOUNDEFS: u32 = 1;
const LC_SEGMENT_64: u32 = 0x19;
const LC_UNIXTHREAD: u32 = 0x5;
const LC_SYMTAB: u32 = 0x2;
const LC_DYSYMTAB: u32 = 0xB;
const LC_LOAD_DYLIB: u32 = 0xC;
const LC_LOAD_DYLINKER: u32 = 0xE;
const LC_UUID: u32 = 0x1B;
const LC_MAIN: u32 = 0x8000_0028;
const LC_DYLD_EXPORTS_TRIE: u32 = 0x8000_0033;
const LC_DYLD_CHAINED_FIXUPS: u32 = 0x8000_0034;
const MH_DYLDLINK: u32 = 4;
const MH_TWOLEVEL: u32 = 0x80;
const MH_PIE: u32 = 0x20_0000;
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
    // arm64: __LINKEDIT holds the (empty) dyld tables and, last, the code signature.
    let linkedit_off = (data_off + file_data_len) as usize;
    let mut linkedit: Vec<u8> = vec![];
    let (mut cf_off, mut cf_len, mut trie_off, mut sym_off, mut str_off) = (0, 0, 0, 0, 0);
    if arch == Arch::Arm64 {
        // chained fixups: header, starts_in_image (one entry per segment, none with fixups), no imports
        cf_off = linkedit_off;
        put32(&mut linkedit, 0); // fixups_version
        put32(&mut linkedit, 32); // starts_offset
        put32(&mut linkedit, 56); // imports_offset
        put32(&mut linkedit, 56); // symbols_offset
        put32(&mut linkedit, 0); // imports_count
        put32(&mut linkedit, 1); // imports_format: DYLD_CHAINED_IMPORT
        put32(&mut linkedit, 0); // symbols_format
        put32(&mut linkedit, 0); // padding up to starts_offset
        put32(&mut linkedit, 4); // seg_count
        for _ in 0..4 {
            put32(&mut linkedit, 0); // seg_info_offset: no fixups in this segment
        }
        linkedit.resize(64, 0); // padding + empty symbol pool
        cf_len = linkedit.len();
        trie_off = linkedit_off + linkedit.len();
        linkedit.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]); // empty exports trie
        sym_off = linkedit_off + linkedit.len();
        str_off = sym_off;
        linkedit.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]); // string table: just the leading NUL
        while linkedit.len() % 16 != 0 {
            linkedit.push(0);
        }
    }
    let signed_len = linkedit_off + linkedit.len();
    let sig_size = if arch == Arch::Arm64 { codesign::signature_size(signed_len, ident) as u64 } else { 0 };
    if arch == Arch::Arm64 {
        let linkedit_vm = l.data + data_vmsize;
        let le_size = linkedit.len() as u64 + sig_size;
        segment(&mut cmds, "__LINKEDIT", linkedit_vm, align(le_size, page), linkedit_off as u64, le_size, VM_PROT_R, 0);
        ncmds += 1;
        let lcdata = |cmds: &mut Vec<u8>, cmd: u32, off: usize, size: usize| {
            put32(cmds, cmd);
            put32(cmds, 16);
            put32(cmds, off as u32);
            put32(cmds, size as u32);
        };
        lcdata(&mut cmds, LC_DYLD_CHAINED_FIXUPS, cf_off, cf_len);
        lcdata(&mut cmds, LC_DYLD_EXPORTS_TRIE, trie_off, 8);
        ncmds += 2;
        // LC_SYMTAB: no symbols
        put32(&mut cmds, LC_SYMTAB);
        put32(&mut cmds, 24);
        put32(&mut cmds, sym_off as u32);
        put32(&mut cmds, 0);
        put32(&mut cmds, str_off as u32);
        put32(&mut cmds, 8);
        ncmds += 1;
        put32(&mut cmds, LC_DYSYMTAB);
        put32(&mut cmds, 80);
        cmds.resize(cmds.len() + 72, 0);
        ncmds += 1;
        // dyld loads the image and calls LC_MAIN
        let dyld = b"/usr/lib/dyld\0";
        put32(&mut cmds, LC_LOAD_DYLINKER);
        put32(&mut cmds, 32);
        put32(&mut cmds, 12);
        cmds.extend_from_slice(dyld);
        cmds.resize(cmds.len() + (20 - dyld.len()), 0);
        ncmds += 1;
        // LC_UUID: derived from the code so that the same program has the same UUID
        put32(&mut cmds, LC_UUID);
        put32(&mut cmds, 24);
        let mut uuid = [0u8; 16];
        for (i, b) in img.text.iter().chain(img.rodata).chain(img.data).enumerate() {
            uuid[i % 16] = uuid[i % 16].wrapping_mul(31).wrapping_add(*b).wrapping_add(i as u8);
        }
        uuid[6] = (uuid[6] & 0x0F) | 0x40;
        uuid[8] = (uuid[8] & 0x3F) | 0x80;
        cmds.extend_from_slice(&uuid);
        ncmds += 1;
        // LC_BUILD_VERSION: macOS, minimum and SDK version 11.0, no tools
        put32(&mut cmds, LC_BUILD_VERSION);
        put32(&mut cmds, 24);
        put32(&mut cmds, PLATFORM_MACOS);
        put32(&mut cmds, 0x000B_0000);
        put32(&mut cmds, 0x000B_0000);
        put32(&mut cmds, 0);
        ncmds += 1;
        // LC_MAIN: the process entry (offset from the start of __TEXT)
        put32(&mut cmds, LC_MAIN);
        put32(&mut cmds, 24);
        put64(&mut cmds, img.entry - l.base);
        put64(&mut cmds, 0);
        ncmds += 1;
        // libSystem is loaded (dyld requires it) but never called
        let lib = b"/usr/lib/libSystem.B.dylib\0";
        put32(&mut cmds, LC_LOAD_DYLIB);
        put32(&mut cmds, 56);
        put32(&mut cmds, 24);
        put32(&mut cmds, 2); // timestamp
        put32(&mut cmds, 0x0538_0000); // current version 1336.0.0
        put32(&mut cmds, 0x0001_0000); // compatibility version 1.0.0
        cmds.extend_from_slice(lib);
        cmds.resize(cmds.len() + (32 - lib.len()), 0);
        ncmds += 1;
        lcdata(&mut cmds, LC_CODE_SIGNATURE, signed_len, sig_size as usize);
        ncmds += 1;
    }
    // LC_UNIXTHREAD (x86-64): initial register state; only the program counter is set (the kernel provides the stack).
    if arch == Arch::X86_64 {
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
        Arch::Arm64 => unreachable!(),
    }
    ncmds += 1;
    }

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
    put32(&mut out, if arch == Arch::Arm64 { MH_NOUNDEFS | MH_DYLDLINK | MH_TWOLEVEL | MH_PIE } else { MH_NOUNDEFS });
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
    out.resize(linkedit_off, 0);
    out.extend_from_slice(&linkedit);
    assert_eq!(out.len(), signed_len);
    if arch == Arch::Arm64 {
        let sig = codesign::ad_hoc(&out, text_seg_end, ident);
        out.extend_from_slice(&sig);
    }
    out
}
