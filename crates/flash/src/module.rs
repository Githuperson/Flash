//! A parsed source file together with its resolved imports.

use crate::ast;

#[derive(Debug)]
pub struct LoadedModule {
    /// Name other modules use to refer to this module (`import name`).
    pub alias: String,
    pub ast: ast::Module,
    /// (alias used in this module, index into the module list)
    pub imports: Vec<(String, usize)>,
    /// (alias used in this module, index into the plugin list)
    pub plugins: Vec<(String, usize)>,
    /// The runtime library: may use the `__` intrinsics (raw memory access, system calls).
    pub privileged: bool,
}
