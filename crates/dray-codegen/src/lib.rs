// SPDX-License-Identifier: Apache-2.0

//! IR → C lowering, via Tamago.

use dray_ir::Ir;

mod lower;

pub use lower::lower_ir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodegenError {
    pub message: String,
}

impl CodegenError {
    pub(crate) fn new(message: impl Into<String>) -> CodegenError {
        CodegenError {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "codegen: {}", self.message)
    }
}

impl std::error::Error for CodegenError {}

pub type Result<T> = std::result::Result<T, CodegenError>;

/// `draybase.h` — the hand-written C
/// The analogue of Nim's `nimbase.h`: fixed-width integer typedefs, the RC header
/// layout, and the portability macros whose spelling differs between compilers.
pub const DRAYBASE_H: &str = include_str!("../runtime/draybase.h");

/// The runtime's definitions, to be compiled exactly once per program.
pub const DRAYBASE_C: &str = include_str!("../runtime/draybase.c");

/// Lower a whole IR module to C source.
pub fn ir_to_c(ir: &Ir) -> Result<String> {
    let scope = lower_ir(ir)?;
    Ok(tamago::render(
        &scope,
        tamago::RenderOptions {
            line_directives: ir.source.is_some(),
            ..Default::default()
        },
    ))
}
