// SPDX-License-Identifier: Apache-2.0

//! `dray-syntax` — lexer, CST, and parser for the Dray language.
//!
//! This crate is the foundation every other crate views the source through:
//! it must not depend on `tamago` or any codegen concern.
//! It provides the lexer, the green/red concrete syntax tree, and a
//! recursive-descent + Pratt parser

pub mod cst;
pub mod lexer;
pub mod parser;
pub mod token;

pub use cst::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, debug_tree};
pub use lexer::{Lexer, tokenize};
pub use parser::{Parse, ParseError, parse};
pub use token::{LexError, Span, Token, TokenKind};
