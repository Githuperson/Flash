//! Minimal PE32+ (Windows x86-64) console executable writer.
//!
//! The executable imports its operating-system services from kernel32.dll: the runtime calls
//! them through the import address table (`call [rip+disp]`, see the code generator). There are
//! no relocations, so the image is loaded at its preferred base (below 4 GiB, like the Linux and
//! macOS images, because the x86-64 code generator uses 32-bit absolute data addresses).

use super::elf::{Image, Layout};

/// The kernel32.dll functions the runtime uses; the index is the `__winapi` slot number
/// (keep in sync with runtime/win.flash).
pub const IMPORTS: [&str; 19] = [
    "GetStdHandle",
    "WriteFile",
    "ReadFile",
    "CreateFileA",
    "CloseHandle",
    "VirtualAlloc",
    "VirtualFree",
    "ExitProcess",
    "QueryPerformanceCounter",
    "QueryPerformanceFrequency",
    "Sleep",
    "GetFileAttributesA",
    "DeleteFileA",
    "GetCommandLineA",
    "GetEnvironmentStringsA",
    "GetLastError",
    "CreateThread",
    "WaitForSingleObject",
    "SwitchToThread",
];

const DLL: &str = "KERNEL32.dll";
const FILE_ALIGN: u64 = 0x200;

fn align(x: u64, a: u64) -> u64 {
    x.div_ceil(a) * a
}
fn put16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn put32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn put64(v: &mut Vec<u8>, x: u64) {
    v.extend_from_slice(&x.to_le_bytes());
}

/// Where the import tables sit inside the read-only data.
pub struct ImportBlock {
    pub bytes: Vec<u8>,
    /// Offset of the import address table (8 bytes per import) inside `bytes`.
    pub iat_off: usize,
    /// Offset and size of the import directory (the two descriptors).
    pub dir_off: usize,
    pub dir_size: usize,
}

/// Build the import tables for a block that will live at relative virtual address `rva`.
pub fn import_block(rva: u64) -> ImportBlock {
    let n = IMPORTS.len();
    let mut b: Vec<u8> = vec![];
    let dir_off = 0usize;
    let dir_size = 40usize;
    let ilt_off = dir_size;
    let iat_off = ilt_off + 8 * (n + 1);
    let names_off = iat_off + 8 * (n + 1);
    // hint/name entries
    let mut hint_rvas = vec![];
    let mut names: Vec<u8> = vec![];
    for name in IMPORTS {
        hint_rvas.push(rva + (names_off + names.len()) as u64);
        names.extend_from_slice(&0u16.to_le_bytes());
        names.extend_from_slice(name.as_bytes());
        names.push(0);
        if names.len() % 2 == 1 {
            names.push(0);
        }
    }
    let dll_off = names_off + names.len();
    // descriptors: OriginalFirstThunk, TimeDateStamp, ForwarderChain, Name, FirstThunk
    put32(&mut b, (rva + ilt_off as u64) as u32);
    put32(&mut b, 0);
    put32(&mut b, 0);
    put32(&mut b, (rva + dll_off as u64) as u32);
    put32(&mut b, (rva + iat_off as u64) as u32);
    b.resize(dir_size, 0); // terminating all-zero descriptor
    for _ in 0..2 {
        for r in &hint_rvas {
            put64(&mut b, *r);
        }
        put64(&mut b, 0);
    }
    b.extend_from_slice(&names);
    b.extend_from_slice(DLL.as_bytes());
    b.push(0);
    ImportBlock { bytes: b, iat_off, dir_off, dir_size }
}

/// Write the executable. `imports` is the (offset, size) of the import block inside the read-only
/// data and `iat` the (offset, size) of the address table inside it.
pub fn write(img: &Image, l: &Layout, imports: (u64, u64), iat: (u64, u64)) -> Vec<u8> {
    let sect_align = l.page;
    let headers_size = align(0x40 + 4 + 20 + 240 + 3 * 40 + 0x40, FILE_ALIGN);
    // raw sizes (file) and virtual sizes (memory) of the sections
    let text_raw = align(img.text.len() as u64, FILE_ALIGN);
    let ro_raw = align(img.rodata.len() as u64, FILE_ALIGN);
    let data_len = img.data.len() as u64;
    let data_raw = align(data_len, FILE_ALIGN);
    let data_virt = (l.bss - l.data) + img.bss;
    let text_rva = l.text - l.base;
    let ro_rva = l.rodata - l.base;
    let data_rva = l.data - l.base;
    let image_size = align(data_rva + data_virt.max(1), sect_align);
    let text_off = headers_size;
    let ro_off = text_off + text_raw;
    let data_off = ro_off + ro_raw;

    let mut out: Vec<u8> = vec![];
    // DOS header: only "MZ" and the offset of the PE header matter
    out.extend_from_slice(b"MZ");
    out.resize(0x3C, 0);
    put32(&mut out, 0x40);
    out.resize(0x40, 0);
    out.extend_from_slice(b"PE\0\0");
    // COFF header
    put16(&mut out, 0x8664); // AMD64
    put16(&mut out, 3); // sections
    put32(&mut out, 0); // time stamp
    put32(&mut out, 0);
    put32(&mut out, 0);
    put16(&mut out, 240); // size of optional header
    put16(&mut out, 0x0001 | 0x0002 | 0x0020); // relocs stripped, executable, large address aware
    // optional header (PE32+)
    put16(&mut out, 0x20B);
    out.push(14);
    out.push(0);
    put32(&mut out, text_raw as u32); // size of code
    put32(&mut out, (ro_raw + data_raw) as u32); // initialised data
    put32(&mut out, 0); // uninitialised data
    put32(&mut out, (img.entry - l.base) as u32);
    put32(&mut out, text_rva as u32); // base of code
    put64(&mut out, l.base);
    put32(&mut out, sect_align as u32);
    put32(&mut out, FILE_ALIGN as u32);
    put16(&mut out, 6); // OS version
    put16(&mut out, 0);
    put16(&mut out, 0); // image version
    put16(&mut out, 0);
    put16(&mut out, 6); // subsystem version
    put16(&mut out, 0);
    put32(&mut out, 0); // win32 version
    put32(&mut out, image_size as u32);
    put32(&mut out, headers_size as u32);
    put32(&mut out, 0); // checksum
    put16(&mut out, 3); // console subsystem
    put16(&mut out, 0x8000 | 0x0100); // terminal server aware, NX compatible (no dynamic base)
    put64(&mut out, 8 << 20); // stack reserve
    put64(&mut out, 0x10000); // stack commit
    put64(&mut out, 1 << 20); // heap reserve
    put64(&mut out, 0x1000); // heap commit
    put32(&mut out, 0); // loader flags
    put32(&mut out, 16); // data directories
    for i in 0..16 {
        let (rva, size) = match i {
            1 => (ro_rva + imports.0, imports.1),
            12 => (ro_rva + iat.0, iat.1),
            _ => (0, 0),
        };
        put32(&mut out, rva as u32);
        put32(&mut out, size as u32);
    }
    // section table
    let mut section = |name: &str, virt: u64, rva: u64, raw: u64, off: u64, flags: u32| {
        let mut n = [0u8; 8];
        n[..name.len()].copy_from_slice(name.as_bytes());
        out.extend_from_slice(&n);
        put32(&mut out, virt as u32);
        put32(&mut out, rva as u32);
        put32(&mut out, raw as u32);
        put32(&mut out, if raw == 0 { 0 } else { off as u32 });
        put32(&mut out, 0);
        put32(&mut out, 0);
        put16(&mut out, 0);
        put16(&mut out, 0);
        put32(&mut out, flags);
    };
    section(".text", img.text.len() as u64, text_rva, text_raw, text_off, 0x6000_0020);
    section(".rdata", img.rodata.len() as u64, ro_rva, ro_raw, ro_off, 0x4000_0040);
    section(".data", data_virt, data_rva, data_raw, data_off, 0xC000_0040);
    assert!(out.len() as u64 <= headers_size);
    out.resize(headers_size as usize, 0);
    out.extend_from_slice(img.text);
    out.resize((text_off + text_raw) as usize, 0);
    out.extend_from_slice(img.rodata);
    out.resize((ro_off + ro_raw) as usize, 0);
    out.extend_from_slice(img.data);
    out.resize((data_off + data_raw) as usize, 0);
    out
}
