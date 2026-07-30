// SPDX-License-Identifier: Apache-2.0

//! IR → C lowering, via Tamago.

use dray_ir::Ir;

mod lower;

pub use lower::{lower_ir, lower_ir_split};

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

#[derive(Debug, Clone)]
pub struct CModules {
    pub header: String,
    pub modules: Vec<String>,
}

pub fn ir_to_c_modules(ir: &Ir, header_name: &str) -> Result<CModules> {
    let (header_scope, module_scopes) = lower::lower_ir_split(ir, header_name)?;
    let opts = || tamago::RenderOptions {
        line_directives: false,
        ..Default::default()
    };
    Ok(CModules {
        header: tamago::render(&header_scope, opts()),
        modules: module_scopes
            .iter()
            .map(|s| tamago::render(s, opts()))
            .collect(),
    })
}
