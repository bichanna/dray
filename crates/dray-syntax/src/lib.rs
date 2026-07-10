// SPDX-License-Identifier: Apache-2.0

//! `dray-syntax` — lexer and CST/parser for the Dray language.
//!
//! This crate is the foundation every other crate views the source.

pub mod lexer;
pub mod token;

pub use lexer::{tokenize, Lexer};
pub use token::{LexError, Span, Token, TokenKind};
