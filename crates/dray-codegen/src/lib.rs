// SPDX-License-Identifier: Apache-2.0

//! Direct CST → C lowering for the Dray walking skeleton.

use dray_syntax::{SyntaxKind, SyntaxNode, parse};

mod expr;
mod lower;
mod stmt;
mod ty;

pub use lower::lower_source_file;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LowerError {
    pub message: String,
}

impl LowerError {
    pub(crate) fn new(message: impl Into<String>) -> LowerError {
        LowerError {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "codegen: {}", self.message)
    }
}

impl std::error::Error for LowerError {}

pub type Result<T> = std::result::Result<T, LowerError>;

pub fn compile_to_c(src: &str) -> Result<String> {
    let parsed = parse(src);
    if !parsed.errors.is_empty() {
        let first = &parsed.errors[0];
        return Err(LowerError::new(format!(
            "cannot generate C from source with parse errors ({} total); first: {}..{}: {}",
            parsed.errors.len(),
            first.span.start,
            first.span.end,
            first.message
        )));
    }
    let scope = lower_source_file(&parsed.root)?;
    Ok(format!("{scope}"))
}

// ── small shared CST helpers used across the lowering submodules ─────────────

/// The first child node of `kind`, or a `LowerError` naming what was missing.
pub(crate) fn require_child(node: &SyntaxNode, kind: SyntaxKind, what: &str) -> Result<SyntaxNode> {
    node.child_of_kind(kind)
        .ok_or_else(|| LowerError::new(format!("missing {what}")))
}

/// The trimmed source text of a node (used to read identifiers, literals).
pub(crate) fn node_text(node: &SyntaxNode) -> String {
    node.text().trim().to_string()
}

/// The text of the first `Ident` token directly under `node`, if any
pub(crate) fn first_ident(node: &SyntaxNode) -> Option<String> {
    node.token_of_kind(SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}
