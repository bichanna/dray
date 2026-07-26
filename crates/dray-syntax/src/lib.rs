// SPDX-License-Identifier: Apache-2.0

//! `dray-syntax` — lexer, CST, and parser for the Dray language.
//!
//! This crate is the foundation every other crate views the source through:
//! it must not depend on `tamago` or any codegen concern.
//! It provides the lexer, the green/red concrete syntax tree, and a
//! recursive-descent + Pratt parser

pub mod cst;
pub mod debug;
pub mod lexer;
pub mod parser;
pub mod token;

pub use cst::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, debug_tree};
pub use debug::{
    DumpOptions, dump_cst, dump_cst_with, dump_tokens, dump_tokens_no_trivia, kind_name,
    token_kind_name,
};
pub use lexer::{Lexer, tokenize};
pub use parser::{Parse, ParseError, parse};
pub use token::{LexError, Span, Token, TokenKind};

/// The string paths of every top-level `import(...)` in a parsed file, in order.
/// Used by the driver to resolve modules before lowering
pub fn import_paths(root: &SyntaxNode) -> Vec<String> {
    root.children()
        .into_iter()
        .filter(|d| d.kind() == SyntaxKind::ImportDecl)
        .filter_map(|d| {
            d.token_of_kind(SyntaxKind::StringLit)
                .map(|t| unquote_str(t.text()))
        })
        .collect()
}

fn unquote_str(s: &str) -> String {
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s)
        .to_string()
}
