// SPDX-License-Identifier: Apache-2.0

//! HIR → C lowering, via Tamago.

use dray_hir::Hir;

mod lower;

pub use lower::lower_hir;

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

pub fn hir_to_c(hir: &Hir) -> Result<String> {
    let scope = lower_hir(hir)?;
    Ok(format!("{scope}"))
}
