// SPDX-License-Identifier: Apache-2.0

//! Tokens and source spans for the Dray lexer.

use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Span {
        debug_assert!(start <= end, "span start must not exceed end");
        Span { start, end }
    }

    pub fn len(self) -> u32 {
        self.end - self.start
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub fn text(self, src: &str) -> &str {
        &src[self.start as usize..self.end as usize]
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Token {
        Token { kind, span }
    }

    pub fn is_trivia(self) -> bool {
        matches!(
            self.kind,
            TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenKind {
    // ── literals ────────────────────────────────────────────────────────────
    /// A decimal integer literal, e.g. `42`. (grammar `decimal_lit`)
    IntLit,
    /// A decimal float literal, e.g. `3.14`. (grammar `float_lit`)
    FloatLit,
    /// A double-quoted string literal, e.g. `"hi"`. Span includes both quotes.
    StringLit,
    /// A single-quoted rune literal, e.g. `'a'`. Span includes both quotes.
    RuneLit,

    // ── identifiers & keywords ──────────────────────────────────────────────
    Ident,
    KwProc,
    KwStruct,
    KwEnum,
    KwSwitch,
    KwCase,
    KwDefault,
    KwIf,
    KwElse,
    KwFor,
    KwIn,
    KwReturn,
    KwAlloc,
    KwTryAlloc,
    KwCast,
    KwPub,
    KwImport,
    KwExtern,
    KwCHeader,
    KwBreak,
    KwContinue,
    KwComptime,
    KwTrue,
    KwFalse,

    // ── the six binding operators (grammar lexer note; maximal munch) ────────
    ColonColonEq, // ::=
    ColonColon,   // ::
    ColonEq,      // :=
    Colon,        // :
    Eq,           // =

    // ── compound assignment (grammar AssignOp) ──────────────────────────────
    PlusEq,    // +=
    MinusEq,   // -=
    StarEq,    // *=
    SlashEq,   // /=
    PercentEq, // %=
    AmpEq,     // &=
    PipeEq,    // |=
    CaretEq,   // ^=
    ShlEq,     // <<=
    ShrEq,     // >>=

    // ── arithmetic / bitwise (grammar precedence chain) ─────────────────────
    Plus,    // +
    Minus,   // -
    Star,    // *   (also: multiplication, deref, pointer type, receiver ptr)
    Slash,   // /
    Percent, // %
    Amp,     // &   (bitwise-and, address-of)
    Pipe,    // |
    Caret,   // ^
    Tilde,   // ~
    Shl,     // <<
    Shr,     // >>

    // ── comparison / logical ────────────────────────────────────────────────
    EqEq,     // ==
    BangEq,   // !=
    Lt,       // <
    LtEq,     // <=
    Gt,       // >
    GtEq,     // >=
    AmpAmp,   // &&
    PipePipe, // ||
    Bang,     // !

    // ── other punctuation ───────────────────────────────────────────────────
    At,        // @   (RC pointer type)
    Arrow,     // ->  (return types)
    DotDotDot, // ... (variadic type / spread argument)
    Dot,       // .   (selector; note: NOT part of any `..` operator — Dray has none)
    Comma,     // ,
    Semi,      // ;
    LParen,    // (
    RParen,    // )
    LBrace,    // {
    RBrace,    // }
    LBracket,  // [
    RBracket,  // ]

    // ── trivia (retained for the lossless CST, arch §5) ─────────────────────
    Whitespace,
    LineComment,  // // ... to end of line
    BlockComment, // /* ... */  (non-nesting, per grammar `block_comment`)

    // ── end / error ─────────────────────────────────────────────────────────
    Eof,
    Error(LexError),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LexError {
    /// A string literal reached end-of-file or a newline before its closing `"`.
    UnterminatedString,
    /// A rune literal reached end-of-file or a newline before its closing `'`.
    UnterminatedRune,
    /// A block comment reached end-of-file before its closing `*/`.
    UnterminatedBlockComment,
    /// A `\` escape that isn't one of the sequences in spec §3.4.
    InvalidEscape,
    /// A `\xHH` escape without exactly two hex digits.
    BadHexEscape,
    /// A `\u{...}` escape that is empty, unbraced, over 6 digits, or non-hex.
    BadUnicodeEscape,
    /// A rune literal that isn't exactly one character/escape (`''` or `'ab'`).
    BadRuneLength,
    /// A character that can't begin any token (e.g. `` ` `` or `$`).
    UnexpectedChar,
    /// A `.` followed by a digit with no leading integer (`.5`) — not a valid
    /// `float_lit`, which requires `decimal_lit "." decimal_lit` (grammar §1).
    /// Lexed as `Dot` then handled by the parser; recorded here only when the
    /// lexer is certain it's malformed (a trailing `123.` with no fraction).
    MissingFraction,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            LexError::UnterminatedString => "unterminated string literal",
            LexError::UnterminatedRune => "unterminated rune literal",
            LexError::UnterminatedBlockComment => "unterminated block comment",
            LexError::InvalidEscape => "invalid escape sequence",
            LexError::BadHexEscape => "\\x escape needs exactly two hex digits",
            LexError::BadUnicodeEscape => "malformed \\u{...} escape",
            LexError::BadRuneLength => "rune literal must contain exactly one character",
            LexError::UnexpectedChar => "unexpected character",
            LexError::MissingFraction => "float literal needs digits after the '.'",
        };
        f.write_str(msg)
    }
}

pub(crate) fn keyword_kind(text: &str) -> Option<TokenKind> {
    Some(match text {
        "proc" => TokenKind::KwProc,
        "struct" => TokenKind::KwStruct,
        "enum" => TokenKind::KwEnum,
        "switch" => TokenKind::KwSwitch,
        "case" => TokenKind::KwCase,
        "default" => TokenKind::KwDefault,
        "if" => TokenKind::KwIf,
        "else" => TokenKind::KwElse,
        "for" => TokenKind::KwFor,
        "in" => TokenKind::KwIn,
        "return" => TokenKind::KwReturn,
        "alloc" => TokenKind::KwAlloc,
        "try_alloc" => TokenKind::KwTryAlloc,
        "cast" => TokenKind::KwCast,
        "pub" => TokenKind::KwPub,
        "import" => TokenKind::KwImport,
        "extern" => TokenKind::KwExtern,
        "c_header" => TokenKind::KwCHeader,
        "break" => TokenKind::KwBreak,
        "continue" => TokenKind::KwContinue,
        "comptime" => TokenKind::KwComptime,
        "true" => TokenKind::KwTrue,
        "false" => TokenKind::KwFalse,
        _ => return None,
    })
}
