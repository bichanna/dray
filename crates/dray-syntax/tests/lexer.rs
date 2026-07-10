// SPDX-License-Identifier: Apache-2.0

//! Lexer tests for Dray.

use dray_syntax::{tokenize, LexError, TokenKind};

fn kinds(src: &str) -> Vec<TokenKind> {
    tokenize(src)
        .into_iter()
        .filter(|t| !t.is_trivia())
        .map(|t| t.kind)
        .filter(|k| *k != TokenKind::Eof)
        .collect()
}

/// Assert the concatenated spans of every token exactly reconstruct the source —
/// the losslessness property the CST depends on (arch §5). No byte is dropped,
/// duplicated, or reordered.
fn assert_lossless(src: &str) {
    let toks = tokenize(src);
    let mut cursor = 0u32;
    for t in &toks {
        assert_eq!(
            t.span.start, cursor,
            "gap or overlap before token {:?} in {src:?}",
            t
        );
        cursor = t.span.end;
    }
    assert_eq!(cursor as usize, src.len(), "final span must reach EOF");
    let last = toks.last().unwrap();
    assert_eq!(last.kind, TokenKind::Eof);
    assert!(last.span.is_empty());
}

// ── basics ──────────────────────────────────────────────────────────────────

#[test]
fn empty_source_is_just_eof() {
    let toks = tokenize("");
    assert_eq!(toks.len(), 1);
    assert_eq!(toks[0].kind, TokenKind::Eof);
    assert!(toks[0].span.is_empty());
}

#[test]
fn whitespace_only_is_trivia_then_eof() {
    let toks = tokenize("   \t\n  ");
    assert_eq!(toks[0].kind, TokenKind::Whitespace);
    assert_eq!(toks.last().unwrap().kind, TokenKind::Eof);
    assert_eq!(kinds("   \t\n  "), Vec::<TokenKind>::new());
}

#[test]
fn spans_are_lossless_over_a_realistic_snippet() {
    // The `main` walking-skeleton program plus a comment and some operators.
    let src = "// entry\nmain :: proc() -> int32 {\n\treturn 1 + 2 * 3;\n}\n";
    assert_lossless(src);
}

// ── identifiers vs keywords vs predeclared identifiers ──────────────────────

#[test]
fn keywords_are_recognized() {
    assert_eq!(
        kinds("proc struct enum switch case default if else for in return"),
        vec![
            TokenKind::KwProc,
            TokenKind::KwStruct,
            TokenKind::KwEnum,
            TokenKind::KwSwitch,
            TokenKind::KwCase,
            TokenKind::KwDefault,
            TokenKind::KwIf,
            TokenKind::KwElse,
            TokenKind::KwFor,
            TokenKind::KwIn,
            TokenKind::KwReturn,
        ]
    );
    assert_eq!(
        kinds("alloc try_alloc cast pub import extern c_header break continue comptime true false"),
        vec![
            TokenKind::KwAlloc,
            TokenKind::KwTryAlloc,
            TokenKind::KwCast,
            TokenKind::KwPub,
            TokenKind::KwImport,
            TokenKind::KwExtern,
            TokenKind::KwCHeader,
            TokenKind::KwBreak,
            TokenKind::KwContinue,
            TokenKind::KwComptime,
            TokenKind::KwTrue,
            TokenKind::KwFalse,
        ]
    );
}

#[test]
fn predeclared_words_are_plain_identifiers_not_keywords() {
    for word in [
        "retain",
        "release",
        "sizeof",
        "static_assert",
        "free",
        "type",
        "int32",
        "float32",
        "string",
        "void",
        "Weak",
        "Result",
        "Maybe",
    ] {
        assert_eq!(
            kinds(word),
            vec![TokenKind::Ident],
            "{word} should be Ident"
        );
    }
}

#[test]
fn try_alloc_is_a_keyword_like_alloc() {
    assert_eq!(kinds("try_alloc"), vec![TokenKind::KwTryAlloc]);

    assert_eq!(
        kinds("alloc Node{ value = 1 }"),
        vec![
            TokenKind::KwAlloc,
            TokenKind::Ident,
            TokenKind::LBrace,
            TokenKind::Ident,
            TokenKind::Eq,
            TokenKind::IntLit,
            TokenKind::RBrace,
        ]
    );
    assert_eq!(
        kinds("try_alloc Node{ value = 1 }"),
        vec![
            TokenKind::KwTryAlloc,
            TokenKind::Ident,
            TokenKind::LBrace,
            TokenKind::Ident,
            TokenKind::Eq,
            TokenKind::IntLit,
            TokenKind::RBrace,
        ]
    );

    assert_eq!(kinds("try_alloc_helper"), vec![TokenKind::Ident]);
}

#[test]
fn lone_underscore_is_an_identifier() {
    assert_eq!(kinds("_"), vec![TokenKind::Ident]);
    assert_eq!(kinds("_x _1 x_"), vec![TokenKind::Ident; 3]);
}

#[test]
fn keyword_prefix_is_still_an_identifier() {
    assert_eq!(kinds("procedure"), vec![TokenKind::Ident]);
    assert_eq!(kinds("iffy for_each returns"), vec![TokenKind::Ident; 3]);
}

// ── the colon family (grammar lexer note: maximal munch) ────────────────────

#[test]
fn binding_operators_use_maximal_munch() {
    assert_eq!(kinds("::="), vec![TokenKind::ColonColonEq]);
    assert_eq!(kinds("::"), vec![TokenKind::ColonColon]);
    assert_eq!(kinds(":="), vec![TokenKind::ColonEq]);
    assert_eq!(kinds(":"), vec![TokenKind::Colon]);
    assert_eq!(kinds("="), vec![TokenKind::Eq]);
}

#[test]
fn colon_family_in_context() {
    assert_eq!(
        kinds("x ::= 1"),
        vec![TokenKind::Ident, TokenKind::ColonColonEq, TokenKind::IntLit]
    );
    assert_eq!(
        kinds("p: int32"),
        vec![TokenKind::Ident, TokenKind::Colon, TokenKind::Ident]
    );
    assert_eq!(
        kinds("a :: b"),
        vec![TokenKind::Ident, TokenKind::ColonColon, TokenKind::Ident]
    );
    assert_eq!(kinds(": :"), vec![TokenKind::Colon, TokenKind::Colon]);
}

// ── numbers: float vs. selector `.` ─────────────────────────────────────────

#[test]
fn integer_and_float_literals() {
    assert_eq!(kinds("0"), vec![TokenKind::IntLit]);
    assert_eq!(kinds("42"), vec![TokenKind::IntLit]);
    assert_eq!(kinds("3.14"), vec![TokenKind::FloatLit]);
    assert_eq!(kinds("100.0"), vec![TokenKind::FloatLit]);
}

#[test]
fn dot_after_integer_without_digit_is_a_selector_not_a_float() {
    assert_eq!(
        kinds("x.foo"),
        vec![TokenKind::Ident, TokenKind::Dot, TokenKind::Ident]
    );
    assert_eq!(
        kinds("3.foo"),
        vec![TokenKind::IntLit, TokenKind::Dot, TokenKind::Ident]
    );
    assert_eq!(kinds("3."), vec![TokenKind::IntLit, TokenKind::Dot]);
}

#[test]
fn no_range_operator_two_dots_are_two_dots() {
    assert_eq!(
        kinds("1..2"),
        vec![
            TokenKind::IntLit,
            TokenKind::Dot,
            TokenKind::Dot,
            TokenKind::IntLit
        ]
    );
}

#[test]
fn triple_dot_is_dotdotdot() {
    assert_eq!(kinds("..."), vec![TokenKind::DotDotDot]);
    assert_eq!(
        kinds("[...]int32"),
        vec![
            TokenKind::LBracket,
            TokenKind::DotDotDot,
            TokenKind::RBracket,
            TokenKind::Ident,
        ]
    );
}

// ── operators & punctuation ─────────────────────────────────────────────────

#[test]
fn arithmetic_and_compound_assign() {
    assert_eq!(
        kinds("+ - * / % += -= *= /= %="),
        vec![
            TokenKind::Plus,
            TokenKind::Minus,
            TokenKind::Star,
            TokenKind::Slash,
            TokenKind::Percent,
            TokenKind::PlusEq,
            TokenKind::MinusEq,
            TokenKind::StarEq,
            TokenKind::SlashEq,
            TokenKind::PercentEq,
        ]
    );
}

#[test]
fn all_compound_assignment_operators() {
    assert_eq!(
        kinds("&= |= ^="),
        vec![TokenKind::AmpEq, TokenKind::PipeEq, TokenKind::CaretEq]
    );
    assert_eq!(kinds("<<="), vec![TokenKind::ShlEq]);
    assert_eq!(kinds(">>="), vec![TokenKind::ShrEq]);
}

#[test]
fn shift_assign_maximal_munch() {
    assert_eq!(kinds("<<="), vec![TokenKind::ShlEq]);
    assert_eq!(kinds(">>="), vec![TokenKind::ShrEq]);
    assert_eq!(kinds("<< ="), vec![TokenKind::Shl, TokenKind::Eq]);
    assert_eq!(kinds("<<=="), vec![TokenKind::ShlEq, TokenKind::Eq]);
    assert_eq!(kinds("&="), vec![TokenKind::AmpEq]);
    assert_eq!(kinds("&&"), vec![TokenKind::AmpAmp]);
}

#[test]
fn shift_assign_lossless() {
    assert_lossless("x <<= 2; y >>= 1; z &= 7;");
}

#[test]
fn comparison_logical_bitwise() {
    assert_eq!(
        kinds("== != < <= > >= && || ! & | ^ ~ << >>"),
        vec![
            TokenKind::EqEq,
            TokenKind::BangEq,
            TokenKind::Lt,
            TokenKind::LtEq,
            TokenKind::Gt,
            TokenKind::GtEq,
            TokenKind::AmpAmp,
            TokenKind::PipePipe,
            TokenKind::Bang,
            TokenKind::Amp,
            TokenKind::Pipe,
            TokenKind::Caret,
            TokenKind::Tilde,
            TokenKind::Shl,
            TokenKind::Shr,
        ]
    );
}

#[test]
fn pointer_sigils_and_arrow() {
    assert_eq!(
        kinds("@Node *int32 -> void"),
        vec![
            TokenKind::At,
            TokenKind::Ident,
            TokenKind::Star,
            TokenKind::Ident,
            TokenKind::Arrow,
            TokenKind::Ident,
        ]
    );
}

#[test]
fn shift_is_not_two_comparisons() {
    assert_eq!(kinds("<<"), vec![TokenKind::Shl]);
    assert_eq!(kinds(">>"), vec![TokenKind::Shr]);
    assert_eq!(kinds("< <"), vec![TokenKind::Lt, TokenKind::Lt]);
}

#[test]
fn delimiters() {
    assert_eq!(
        kinds("(){}[],.;"),
        vec![
            TokenKind::LParen,
            TokenKind::RParen,
            TokenKind::LBrace,
            TokenKind::RBrace,
            TokenKind::LBracket,
            TokenKind::RBracket,
            TokenKind::Comma,
            TokenKind::Dot,
            TokenKind::Semi,
        ]
    );
}

// ── strings & runes ─────────────────────────────────────────────────────────

#[test]
fn simple_string_and_rune() {
    assert_eq!(kinds(r#""hello""#), vec![TokenKind::StringLit]);
    assert_eq!(kinds("'a'"), vec![TokenKind::RuneLit]);
    assert_eq!(kinds(r#""""#), vec![TokenKind::StringLit]);
}

#[test]
fn string_span_includes_both_quotes() {
    let toks = tokenize(r#""hi""#);
    let s = toks
        .iter()
        .find(|t| t.kind == TokenKind::StringLit)
        .unwrap();
    assert_eq!(s.span.start, 0);
    assert_eq!(s.span.end, 4); // "hi" is 4 bytes incl. quotes
}

#[test]
fn all_simple_escapes_accepted() {
    let src = r#""a\nb\tc\rd\\e\"f\'g\0h""#;
    assert_eq!(kinds(src), vec![TokenKind::StringLit]);
}

#[test]
fn hex_and_unicode_escapes() {
    assert_eq!(kinds(r#""\xFF""#), vec![TokenKind::StringLit]);
    assert_eq!(kinds(r#""\x00""#), vec![TokenKind::StringLit]);
    assert_eq!(kinds(r#""\u{1F600}""#), vec![TokenKind::StringLit]);
    assert_eq!(kinds(r#""\u{41}""#), vec![TokenKind::StringLit]);
}

#[test]
fn escaped_quote_does_not_end_string() {
    let toks = kinds(r#""say \"hi\" ok""#);
    assert_eq!(toks, vec![TokenKind::StringLit]);
}

#[test]
fn utf8_inside_string_and_rune() {
    assert_eq!(kinds("\"café — 日本語\""), vec![TokenKind::StringLit]);
    assert_eq!(kinds("'日'"), vec![TokenKind::RuneLit]);
    assert_lossless("\"café — 日本語\"");
}

// ── comments ────────────────────────────────────────────────────────────────

#[test]
fn line_comment_is_trivia() {
    let toks = tokenize("x // trailing\ny");
    let comments: Vec<_> = toks
        .iter()
        .filter(|t| t.kind == TokenKind::LineComment)
        .collect();
    assert_eq!(comments.len(), 1);
    // The two idents survive as non-trivia.
    assert_eq!(kinds("x // trailing\ny"), vec![TokenKind::Ident; 2]);
}

#[test]
fn line_comment_at_eof_without_newline() {
    assert_eq!(kinds("x // no newline at end"), vec![TokenKind::Ident]);
    assert_lossless("x // no newline at end");
}

#[test]
fn block_comment_is_trivia_and_non_nesting() {
    let src = "a /* one /* still one */ outer";
    assert_eq!(
        kinds(src),
        vec![TokenKind::Ident, TokenKind::Ident] // `a` and `outer`
    );
}

#[test]
fn block_comment_spanning_newlines() {
    let src = "a /* line one\nline two */ b";
    assert_eq!(kinds(src), vec![TokenKind::Ident, TokenKind::Ident]);
    assert_lossless(src);
}

// ── error recovery ──────────────────────────────────────────────────────────

fn first_error(src: &str) -> LexError {
    tokenize(src)
        .into_iter()
        .find_map(|t| match t.kind {
            TokenKind::Error(e) => Some(e),
            _ => None,
        })
        .expect("expected a lex error")
}

#[test]
fn unterminated_string() {
    assert_eq!(first_error("\"open"), LexError::UnterminatedString);
    assert_eq!(first_error("\"open\nnext"), LexError::UnterminatedString);
}

#[test]
fn unterminated_rune_and_bad_length() {
    assert_eq!(first_error("'a"), LexError::UnterminatedRune);
    assert_eq!(first_error("''"), LexError::BadRuneLength);
    assert_eq!(first_error("'ab'"), LexError::BadRuneLength);
}

#[test]
fn unterminated_block_comment() {
    assert_eq!(
        first_error("/* never closed"),
        LexError::UnterminatedBlockComment
    );
}

#[test]
fn bad_escapes() {
    assert_eq!(first_error(r#""\q""#), LexError::InvalidEscape);
    assert_eq!(first_error(r#""\xZZ""#), LexError::BadHexEscape);
    assert_eq!(first_error(r#""\x1""#), LexError::BadHexEscape); // only one digit
    assert_eq!(first_error(r#""\u{}""#), LexError::BadUnicodeEscape); // empty
    assert_eq!(first_error(r#""\u41""#), LexError::BadUnicodeEscape); // unbraced
    assert_eq!(
        first_error(r#""\u{1234567}""#),
        LexError::BadUnicodeEscape // 7 digits > 6
    );
}

#[test]
fn unexpected_char_is_isolated_and_recovers() {
    let ks = kinds("a $ b");
    assert_eq!(
        ks,
        vec![
            TokenKind::Ident,
            TokenKind::Error(LexError::UnexpectedChar),
            TokenKind::Ident,
        ]
    );
}

#[test]
fn lexing_continues_after_an_error() {
    let src = "x := \"unterminated\ny := 2;";
    let ks = kinds(src);
    assert!(ks.contains(&TokenKind::Error(LexError::UnterminatedString)));

    let tail = &ks[ks.len() - 4..];
    assert_eq!(
        tail,
        &[
            TokenKind::Ident,   // y
            TokenKind::ColonEq, // :=
            TokenKind::IntLit,  // 2
            TokenKind::Semi,    // ;
        ]
    );
}

#[test]
fn error_tokens_still_preserve_losslessness() {
    assert_lossless("a $ \"open\n'ab' /* x");
}

// ── a fuller program ────────────────────────────────────────────────────────

#[test]
fn tokenizes_a_representative_program() {
    let src = r#"
c_header("stdio.h");

Node :: struct {
    value: int32,
    next: @Node,
}

main :: proc() -> int32 {
    n := alloc Node{ value = 1, next = _ };
    for x, idx in items {
        total += x;
    }
    return cast(int32, 0);
}
"#;
    let ks = kinds(src);
    assert_eq!(ks[0], TokenKind::KwCHeader);
    assert!(ks.contains(&TokenKind::KwStruct));
    assert!(ks.contains(&TokenKind::At)); // @Node
    assert!(ks.contains(&TokenKind::KwAlloc));
    assert!(ks.contains(&TokenKind::KwFor));
    assert!(ks.contains(&TokenKind::KwIn));
    assert!(ks.contains(&TokenKind::PlusEq));
    assert!(ks.contains(&TokenKind::KwCast));
    assert!(ks.contains(&TokenKind::Arrow));

    assert!(!ks.iter().any(|k| matches!(k, TokenKind::Error(_))));
    assert_lossless(src);
}
