//! In-process loader for programs built with the built-in linker (`flash build --linker builtin`).
//!
//! The executable is `[stub][ELF relocatable object][len: u64 LE][b"FLASHPAY"]`. This module finds the
//! object at the end of the running executable, lays its sections out in an anonymous memory
//! mapping, resolves undefined symbols against [`crate::symbols`], applies the relocations and calls
//! `flash_main`. It handles the x86-64 ELF relocations that Cranelift emits for position-independent
//! code; nothing is interpreted - the program is ordinary native machine code.

use object::read::{Object, ObjectSection, ObjectSymbol};
use object::{RelocationFlags, RelocationTarget, SectionFlags, SectionIndex, SymbolIndex, SymbolSection};
use std::collections::HashMap;

pub const MAGIC: &[u8; 8] = b"FLASHPAY";

fn fatal(msg: &str) -> ! {
    crate::flush_out();
    eprintln!("Error: {}", msg);
    std::process::exit(1);
}

/// The compiled object appended to `exe`, if any.
pub fn payload_of(exe: &[u8]) -> Option<&[u8]> {
    if exe.len() < 16 || &exe[exe.len() - 8..] != MAGIC {
        return None;
    }
    let len = u64::from_le_bytes(exe[exe.len() - 16..exe.len() - 8].try_into().ok()?) as usize;
    let end = exe.len() - 16;
    end.checked_sub(len).map(|start| &exe[start..end])
}

pub fn run_payload() {
    let exe_path = std::env::current_exe().unwrap_or_else(|e| fatal(&format!("cannot locate the running executable: {}", e)));
    let exe = std::fs::read(&exe_path).unwrap_or_else(|e| fatal(&format!("cannot read {}: {}", exe_path.display(), e)));
    let Some(obj) = payload_of(&exe) else {
        // Not a built program: the bare stub. Offer a diagnostic used by the compiler's tests.
        let args: Vec<String> = std::env::args().collect();
        if args.get(1).map(|s| s.as_str()) == Some("--check-symbols") {
            let missing: Vec<&String> = args[2..].iter().filter(|n| crate::symbols::lookup(n).is_none()).collect();
            if missing.is_empty() {
                println!("all {} symbols resolve", args.len() - 2);
                return;
            }
            fatal(&format!("unresolved: {:?}", missing));
        }
        fatal("this is the Flash stub; it only runs programs built by `flash build`");
    };
    let entry = load(obj).unwrap_or_else(|e| fatal(&format!("cannot load the program: {}", e)));
    // SAFETY: `entry` is the address of the compiled `flash_main`, an `extern "C" fn()`.
    unsafe {
        let f: extern "C" fn() = std::mem::transmute(entry);
        f();
    }
}

const SHF_ALLOC: u64 = 0x2;
const SHF_EXECINSTR: u64 = 0x4;

fn sh_flags(f: SectionFlags) -> u64 {
    match f {
        SectionFlags::Elf { sh_flags, .. } => sh_flags.0 as u64,
        _ => 0,
    }
}

fn align_up(v: usize, a: usize) -> usize {
    let a = a.max(1);
    v.div_ceil(a) * a
}

/// Load the object and return the address of `flash_main`.
pub fn load(data: &[u8]) -> Result<usize, String> {
    let file = object::File::parse(data).map_err(|e| format!("invalid object: {}", e))?;
    if file.architecture() != object::Architecture::X86_64 || file.format() != object::BinaryFormat::Elf {
        return Err("the built-in linker only supports x86-64 ELF objects".into());
    }
    // Which sections are loaded, and where (offset from the image start).
    struct Placed {
        index: SectionIndex,
        offset: usize,
        exec: bool,
    }
    let mut exec: Vec<(SectionIndex, usize, usize)> = vec![];
    let mut other: Vec<(SectionIndex, usize, usize)> = vec![];
    for s in file.sections() {
        let fl = sh_flags(s.flags());
        if fl & SHF_ALLOC == 0 || s.name().unwrap_or("") == ".eh_frame" || s.size() == 0 {
            continue;
        }
        let item = (s.index(), s.size() as usize, s.align() as usize);
        if fl & SHF_EXECINSTR != 0 {
            exec.push(item);
        } else {
            other.push(item);
        }
    }
    // Undefined symbols get a 16-byte veneer (for 32-bit calls that cannot reach the runtime) and a GOT slot.
    let nsyms = file.symbols().map(|s| s.index().0).max().map_or(0, |m| m + 1);
    let mut placed: Vec<Placed> = vec![];
    let mut off = 0usize;
    for (idx, size, align) in &exec {
        off = align_up(off, *align);
        placed.push(Placed { index: *idx, offset: off, exec: true });
        off += size;
    }
    let veneer_base = align_up(off, 16);
    let exec_end = veneer_base + nsyms * 16;
    let data_start = align_up(exec_end, 4096);
    let mut off = data_start;
    for (idx, size, align) in &other {
        off = align_up(off, *align);
        placed.push(Placed { index: *idx, offset: off, exec: false });
        off += size;
    }
    let got_base = align_up(off, 8);
    let total = align_up(got_base + nsyms * 8, 4096);

    // SAFETY: plain anonymous mapping; checked for failure.
    let base =
        unsafe { libc::mmap(std::ptr::null_mut(), total, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANON, -1, 0) };
    if base == libc::MAP_FAILED {
        return Err("cannot allocate memory for the program".into());
    }
    let base = base as usize;
    let mut section_addr: HashMap<SectionIndex, usize> = HashMap::new();
    for p in &placed {
        let s = file.section_by_index(p.index).map_err(|e| e.to_string())?;
        let bytes = s.data().map_err(|e| e.to_string())?;
        let dst = base + p.offset;
        // SAFETY: dst..dst+len lies inside the mapping (sized from the same section list).
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len()) };
        section_addr.insert(p.index, dst);
        let _ = p.exec;
    }

    // Symbol addresses.
    let mut sym_addr: HashMap<SymbolIndex, usize> = HashMap::new();
    let mut veneers: HashMap<SymbolIndex, usize> = HashMap::new();
    let mut flash_main = None;
    let mut unresolved: Vec<String> = vec![];
    for sym in file.symbols() {
        let name = sym.name().unwrap_or("");
        let addr = match sym.section() {
            SymbolSection::Section(i) => section_addr.get(&i).map(|b| b + sym.address() as usize),
            SymbolSection::Undefined => match crate::symbols::lookup(name) {
                Some(a) => Some(a),
                None => {
                    if !name.is_empty() {
                        unresolved.push(name.to_string());
                    }
                    None
                }
            },
            _ => None,
        };
        if let Some(a) = addr {
            sym_addr.insert(sym.index(), a);
            if name == "flash_main" && sym.is_global() {
                flash_main = Some(a);
            }
            if sym.is_undefined() {
                let v = base + veneer_base + sym.index().0 * 16;
                // jmp *0(%rip) ; .quad target
                let code: [u8; 6] = [0xff, 0x25, 0x02, 0, 0, 0];
                // SAFETY: inside the veneer area reserved above.
                unsafe {
                    std::ptr::copy_nonoverlapping(code.as_ptr(), v as *mut u8, 6);
                    std::ptr::copy_nonoverlapping((a as u64).to_le_bytes().as_ptr(), (v + 8) as *mut u8, 8);
                }
                veneers.insert(sym.index(), v);
            }
        }
    }
    if !unresolved.is_empty() {
        return Err(format!(
            "the program uses symbols that the built-in linker does not provide: {} (build with `--linker system` and pass the library with --link)",
            unresolved.join(", ")
        ));
    }
    let mut got: HashMap<SymbolIndex, usize> = HashMap::new();
    let mut got_slot = |sym: SymbolIndex, addr: usize| -> usize {
        *got.entry(sym).or_insert_with(|| {
            let slot = base + got_base + sym.0 * 8;
            // SAFETY: slot lies in the GOT area reserved above.
            unsafe { *(slot as *mut u64) = addr as u64 };
            slot
        })
    };

    // Relocations.
    for p in &placed {
        let s = file.section_by_index(p.index).map_err(|e| e.to_string())?;
        let sbase = section_addr[&p.index];
        for (roff, r) in s.relocations() {
            let RelocationFlags::Elf { r_type } = r.flags() else { return Err("unexpected relocation format".into()) };
            let place = sbase + roff as usize;
            let (target, sym_index) = match r.target() {
                RelocationTarget::Symbol(i) => {
                    (*sym_addr.get(&i).ok_or_else(|| format!("relocation against unresolved symbol {}", i.0))?, Some(i))
                }
                RelocationTarget::Section(i) => (*section_addr.get(&i).ok_or("relocation against an unloaded section")?, None),
                _ => return Err("unsupported relocation target".into()),
            };
            let a = r.addend();
            // SAFETY: `place` is inside a loaded section; writes stay within the relocated field.
            unsafe {
                match r_type.0 {
                    1 => (place as *mut u64).write_unaligned((target as i64 + a) as u64), // R_X86_64_64
                    2 | 4 => {
                        // PC32 / PLT32: calls to runtime functions go through a veneer (they are far away)
                        let s_addr = match sym_index.and_then(|i| veneers.get(&i)) {
                            Some(v) => *v,
                            None => target,
                        };
                        let v = s_addr as i64 + a - place as i64;
                        let v32 = i32::try_from(v).map_err(|_| "relocation out of range".to_string())?;
                        (place as *mut i32).write_unaligned(v32);
                    }
                    9 | 41 | 42 => {
                        // GOTPCREL(X): load the symbol's address from a GOT slot
                        let i = sym_index.ok_or("GOT relocation against a section")?;
                        let slot = got_slot(i, target);
                        let v = slot as i64 + a - place as i64;
                        (place as *mut i32).write_unaligned(i32::try_from(v).map_err(|_| "GOT relocation out of range".to_string())?);
                    }
                    other => return Err(format!("unsupported x86-64 relocation type {}", other)),
                }
            }
        }
    }
    // Code is read+execute from here on.
    // SAFETY: the range was mapped above and is page aligned.
    let rc = unsafe { libc::mprotect(base as *mut libc::c_void, data_start, libc::PROT_READ | libc::PROT_EXEC) };
    if rc != 0 {
        return Err("cannot make the program executable (is the system blocking executable memory?)".into());
    }
    flash_main.ok_or_else(|| "the object has no flash_main".to_string())
}
