//! Linking a user program with the runtime library (itself written in Flash).
//!
//! The runtime is compiled through the ordinary front end and lowered to IR; its `fl_*` functions
//! replace the calls the user program makes to runtime "externs", and unreachable functions are
//! removed so that a program contains only the runtime code it can actually reach.

use crate::ir::*;

/// The runtime library source: several files compiled as one module.
pub fn runtime_source() -> String {
    [
        include_str!("../../runtime/sys.flash"),
        include_str!("../../runtime/text.flash"),
        include_str!("../../runtime/float.flash"),
        include_str!("../../runtime/math.flash"),
        include_str!("../../runtime/io.flash"),
    ]
    .join("\n")
}

/// Compile the runtime library to IR.
pub fn lower_runtime() -> Result<Module, String> {
    let fe = crate::driver::frontend_runtime("<flash runtime>", &runtime_source())?;
    Ok(crate::lower::lower_runtime(&fe.program))
}

fn shift_function(f: &mut Function, fbase: u32, dbase: u32) {
    for b in &mut f.blocks {
        for inst in &mut b.insts {
            match inst {
                Inst::Call { callee: Callee::Func(c), .. } => *c += fbase,
                Inst::DataAddr { data, .. } => *data += dbase,
                _ => {}
            }
        }
    }
}

/// Merge `rt` into `user`, resolve every extern call by name, and keep only what is reachable
/// from the process entry point.
pub fn link(mut user: Module, mut rt: Module, release: bool) -> Result<Module, String> {
    let fbase = user.funcs.len() as u32;
    let dbase = user.data.len() as u32;
    for f in &mut rt.funcs {
        shift_function(f, fbase, dbase);
    }
    // Extern tables are merged: both sides may declare externs (the runtime declares `flash_main`).
    let user_externs = std::mem::take(&mut user.externs);
    let rt_externs = std::mem::take(&mut rt.externs);
    user.funcs.append(&mut rt.funcs);
    user.data.append(&mut rt.data);

    let mut by_name: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for (i, f) in user.funcs.iter().enumerate() {
        // User functions are `name_N`; only runtime functions and `flash_main` are visible by name.
        if (i as u32) >= fbase || f.name == "flash_main" {
            by_name.insert(f.name.clone(), i as u32);
        }
    }
    let resolve = |decl: &ExternDecl, funcs: &[Function]| -> Result<u32, String> {
        let idx = *by_name.get(&decl.name).ok_or_else(|| {
            format!(
                "unresolved function `{}`: the native back end links only the Flash runtime library, not C libraries (`extern fn`/`--link` need --backend cranelift or llvm)",
                decl.name
            )
        })?;
        let f = &funcs[idx as usize];
        if f.params != decl.params || f.ret != decl.ret {
            return Err(format!(
                "internal error: runtime function `{}` has signature {:?} -> {:?} but the compiler calls it as {:?} -> {:?}",
                decl.name, f.params, f.ret, decl.params, decl.ret
            ));
        }
        Ok(idx)
    };
    let user_map: Vec<u32> = user_externs.iter().map(|d| resolve(d, &user.funcs)).collect::<Result<_, _>>()?;
    let rt_map: Vec<u32> = rt_externs.iter().map(|d| resolve(d, &user.funcs)).collect::<Result<_, _>>()?;
    for (i, f) in user.funcs.iter_mut().enumerate() {
        let map = if (i as u32) < fbase { &user_map } else { &rt_map };
        for b in &mut f.blocks {
            for inst in &mut b.insts {
                if let Inst::Call { callee, .. } = inst {
                    if let Callee::Extern(e) = callee {
                        *callee = Callee::Func(map[*e as usize]);
                    }
                }
            }
        }
    }
    let entry = *by_name.get(super::ENTRY_FN).ok_or("internal error: the runtime has no entry function")?;
    for f in &mut user.funcs {
        f.exported = false;
    }
    user.funcs[entry as usize].exported = true;
    // Drop everything the program cannot reach before spending time optimising it.
    crate::opt::remove_unused_functions(&mut user);
    crate::opt::optimise(&mut user, release && std::env::var_os("FLASH_DEBUG_NOINLINE").is_none());
    if let Some(name) = std::env::var_os("FLASH_DEBUG_DUMP") {
        for f in user.funcs.iter().filter(|f| f.name == name.to_string_lossy()) {
            eprintln!("{}", Module { funcs: vec![f.clone()], ..Default::default() });
        }
    }
    Ok(user)
}
