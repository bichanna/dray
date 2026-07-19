// SPDX-License-Identifier: Apache-2.0

//! Debug pretty-printing for tokens and the CST.
//!
//! This is the single home for human-readable dumps of the lexer/parser output.
//! It's public so the CLI (`dray dump-tokens` / `dray dump-cst`), and later the
//! formatter and LSP, can share one consistent rendering rather than each hand-
//! rolling its own

use crate::cst::{SyntaxElement, SyntaxKind, SyntaxNode};
use crate::lexer::tokenize;
use crate::token::{Span, TokenKind};
use std::fmt::Write;

// ── token stream dumps ───────────────────────────────────────────────────────

pub fn dump_tokens(src: &str) -> String {
    let toks = tokenize(src);
    // width the span column to the largest end offset for clean alignment
    let max_end = toks.last().map(|t| t.span.end).unwrap_or(0);
    let span_w = format!("{max_end}").len().max(1);

    let mut out = String::new();
    for tok in &toks {
        let span = tok.span;
        let name = token_kind_name(tok.kind);
        let slice = span.text(src);
        let _ = writeln!(
            out,
            "{:>w$}..{:<w$}  {:<14} {}",
            span.start,
            span.end,
            name,
            escape_for_display(slice),
            w = span_w,
        );
    }
    out
}

pub fn dump_tokens_no_trivia(src: &str) -> String {
    let toks = tokenize(src);
    let max_end = toks.last().map(|t| t.span.end).unwrap_or(0);
    let span_w = format!("{max_end}").len().max(1);

    let mut out = String::new();
    for tok in &toks {
        if tok.is_trivia() {
            continue;
        }
        let span = tok.span;
        let _ = writeln!(
            out,
            "{:>w$}..{:<w$}  {:<14} {}",
            span.start,
            span.end,
            token_kind_name(tok.kind),
            escape_for_display(span.text(src)),
            w = span_w,
        );
    }
    out
}

// ── CST dumps ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub struct DumpOptions {
    /// Show whitespace/comment/lex-error leaf tokens. Off by default so the
    /// structural shape is legible; turn on to verify losslessness
    pub show_trivia: bool,
    /// Append `@start..end` byte spans to every line.
    pub show_spans: bool,
    /// Show the source text of token leaves (quoted, escaped).
    pub show_token_text: bool,
    /// Indent width in spaces per depth level.
    pub indent: usize,
}

impl Default for DumpOptions {
    fn default() -> DumpOptions {
        DumpOptions {
            show_trivia: false,
            show_spans: true,
            show_token_text: true,
            indent: 2,
        }
    }
}

impl DumpOptions {
    pub fn structural() -> DumpOptions {
        DumpOptions::default()
    }

    pub fn lossless() -> DumpOptions {
        DumpOptions {
            show_trivia: true,
            ..DumpOptions::default()
        }
    }

    pub fn shape_only() -> DumpOptions {
        DumpOptions {
            show_trivia: false,
            show_spans: false,
            show_token_text: false,
            indent: 2,
        }
    }

    pub fn with_trivia(mut self, yes: bool) -> DumpOptions {
        self.show_trivia = yes;
        self
    }

    pub fn with_spans(mut self, yes: bool) -> DumpOptions {
        self.show_spans = yes;
        self
    }
}

pub fn dump_cst(node: &SyntaxNode) -> String {
    dump_cst_with(node, DumpOptions::default())
}

/// Render a CST subtree with explicit options.
pub fn dump_cst_with(node: &SyntaxNode, opts: DumpOptions) -> String {
    let mut out = String::new();
    fmt_node(node, 0, opts, &mut out);
    out
}

fn fmt_node(node: &SyntaxNode, depth: usize, opts: DumpOptions, buf: &mut String) {
    indent(buf, depth, opts.indent);
    buf.push_str(kind_name(node.kind()));
    if opts.show_spans {
        write_span(buf, node.span());
    }
    buf.push('\n');

    for el in node.children_with_tokens() {
        match el {
            SyntaxElement::Node(n) => fmt_node(&n, depth + 1, opts, buf),
            SyntaxElement::Token(t) => {
                if t.kind().is_trivia() && !opts.show_trivia {
                    continue;
                }
                indent(buf, depth + 1, opts.indent);
                buf.push_str(kind_name(t.kind()));
                if opts.show_spans {
                    write_span(buf, t.span());
                }
                if opts.show_token_text {
                    let _ = write!(buf, " {}", escape_for_display(t.text()));
                }
                buf.push('\n');
            }
        }
    }
}

fn indent(buf: &mut String, depth: usize, width: usize) {
    for _ in 0..depth * width {
        buf.push(' ');
    }
}

fn write_span(buf: &mut String, span: Span) {
    let _ = write!(buf, "@{}..{}", span.start, span.end);
}

// ── name tables ──────────────────────────────────────────────────────────────

pub fn token_kind_name(kind: TokenKind) -> &'static str {
    match kind {
        TokenKind::IntLit => "IntLit",
        TokenKind::FloatLit => "FloatLit",
        TokenKind::StringLit => "StringLit",
        TokenKind::RuneLit => "RuneLit",
        TokenKind::Ident => "Ident",
        TokenKind::KwProc => "proc",
        TokenKind::KwStruct => "struct",
        TokenKind::KwEnum => "enum",
        TokenKind::KwSwitch => "switch",
        TokenKind::KwCase => "case",
        TokenKind::KwDefault => "default",
        TokenKind::KwIf => "if",
        TokenKind::KwElse => "else",
        TokenKind::KwFor => "for",
        TokenKind::KwIn => "in",
        TokenKind::KwReturn => "return",
        TokenKind::KwAlloc => "alloc",
        TokenKind::KwTryAlloc => "try_alloc",
        TokenKind::KwCast => "cast",
        TokenKind::KwPub => "pub",
        TokenKind::KwImport => "import",
        TokenKind::KwExtern => "extern",
        TokenKind::KwCHeader => "c_header",
        TokenKind::KwBreak => "break",
        TokenKind::KwContinue => "continue",
        TokenKind::KwComptime => "comptime",
        TokenKind::KwTrue => "true",
        TokenKind::KwFalse => "false",
        TokenKind::ColonColonEq => "::=",
        TokenKind::ColonColon => "::",
        TokenKind::ColonEq => ":=",
        TokenKind::Colon => ":",
        TokenKind::Eq => "=",
        TokenKind::PlusEq => "+=",
        TokenKind::MinusEq => "-=",
        TokenKind::StarEq => "*=",
        TokenKind::SlashEq => "/=",
        TokenKind::PercentEq => "%=",
        TokenKind::AmpEq => "&=",
        TokenKind::PipeEq => "|=",
        TokenKind::CaretEq => "^=",
        TokenKind::ShlEq => "<<=",
        TokenKind::ShrEq => ">>=",
        TokenKind::Plus => "+",
        TokenKind::Minus => "-",
        TokenKind::Star => "*",
        TokenKind::Slash => "/",
        TokenKind::Percent => "%",
        TokenKind::Amp => "&",
        TokenKind::Pipe => "|",
        TokenKind::Caret => "^",
        TokenKind::Tilde => "~",
        TokenKind::Shl => "<<",
        TokenKind::Shr => ">>",
        TokenKind::EqEq => "==",
        TokenKind::BangEq => "!=",
        TokenKind::Lt => "<",
        TokenKind::LtEq => "<=",
        TokenKind::Gt => ">",
        TokenKind::GtEq => ">=",
        TokenKind::AmpAmp => "&&",
        TokenKind::PipePipe => "||",
        TokenKind::Bang => "!",
        TokenKind::At => "@",
        TokenKind::Arrow => "->",
        TokenKind::DotDotDot => "...",
        TokenKind::Dot => ".",
        TokenKind::Comma => ",",
        TokenKind::Semi => ";",
        TokenKind::LParen => "(",
        TokenKind::RParen => ")",
        TokenKind::LBrace => "{",
        TokenKind::RBrace => "}",
        TokenKind::LBracket => "[",
        TokenKind::RBracket => "]",
        TokenKind::Whitespace => "Whitespace",
        TokenKind::LineComment => "LineComment",
        TokenKind::BlockComment => "BlockComment",
        TokenKind::Eof => "Eof",
        TokenKind::Error(_) => "LexError",
    }
}

pub fn kind_name(kind: SyntaxKind) -> &'static str {
    match kind {
        // token-leaf kinds → symbolic names (mirror token_kind_name)
        SyntaxKind::IntLit => "IntLit",
        SyntaxKind::FloatLit => "FloatLit",
        SyntaxKind::StringLit => "StringLit",
        SyntaxKind::RuneLit => "RuneLit",
        SyntaxKind::Ident => "Ident",
        SyntaxKind::KwProc => "proc",
        SyntaxKind::KwStruct => "struct",
        SyntaxKind::KwEnum => "enum",
        SyntaxKind::KwSwitch => "switch",
        SyntaxKind::KwCase => "case",
        SyntaxKind::KwDefault => "default",
        SyntaxKind::KwIf => "if",
        SyntaxKind::KwElse => "else",
        SyntaxKind::KwFor => "for",
        SyntaxKind::KwIn => "in",
        SyntaxKind::KwReturn => "return",
        SyntaxKind::KwAlloc => "alloc",
        SyntaxKind::KwTryAlloc => "try_alloc",
        SyntaxKind::KwCast => "cast",
        SyntaxKind::KwPub => "pub",
        SyntaxKind::KwImport => "import",
        SyntaxKind::KwExtern => "extern",
        SyntaxKind::KwCHeader => "c_header",
        SyntaxKind::KwBreak => "break",
        SyntaxKind::KwContinue => "continue",
        SyntaxKind::KwComptime => "comptime",
        SyntaxKind::KwTrue => "true",
        SyntaxKind::KwFalse => "false",
        SyntaxKind::ColonColonEq => "::=",
        SyntaxKind::ColonColon => "::",
        SyntaxKind::ColonEq => ":=",
        SyntaxKind::Colon => ":",
        SyntaxKind::Eq => "=",
        SyntaxKind::PlusEq => "+=",
        SyntaxKind::MinusEq => "-=",
        SyntaxKind::StarEq => "*=",
        SyntaxKind::SlashEq => "/=",
        SyntaxKind::PercentEq => "%=",
        SyntaxKind::AmpEq => "&=",
        SyntaxKind::PipeEq => "|=",
        SyntaxKind::CaretEq => "^=",
        SyntaxKind::ShlEq => "<<=",
        SyntaxKind::ShrEq => ">>=",
        SyntaxKind::Plus => "+",
        SyntaxKind::Minus => "-",
        SyntaxKind::Star => "*",
        SyntaxKind::Slash => "/",
        SyntaxKind::Percent => "%",
        SyntaxKind::Amp => "&",
        SyntaxKind::Pipe => "|",
        SyntaxKind::Caret => "^",
        SyntaxKind::Tilde => "~",
        SyntaxKind::Shl => "<<",
        SyntaxKind::Shr => ">>",
        SyntaxKind::EqEq => "==",
        SyntaxKind::BangEq => "!=",
        SyntaxKind::Lt => "<",
        SyntaxKind::LtEq => "<=",
        SyntaxKind::Gt => ">",
        SyntaxKind::GtEq => ">=",
        SyntaxKind::AmpAmp => "&&",
        SyntaxKind::PipePipe => "||",
        SyntaxKind::Bang => "!",
        SyntaxKind::At => "@",
        SyntaxKind::Arrow => "->",
        SyntaxKind::DotDotDot => "...",
        SyntaxKind::Dot => ".",
        SyntaxKind::Comma => ",",
        SyntaxKind::Semi => ";",
        SyntaxKind::LParen => "(",
        SyntaxKind::RParen => ")",
        SyntaxKind::LBrace => "{",
        SyntaxKind::RBrace => "}",
        SyntaxKind::LBracket => "[",
        SyntaxKind::RBracket => "]",
        SyntaxKind::Whitespace => "Whitespace",
        SyntaxKind::LineComment => "LineComment",
        SyntaxKind::BlockComment => "BlockComment",
        SyntaxKind::LexError => "LexError",
        // node kinds → nonterminal names
        SyntaxKind::SourceFile => "SourceFile",
        SyntaxKind::ProcDef => "ProcDef",
        SyntaxKind::CHeaderDecl => "CHeaderDecl",
        SyntaxKind::ExternProcDecl => "ExternProcDecl",
        SyntaxKind::StructDef => "StructDef",
        SyntaxKind::FieldDecl => "FieldDecl",
        SyntaxKind::EnumDef => "EnumDef",
        SyntaxKind::EnumVariant => "EnumVariant",
        SyntaxKind::TypeList => "TypeList",
        SyntaxKind::SwitchStmt => "SwitchStmt",
        SyntaxKind::CaseClause => "CaseClause",
        SyntaxKind::EnumPattern => "EnumPattern",
        SyntaxKind::ParamList => "ParamList",
        SyntaxKind::Param => "Param",
        SyntaxKind::RetType => "RetType",
        SyntaxKind::Block => "Block",
        SyntaxKind::VarDecl => "VarDecl",
        SyntaxKind::AssignStmt => "AssignStmt",
        SyntaxKind::IfStmt => "IfStmt",
        SyntaxKind::ElseClause => "ElseClause",
        SyntaxKind::ForStmt => "ForStmt",
        SyntaxKind::Condition => "Condition",
        SyntaxKind::ReturnStmt => "ReturnStmt",
        SyntaxKind::BreakStmt => "BreakStmt",
        SyntaxKind::ContinueStmt => "ContinueStmt",
        SyntaxKind::ExprStmt => "ExprStmt",
        SyntaxKind::PointerType => "PointerType",
        SyntaxKind::RcPointerType => "RcPointerType",
        SyntaxKind::SliceType => "SliceType",
        SyntaxKind::ArrayType => "ArrayType",
        SyntaxKind::NameType => "NameType",
        SyntaxKind::GenericType => "GenericType",
        SyntaxKind::TypeArgList => "TypeArgList",
        SyntaxKind::LiteralExpr => "LiteralExpr",
        SyntaxKind::NameExpr => "NameExpr",
        SyntaxKind::ParenExpr => "ParenExpr",
        SyntaxKind::BinaryExpr => "BinaryExpr",
        SyntaxKind::PrefixExpr => "PrefixExpr",
        SyntaxKind::CastExpr => "CastExpr",
        SyntaxKind::AllocExpr => "AllocExpr",
        SyntaxKind::CompositeLit => "CompositeLit",
        SyntaxKind::Element => "Element",
        SyntaxKind::CallExpr => "CallExpr",
        SyntaxKind::FieldExpr => "FieldExpr",
        SyntaxKind::IndexExpr => "IndexExpr",
        SyntaxKind::SliceExpr => "SliceExpr",
        SyntaxKind::ArgList => "ArgList",
        SyntaxKind::Error => "Error",
    }
}

fn escape_for_display(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
