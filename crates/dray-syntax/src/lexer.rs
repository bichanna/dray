// SPDX-License-Identifier: Apache-2.0

//! Hand-written lexer for Dray

use crate::token::{LexError, Span, Token, TokenKind, keyword_kind};

pub fn tokenize(src: &str) -> Vec<Token> {
    let mut lexer = Lexer::new(src);
    let mut out = Vec::new();
    loop {
        let tok = lexer.next_token();
        let is_eof = tok.kind == TokenKind::Eof;
        out.push(tok);
        if is_eof {
            break;
        }
    }
    out
}

pub struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Lexer<'a> {
        Lexer {
            src,
            bytes: src.as_bytes(),
            pos: 0,
        }
    }

    // ── low-level cursor helpers ────────────────────────────────────────────

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<u8> {
        self.bytes.get(self.pos + 1).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek();
        if b.is_some() {
            self.pos += 1;
        }
        b
    }

    fn eat_while(&mut self, mut pred: impl FnMut(u8) -> bool) {
        while let Some(b) = self.peek() {
            if pred(b) {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn span_from(&self, start: usize) -> Span {
        Span::new(start as u32, self.pos as u32)
    }

    fn tok(&self, kind: TokenKind, start: usize) -> Token {
        Token::new(kind, self.span_from(start))
    }

    // ── the main dispatch ───────────────────────────────────────────────────

    pub fn next_token(&mut self) -> Token {
        let start = self.pos;
        let Some(b) = self.peek() else {
            return self.tok(TokenKind::Eof, start);
        };

        match b {
            b' ' | b'\t' | b'\r' | b'\n' => self.whitespace(start),

            b'/' => match self.peek2() {
                Some(b'/') => self.line_comment(start),
                Some(b'*') => self.block_comment(start),
                Some(b'=') => {
                    self.pos += 2;
                    self.tok(TokenKind::SlashEq, start)
                }
                _ => {
                    self.pos += 1;
                    self.tok(TokenKind::Slash, start)
                }
            },

            b'"' => self.string(start),
            b'\'' => self.rune(start),

            b if is_ident_start(b) => self.ident_or_keyword(start),
            b if b.is_ascii_digit() => self.number(start),

            b':' => self.colons(start),

            _ => self.punct(start),
        }
    }

    // ── trivia ──────────────────────────────────────────────────────────────

    fn whitespace(&mut self, start: usize) -> Token {
        self.eat_while(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'));
        self.tok(TokenKind::Whitespace, start)
    }

    fn line_comment(&mut self, start: usize) -> Token {
        // consume the `//`
        self.pos += 2;
        self.eat_while(|b| b != b'\n');
        self.tok(TokenKind::LineComment, start)
    }

    fn block_comment(&mut self, start: usize) -> Token {
        // consume the `/*`
        self.pos += 2;
        // Non-nesting per grammar `block_comment` = "/*" { any } "*/": the first
        // `*/` closes it, regardless of intervening `/*`.
        loop {
            match self.peek() {
                None => {
                    return self.tok(TokenKind::Error(LexError::UnterminatedBlockComment), start);
                }
                Some(b'*') if self.peek2() == Some(b'/') => {
                    self.pos += 2;
                    return self.tok(TokenKind::BlockComment, start);
                }
                _ => {
                    self.pos += 1;
                }
            }
        }
    }

    // ── identifiers & keywords ──────────────────────────────────────────────

    fn ident_or_keyword(&mut self, start: usize) -> Token {
        self.eat_while(is_ident_continue);
        let text = &self.src[start..self.pos];
        let kind = keyword_kind(text).unwrap_or(TokenKind::Ident);
        self.tok(kind, start)
    }

    // ── numbers (grammar: decimal_lit / float_lit only) ─────────────────────

    fn number(&mut self, start: usize) -> Token {
        self.eat_while(|b| b.is_ascii_digit());

        if self.peek() == Some(b'.') && self.peek2().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1; // the '.'
            self.eat_while(|b| b.is_ascii_digit());
            return self.tok(TokenKind::FloatLit, start);
        }

        self.tok(TokenKind::IntLit, start)
    }

    // ── strings & runes ─────────────────────────────────────────────────────

    fn string(&mut self, start: usize) -> Token {
        self.pos += 1; // opening quote
        loop {
            match self.peek() {
                None | Some(b'\n') => {
                    return self.tok(TokenKind::Error(LexError::UnterminatedString), start);
                }
                Some(b'"') => {
                    self.pos += 1; // closing quote
                    return self.tok(TokenKind::StringLit, start);
                }
                Some(b'\\') => {
                    if let Err(e) = self.eat_escape() {
                        self.recover_to_string_end();
                        return self.tok(TokenKind::Error(e), start);
                    }
                }
                Some(_) => {
                    self.pos += 1;
                }
            }
        }
    }

    fn rune(&mut self, start: usize) -> Token {
        self.pos += 1; // opening quote
        let mut count = 0usize;
        loop {
            match self.peek() {
                None | Some(b'\n') => {
                    return self.tok(TokenKind::Error(LexError::UnterminatedRune), start);
                }
                Some(b'\'') => {
                    self.pos += 1; // closing quote
                    let kind = if count == 1 {
                        TokenKind::RuneLit
                    } else {
                        TokenKind::Error(LexError::BadRuneLength)
                    };
                    return self.tok(kind, start);
                }
                Some(b'\\') => {
                    if let Err(e) = self.eat_escape() {
                        self.recover_to_rune_end();
                        return self.tok(TokenKind::Error(e), start);
                    }
                    count += 1;
                }
                Some(b) => {
                    self.pos += utf8_len(b);
                    count += 1;
                }
            }
        }
    }

    /// Consume one escape sequence starting at the `\`. On success the cursor is
    /// just past the escape; on error it's left at the offending position.
    fn eat_escape(&mut self) -> Result<(), LexError> {
        debug_assert_eq!(self.peek(), Some(b'\\'));
        self.pos += 1; // the backslash
        match self.bump() {
            Some(b'n' | b't' | b'r' | b'\\' | b'"' | b'\'' | b'0') => Ok(()),
            Some(b'x') => self.eat_hex_escape(),
            Some(b'u') => self.eat_unicode_escape(),
            _ => Err(LexError::InvalidEscape),
        }
    }

    /// `\xHH` — exactly two hex digits (spec §3.4).
    fn eat_hex_escape(&mut self) -> Result<(), LexError> {
        for _ in 0..2 {
            match self.peek() {
                Some(b) if b.is_ascii_hexdigit() => self.pos += 1,
                _ => return Err(LexError::BadHexEscape),
            }
        }
        Ok(())
    }

    /// `\u{HHHHHH}` — braced, 1–6 hex digits (spec §3.4).
    fn eat_unicode_escape(&mut self) -> Result<(), LexError> {
        if self.peek() != Some(b'{') {
            return Err(LexError::BadUnicodeEscape);
        }
        self.pos += 1; // '{'
        let mut digits = 0;
        while let Some(b) = self.peek() {
            if b.is_ascii_hexdigit() {
                digits += 1;
                self.pos += 1;
                if digits > 6 {
                    return Err(LexError::BadUnicodeEscape);
                }
            } else {
                break;
            }
        }
        if digits == 0 || self.peek() != Some(b'}') {
            return Err(LexError::BadUnicodeEscape);
        }
        self.pos += 1; // '}'
        Ok(())
    }

    fn recover_to_string_end(&mut self) {
        while let Some(b) = self.peek() {
            match b {
                b'\n' => break,
                b'"' => {
                    self.pos += 1;
                    break;
                }
                _ => self.pos += 1,
            }
        }
    }

    fn recover_to_rune_end(&mut self) {
        while let Some(b) = self.peek() {
            match b {
                b'\n' => break,
                b'\'' => {
                    self.pos += 1;
                    break;
                }
                _ => self.pos += 1,
            }
        }
    }

    // ── the colon family (grammar lexer note: maximal munch) ────────────────

    fn colons(&mut self, start: usize) -> Token {
        debug_assert_eq!(self.peek(), Some(b':'));
        self.pos += 1;
        if self.peek() == Some(b':') {
            self.pos += 1;
            if self.peek() == Some(b'=') {
                self.pos += 1;
                self.tok(TokenKind::ColonColonEq, start) // ::=
            } else {
                self.tok(TokenKind::ColonColon, start) // ::
            }
        } else if self.peek() == Some(b'=') {
            self.pos += 1;
            self.tok(TokenKind::ColonEq, start) // :=
        } else {
            self.tok(TokenKind::Colon, start) // :
        }
    }

    // ── all remaining punctuation & operators ───────────────────────────────

    fn punct(&mut self, start: usize) -> Token {
        let b = self.peek().expect("punct called at EOF");
        let b2 = self.peek2();

        macro_rules! one {
            ($k:expr) => {{
                self.pos += 1;
                return self.tok($k, start);
            }};
        }
        macro_rules! two {
            ($k:expr) => {{
                self.pos += 2;
                return self.tok($k, start);
            }};
        }

        match (b, b2) {
            // three-char: `...`
            (b'.', Some(b'.')) if self.bytes.get(self.pos + 2) == Some(&b'.') => {
                self.pos += 3;
                self.tok(TokenKind::DotDotDot, start)
            }

            // two-char operators
            (b'-', Some(b'>')) => two!(TokenKind::Arrow),
            (b'=', Some(b'=')) => two!(TokenKind::EqEq),
            (b'!', Some(b'=')) => two!(TokenKind::BangEq),
            (b'<', Some(b'=')) => two!(TokenKind::LtEq),
            (b'>', Some(b'=')) => two!(TokenKind::GtEq),
            (b'<', Some(b'<')) => two!(TokenKind::Shl),
            (b'>', Some(b'>')) => two!(TokenKind::Shr),
            (b'&', Some(b'&')) => two!(TokenKind::AmpAmp),
            (b'|', Some(b'|')) => two!(TokenKind::PipePipe),
            (b'+', Some(b'=')) => two!(TokenKind::PlusEq),
            (b'-', Some(b'=')) => two!(TokenKind::MinusEq),
            (b'*', Some(b'=')) => two!(TokenKind::StarEq),
            (b'%', Some(b'=')) => two!(TokenKind::PercentEq),

            // one-char operators & delimiters
            (b'+', _) => one!(TokenKind::Plus),
            (b'-', _) => one!(TokenKind::Minus),
            (b'*', _) => one!(TokenKind::Star),
            (b'%', _) => one!(TokenKind::Percent),
            (b'&', _) => one!(TokenKind::Amp),
            (b'|', _) => one!(TokenKind::Pipe),
            (b'^', _) => one!(TokenKind::Caret),
            (b'~', _) => one!(TokenKind::Tilde),
            (b'!', _) => one!(TokenKind::Bang),
            (b'<', _) => one!(TokenKind::Lt),
            (b'>', _) => one!(TokenKind::Gt),
            (b'=', _) => one!(TokenKind::Eq),
            (b'@', _) => one!(TokenKind::At),
            (b'.', _) => one!(TokenKind::Dot),
            (b',', _) => one!(TokenKind::Comma),
            (b';', _) => one!(TokenKind::Semi),
            (b'(', _) => one!(TokenKind::LParen),
            (b')', _) => one!(TokenKind::RParen),
            (b'{', _) => one!(TokenKind::LBrace),
            (b'}', _) => one!(TokenKind::RBrace),
            (b'[', _) => one!(TokenKind::LBracket),
            (b']', _) => one!(TokenKind::RBracket),

            _ => {
                self.pos += utf8_len(b);
                self.tok(TokenKind::Error(LexError::UnexpectedChar), start)
            }
        }
    }
}

// ── character-class helpers (grammar §1: ASCII letters + `_`) ────────────────

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn utf8_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}
