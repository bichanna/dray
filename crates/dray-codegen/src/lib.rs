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

/// `draybase.h` — the hand-written C that is not worth generating.
pub const DRAYBASE_H: &str = include_str!("../../../lib/system/draybase.h");

/// The runtime's definitions, to be compiled exactly once per program.
pub const DRAYBASE_C: &str = include_str!("../../../lib/system/draybase.c");

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
