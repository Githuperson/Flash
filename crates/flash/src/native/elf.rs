//! Minimal static ELF64 executable writer (Linux x86-64, no dynamic linking, no libc).

/// x86-64 Linux: executable image base and segment alignment.
pub const BASE: u64 = 0x40_0000;
pub const PAGE: u64 = 0x1000;

pub struct Symbol {
    pub name: String,
    pub addr: u64,
    pub size: u64,
}

/// Which instruction set the file is for.
#[derive(Clone, Copy)]
pub enum Machine {
    X86_64 = 62,
    Aarch64 = 183,
}

pub struct Image<'a> {
    pub text: &'a [u8],
    pub rodata: &'a [u8],
    pub data: &'a [u8],
    pub bss: u64,
    pub entry: u64,
    pub symbols: &'a [Symbol],
    /// macOS arm64: names of libSystem functions bound into the first words of the data segment
    pub imports: &'a [&'a str],
}

/// Virtual addresses of the segments for given section sizes.
pub struct Layout {
    pub base: u64,
    pub page: u64,
    pub text: u64,
    pub rodata: u64,
    pub data: u64,
    pub bss: u64,
}

fn align(x: u64, a: u64) -> u64 {
    x.div_ceil(a) * a
}

/// Segments are page aligned; the first page holds the file headers. File offset == virtual
/// address - `base` for every loaded segment.
pub fn layout(base: u64, page: u64, text_len: u64, rodata_len: u64, data_len: u64) -> Layout {
    let text = base + page;
    let rodata = base + align(text - base + text_len, page);
    let data = base + align(rodata - base + rodata_len, page);
    Layout { base, page, text, rodata, data, bss: data + align(data_len, 16) }
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

fn phdr(v: &mut Vec<u8>, kind: u32, flags: u32, off: u64, vaddr: u64, filesz: u64, memsz: u64, align: u64) {
    put32(v, kind);
    put32(v, flags);
    put64(v, off);
    put64(v, vaddr);
    put64(v, vaddr);
    put64(v, filesz);
    put64(v, memsz);
    put64(v, align);
}

#[allow(clippy::too_many_arguments)]
fn shdr(v: &mut Vec<u8>, name: u32, kind: u32, flags: u64, addr: u64, off: u64, size: u64, link: u32, info: u32, align: u64, entsize: u64) {
    put32(v, name);
    put32(v, kind);
    put64(v, flags);
    put64(v, addr);
    put64(v, off);
    put64(v, size);
    put32(v, link);
    put32(v, info);
    put64(v, align);
    put64(v, entsize);
}

pub fn write(img: &Image, l: &Layout, machine: Machine) -> Vec<u8> {
    let page = l.page;
    let text_off = l.text - l.base;
    let ro_off = l.rodata - l.base;
    let data_off = l.data - l.base;
    let mut out: Vec<u8> = vec![];

    // ELF header.
    out.extend_from_slice(&[0x7F, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    put16(&mut out, 2); // ET_EXEC
    put16(&mut out, machine as u16);
    put32(&mut out, 1);
    put64(&mut out, img.entry);
    put64(&mut out, 64); // phoff
    let shoff_pos = out.len();
    put64(&mut out, 0); // shoff, patched below
    put32(&mut out, 0);
    put16(&mut out, 64); // ehsize
    put16(&mut out, 56); // phentsize
    put16(&mut out, 4); // phnum
    put16(&mut out, 64); // shentsize
    put16(&mut out, 8); // shnum
    put16(&mut out, 7); // shstrndx

    let data_filesz = img.data.len() as u64;
    // PF_X=1, PF_W=2, PF_R=4
    phdr(&mut out, 1, 5, text_off, l.text, img.text.len() as u64, img.text.len() as u64, page);
    phdr(&mut out, 1, 4, ro_off, l.rodata, img.rodata.len() as u64, img.rodata.len() as u64, page);
    phdr(&mut out, 1, 6, data_off, l.data, data_filesz, (l.bss - l.data) + img.bss, page);
    phdr(&mut out, 0x6474_E551, 6, 0, 0, 0, 0, 16); // PT_GNU_STACK: non-executable stack

    let pad_to = |out: &mut Vec<u8>, off: u64| {
        assert!(out.len() as u64 <= off);
        out.resize(off as usize, 0);
    };
    pad_to(&mut out, text_off);
    out.extend_from_slice(img.text);
    pad_to(&mut out, ro_off);
    out.extend_from_slice(img.rodata);
    pad_to(&mut out, data_off);
    out.extend_from_slice(img.data);

    // Symbol and string tables (not loaded; for debuggers and `nm`).
    let mut strtab = vec![0u8];
    let mut symtab = vec![0u8; 24];
    for s in img.symbols {
        let name_off = strtab.len() as u32;
        strtab.extend_from_slice(s.name.as_bytes());
        strtab.push(0);
        put32(&mut symtab, name_off);
        symtab.push(0x02); // STB_LOCAL | STT_FUNC
        symtab.push(0);
        put16(&mut symtab, 1); // section index: .text
        put64(&mut symtab, s.addr);
        put64(&mut symtab, s.size);
    }
    let shstr_names = ["", ".text", ".rodata", ".data", ".bss", ".symtab", ".strtab", ".shstrtab"];
    let mut shstrtab = vec![];
    let mut name_offs = vec![];
    for n in shstr_names {
        name_offs.push(shstrtab.len() as u32);
        shstrtab.extend_from_slice(n.as_bytes());
        shstrtab.push(0);
    }
    let symtab_off = align(out.len() as u64, 8);
    pad_to(&mut out, symtab_off);
    out.extend_from_slice(&symtab);
    let strtab_off = out.len() as u64;
    out.extend_from_slice(&strtab);
    let shstr_off = out.len() as u64;
    out.extend_from_slice(&shstrtab);
    let sh_off = align(out.len() as u64, 8);
    pad_to(&mut out, sh_off);
    out[shoff_pos..shoff_pos + 8].copy_from_slice(&sh_off.to_le_bytes());

    shdr(&mut out, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
    shdr(&mut out, name_offs[1], 1, 6, l.text, text_off, img.text.len() as u64, 0, 0, 16, 0);
    shdr(&mut out, name_offs[2], 1, 2, l.rodata, ro_off, img.rodata.len() as u64, 0, 0, 16, 0);
    shdr(&mut out, name_offs[3], 1, 3, l.data, data_off, data_filesz, 0, 0, 16, 0);
    shdr(&mut out, name_offs[4], 8, 3, l.data + align(data_filesz, 16), data_off + data_filesz, img.bss, 0, 0, 16, 0);
    shdr(&mut out, name_offs[5], 2, 0, 0, symtab_off, symtab.len() as u64, 6, 1, 8, 24);
    shdr(&mut out, name_offs[6], 3, 0, 0, strtab_off, strtab.len() as u64, 0, 0, 1, 0);
    shdr(&mut out, name_offs[7], 3, 0, 0, shstr_off, shstrtab.len() as u64, 0, 0, 1, 0);
    out
}
