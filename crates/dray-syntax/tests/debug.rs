// SPDX-License-Identifier: Apache-2.0

//! Tests for the debug pretty-printers

use dray_syntax::{
    DumpOptions, SyntaxKind, TokenKind, dump_cst, dump_cst_with, dump_tokens,
    dump_tokens_no_trivia, kind_name, parse, token_kind_name,
};

// ── token dumps ──────────────────────────────────────────────────────────────

#[test]
fn token_dump_has_one_line_per_token_including_trivia() {
    let src = "a := 1;";
    let dump = dump_tokens(src);
    let lines: Vec<_> = dump.lines().collect();
    // a, ws, :=, ws, 1, ;, Eof  = 7 lines
    assert_eq!(lines.len(), 7, "dump:\n{dump}");

    for line in &lines {
        assert!(line.contains(".."), "missing span in line: {line:?}");
    }
}

#[test]
fn token_dump_uses_glyph_names_for_operators() {
    let dump = dump_tokens("a := b + c;");
    assert!(dump.contains(":="), "should show := glyph\n{dump}");
    assert!(dump.contains(" + "), "should show + glyph\n{dump}");
    assert!(dump.contains("Ident"));
}

#[test]
fn token_dump_no_trivia_drops_whitespace_lines() {
    let src = "a := 1;";
    let full = dump_tokens(src);
    let lean = dump_tokens_no_trivia(src);
    assert!(full.lines().count() > lean.lines().count());
    assert!(!lean.contains("Whitespace"));
    // significant tokens: a := 1 ; Eof = 5
    assert_eq!(lean.lines().count(), 5, "{lean}");
}

#[test]
fn token_dump_escapes_newlines_and_tabs() {
    let dump = dump_tokens("a\n\tb");
    assert!(dump.contains("\\n"), "newline should be escaped\n{dump}");
    assert!(dump.contains("\\t"), "tab should be escaped\n{dump}");
}

#[test]
fn token_dump_shows_lex_errors() {
    let dump = dump_tokens("x := \"open");
    assert!(
        dump.contains("LexError"),
        "should surface lex error\n{dump}"
    );
}

// ── CST dumps ────────────────────────────────────────────────────────────────

#[test]
fn cst_dump_structural_hides_trivia_but_shows_spans() {
    let src = "main :: proc() {\n    return;\n}\n";
    let p = parse(src);
    let dump = dump_cst(&p.root);
    assert!(dump.contains("SourceFile@"), "spans on by default\n{dump}");
    assert!(dump.contains("ProcDef"));
    assert!(dump.contains("ReturnStmt"));
    assert!(
        !dump.contains("Whitespace"),
        "trivia should be hidden\n{dump}"
    );
}

#[test]
fn cst_dump_shape_only_has_no_spans_no_trivia() {
    let src = "f :: proc() {\n    x := 1 + 2;\n}\n";
    let p = parse(src);
    let dump = dump_cst_with(&p.root, DumpOptions::shape_only());
    assert!(!dump.contains('@'), "shape-only omits spans\n{dump}");
    assert!(!dump.contains("Whitespace"));
    // structure still present
    assert!(dump.contains("BinaryExpr"));
    assert!(dump.contains("VarDecl"));
}

#[test]
fn cst_dump_lossless_shows_trivia() {
    let src = "main :: proc() {\n    // hi\n    return;\n}\n";
    let p = parse(src);
    let dump = dump_cst_with(&p.root, DumpOptions::lossless());
    assert!(
        dump.contains("Whitespace"),
        "lossless shows whitespace\n{dump}"
    );
    assert!(
        dump.contains("LineComment"),
        "lossless shows comments\n{dump}"
    );
}

#[test]
fn cst_lossless_dump_token_text_reconstructs_source() {
    let src = "add :: proc(a: int32) -> int32 {\n    // doubled\n    return a + a;\n}\n";
    let p = parse(src);
    let dump = dump_cst_with(&p.root, DumpOptions::lossless());

    let mut reconstructed = String::new();
    for line in dump.lines() {
        if let Some(text) = extract_quoted(line) {
            reconstructed.push_str(&unescape(&text));
        }
    }
    assert_eq!(
        reconstructed, src,
        "lossless dump did not reconstruct source"
    );
}

#[test]
fn cst_dump_indents_by_depth() {
    let src = "f :: proc() {\n}\n";
    let p = parse(src);
    let dump = dump_cst_with(&p.root, DumpOptions::shape_only());
    let lines: Vec<_> = dump.lines().collect();
    assert!(lines[0].starts_with("SourceFile"));
    assert!(
        lines[1].starts_with("  ProcDef"),
        "second line: {:?}",
        lines[1]
    );
}

#[test]
fn cst_dump_builder_methods_compose() {
    let src = "f :: proc() {\n}\n";
    let p = parse(src);
    let opts = DumpOptions::default().with_spans(false).with_trivia(true);
    let dump = dump_cst_with(&p.root, opts);
    assert!(!dump.contains('@'), "spans should be off\n{dump}");
    assert!(dump.contains("Whitespace"), "trivia should be on\n{dump}");
}

// ── name tables ──────────────────────────────────────────────────────────────

#[test]
fn token_kind_names_are_glyphs_for_operators() {
    assert_eq!(token_kind_name(TokenKind::ColonColonEq), "::=");
    assert_eq!(token_kind_name(TokenKind::Arrow), "->");
    assert_eq!(token_kind_name(TokenKind::Shl), "<<");
    assert_eq!(token_kind_name(TokenKind::ShlEq), "<<=");
    assert_eq!(token_kind_name(TokenKind::AmpEq), "&=");
    assert_eq!(token_kind_name(TokenKind::KwTryAlloc), "try_alloc");
    assert_eq!(token_kind_name(TokenKind::Ident), "Ident");
}

#[test]
fn syntax_kind_names_cover_nodes_and_leaves() {
    assert_eq!(kind_name(SyntaxKind::BinaryExpr), "BinaryExpr");
    assert_eq!(kind_name(SyntaxKind::Plus), "+");
    assert_eq!(kind_name(SyntaxKind::ProcDef), "ProcDef");
    assert_eq!(kind_name(SyntaxKind::LexError), "LexError");
    assert_eq!(kind_name(SyntaxKind::AssignStmt), "AssignStmt");
    assert_eq!(kind_name(SyntaxKind::IfStmt), "IfStmt");
    assert_eq!(kind_name(SyntaxKind::ForStmt), "ForStmt");
    assert_eq!(kind_name(SyntaxKind::ExternProcDecl), "ExternProcDecl");
    assert_eq!(kind_name(SyntaxKind::Condition), "Condition");
    assert_eq!(kind_name(SyntaxKind::ShrEq), ">>=");
}

#[test]
fn cst_dump_renders_control_flow_readably() {
    let src = "f :: proc() {\n    for i := 0; i < 3; i += 1 {\n        x += i;\n    }\n}\n";
    let p = parse(src);
    let dump = dump_cst_with(&p.root, DumpOptions::shape_only());
    for expected in ["ForStmt", "Condition", "AssignStmt", "VarDecl"] {
        assert!(dump.contains(expected), "dump missing {expected}\n{dump}");
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Pull the substring inside the final `"..."` on a line, if any.
fn extract_quoted(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let close = line.rfind('"')?;
    // find the matching opening quote before `close`
    let open = line[..close].rfind('"')?;
    let _ = bytes;
    Some(line[open + 1..close].to_string())
}

/// Reverse `escape_for_display`'s transformations.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}
