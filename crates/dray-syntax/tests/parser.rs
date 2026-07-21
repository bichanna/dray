// SPDX-License-Identifier: Apache-2.0

//! Parser / CST tests for Dray.

use dray_syntax::{SyntaxKind, SyntaxNode, debug_tree, parse};

fn assert_lossless(src: &str) {
    let p = parse(src);
    assert_eq!(
        p.root.text(),
        src,
        "CST did not round-trip.\n--- tree ---\n{}",
        p.debug_tree()
    );
}

fn parse_ok(src: &str) -> SyntaxNode {
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "unexpected parse errors: {:?}\n--- tree ---\n{}",
        p.errors,
        p.debug_tree()
    );
    p.root
}

fn node_kinds(node: &SyntaxNode) -> Vec<SyntaxKind> {
    let mut out = vec![node.kind()];
    for child in node.children() {
        out.extend(node_kinds(&child));
    }
    out
}

/// True if any node in the subtree has the given kind.
fn has_node(node: &SyntaxNode, kind: SyntaxKind) -> bool {
    node_kinds(node).contains(&kind)
}

// ── smallest programs ────────────────────────────────────────────────────────

#[test]
fn empty_file_parses_to_empty_source_file() {
    let root = parse_ok("");
    assert_eq!(root.kind(), SyntaxKind::SourceFile);
    assert!(root.children().is_empty());
}

#[test]
fn walking_skeleton_main() {
    let src = "main :: proc() {\n}\n";
    let root = parse_ok(src);
    assert_eq!(root.kind(), SyntaxKind::SourceFile);
    let proc = root.child_of_kind(SyntaxKind::ProcDef).expect("a ProcDef");
    assert!(proc.child_of_kind(SyntaxKind::ParamList).is_some());
    assert!(proc.child_of_kind(SyntaxKind::Block).is_some());
    assert_lossless(src);
}

#[test]
fn proc_with_return_type_and_body() {
    let src = "answer :: proc() -> int32 {\n    return 42;\n}\n";
    let root = parse_ok(src);
    let proc = root.child_of_kind(SyntaxKind::ProcDef).unwrap();
    assert!(proc.child_of_kind(SyntaxKind::RetType).is_some());
    let block = proc.child_of_kind(SyntaxKind::Block).unwrap();
    assert!(block.child_of_kind(SyntaxKind::ReturnStmt).is_some());
    assert_lossless(src);
}

#[test]
fn proc_with_params() {
    let src = "add :: proc(a: int32, b: int32) -> int32 {\n    return a + b;\n}\n";
    let root = parse_ok(src);
    let proc = root.child_of_kind(SyntaxKind::ProcDef).unwrap();
    let params = proc.child_of_kind(SyntaxKind::ParamList).unwrap();
    let ps: Vec<_> = params.children();
    assert_eq!(ps.len(), 2, "two Param nodes");
    assert!(ps.iter().all(|p| p.kind() == SyntaxKind::Param));
    assert_lossless(src);
}

#[test]
fn comptime_param() {
    let src = "id :: proc(comptime T: type, x: T) -> T {\n    return x;\n}\n";
    let root = parse_ok(src);
    assert!(has_node(&root, SyntaxKind::Param));
    assert_lossless(src);
}

#[test]
fn c_header_decl() {
    let src = "c_header(\"stdio.h\");\n";
    let root = parse_ok(src);
    assert!(root.child_of_kind(SyntaxKind::CHeaderDecl).is_some());
    assert_lossless(src);
}

// ── types ────────────────────────────────────────────────────────────────────

#[test]
fn pointer_and_rc_pointer_types() {
    let src = "f :: proc(p: *int32, n: @Node) {\n}\n";
    let root = parse_ok(src);
    assert!(has_node(&root, SyntaxKind::PointerType));
    assert!(has_node(&root, SyntaxKind::RcPointerType));
    assert_lossless(src);
}

#[test]
fn slice_and_array_types() {
    let src = "f :: proc(s: []int32, a: [4]int32) {\n}\n";
    let root = parse_ok(src);
    assert!(has_node(&root, SyntaxKind::SliceType));
    assert!(has_node(&root, SyntaxKind::ArrayType));
    assert_lossless(src);
}

#[test]
fn generic_type() {
    let src = "f :: proc(s: Stack(int32)) {\n}\n";
    let root = parse_ok(src);
    assert!(has_node(&root, SyntaxKind::GenericType));
    assert_lossless(src);
}

#[test]
fn nested_type() {
    // @[]*Node — RC pointer to a slice of raw pointers to Node.
    let src = "f :: proc(x: @[]*Node) {\n}\n";
    let root = parse_ok(src);
    assert!(has_node(&root, SyntaxKind::RcPointerType));
    assert!(has_node(&root, SyntaxKind::SliceType));
    assert!(has_node(&root, SyntaxKind::PointerType));
    assert_lossless(src);
}

// ── statements & var decls ───────────────────────────────────────────────────

#[test]
fn bare_var_decls() {
    let src = "f :: proc() {\n    a := 1;\n    b ::= 2;\n    c :: 3;\n}\n";
    let root = parse_ok(src);
    let decls: Vec<_> = node_kinds(&root)
        .into_iter()
        .filter(|k| *k == SyntaxKind::VarDecl)
        .collect();
    assert_eq!(decls.len(), 3, "three VarDecls");
    assert_lossless(src);
}

#[test]
fn break_and_continue() {
    let src = "f :: proc() {\n    break;\n    continue;\n}\n";
    let root = parse_ok(src);
    assert!(has_node(&root, SyntaxKind::BreakStmt));
    assert!(has_node(&root, SyntaxKind::ContinueStmt));
    assert_lossless(src);
}

#[test]
fn return_with_no_value() {
    let src = "f :: proc() {\n    return;\n}\n";
    let root = parse_ok(src);
    assert!(has_node(&root, SyntaxKind::ReturnStmt));
    assert_lossless(src);
}

#[test]
fn nested_block_statement() {
    let src = "f :: proc() {\n    {\n        x := 1;\n    }\n}\n";
    let root = parse_ok(src);
    let proc = root.child_of_kind(SyntaxKind::ProcDef).unwrap();
    let body = proc.child_of_kind(SyntaxKind::Block).unwrap();
    assert!(body.child_of_kind(SyntaxKind::Block).is_some());
    assert_lossless(src);
}

// ── expressions & precedence ─────────────────────────────────────────────────

/// Render an expression tree as fully-parenthesized text, so precedence nesting
/// can be asserted with a simple string compare. Only handles the node kinds a
/// bare expression produces.
fn sexpr(node: &SyntaxNode) -> String {
    match node.kind() {
        SyntaxKind::BinaryExpr => {
            let kids = node.children();
            let op = node
                .children_with_tokens()
                .into_iter()
                .find_map(|e| match e {
                    dray_syntax::SyntaxElement::Token(t) if !t.kind().is_trivia() => {
                        Some(t.text().to_string())
                    }
                    _ => None,
                })
                .unwrap_or_default();
            format!("({} {} {})", sexpr(&kids[0]), op, sexpr(&kids[1]))
        }
        SyntaxKind::PrefixExpr => {
            let kid = node.children().into_iter().next().unwrap();
            let op = node
                .children_with_tokens()
                .into_iter()
                .find_map(|e| match e {
                    dray_syntax::SyntaxElement::Token(t) if !t.kind().is_trivia() => {
                        Some(t.text().to_string())
                    }
                    _ => None,
                })
                .unwrap_or_default();
            format!("({}{})", op, sexpr(&kid))
        }
        SyntaxKind::ParenExpr => {
            let kid = node.children().into_iter().next().unwrap();
            sexpr(&kid)
        }
        SyntaxKind::LiteralExpr | SyntaxKind::NameExpr => node.text().trim().to_string(),
        _ => node.text().trim().to_string(),
    }
}

/// Parse a single expression by wrapping it in a var decl and digging out the
/// expression child.
fn parse_expr(expr: &str) -> SyntaxNode {
    let src = format!("f :: proc() {{\n    x := {expr};\n}}\n");
    let root = parse_ok(&src);
    let proc = root.child_of_kind(SyntaxKind::ProcDef).unwrap();
    let block = proc.child_of_kind(SyntaxKind::Block).unwrap();
    let vardecl = block.child_of_kind(SyntaxKind::VarDecl).unwrap();
    vardecl.children().into_iter().next_back().unwrap()
}

#[test]
fn precedence_mul_binds_tighter_than_add() {
    let e = parse_expr("1 + 2 * 3");
    assert_eq!(sexpr(&e), "(1 + (2 * 3))");
}

#[test]
fn precedence_left_associative_subtraction() {
    let e = parse_expr("10 - 3 - 2");
    assert_eq!(sexpr(&e), "((10 - 3) - 2)");
}

#[test]
fn precedence_comparison_below_arithmetic() {
    let e = parse_expr("a + b < c * d");
    assert_eq!(sexpr(&e), "((a + b) < (c * d))");
}

#[test]
fn precedence_logical_or_is_lowest() {
    let e = parse_expr("a && b || c && d");
    assert_eq!(sexpr(&e), "((a && b) || (c && d))");
}

#[test]
fn precedence_full_chain() {
    let e = parse_expr("a | b ^ c & d == e");
    assert_eq!(sexpr(&e), "(a | (b ^ (c & (d == e))))");
}

#[test]
fn parens_override_precedence() {
    let e = parse_expr("(1 + 2) * 3");
    assert_eq!(sexpr(&e), "((1 + 2) * 3)");
}

#[test]
fn prefix_operators() {
    let e = parse_expr("-a + !b");
    assert_eq!(sexpr(&e), "((-a) + (!b))");
}

#[test]
fn prefix_deref_and_address() {
    let e = parse_expr("*p");
    assert_eq!(sexpr(&e), "(*p)");
    let e = parse_expr("&x");
    assert_eq!(sexpr(&e), "(&x)");
}

// ── postfix: call / field / index ────────────────────────────────────────────

#[test]
fn call_expression() {
    let src = "f :: proc() {\n    g(1, 2, 3);\n}\n";
    let root = parse_ok(src);
    assert!(has_node(&root, SyntaxKind::CallExpr));
    assert!(has_node(&root, SyntaxKind::ArgList));
    assert_lossless(src);
}

#[test]
fn field_and_method_chain() {
    let src = "f :: proc() {\n    a.b.c();\n}\n";
    let root = parse_ok(src);
    assert!(has_node(&root, SyntaxKind::FieldExpr));
    assert!(has_node(&root, SyntaxKind::CallExpr));
    assert_lossless(src);
}

#[test]
fn index_expression() {
    let src = "f :: proc() {\n    x := arr[i];\n}\n";
    let root = parse_ok(src);
    assert!(has_node(&root, SyntaxKind::IndexExpr));
    assert_lossless(src);
}

#[test]
fn cast_expression() {
    let src = "f :: proc() {\n    x := cast(int32, y);\n}\n";
    // NOTE: grammar's CastExpr is `cast ( Type ) UnaryExpr`, i.e. `cast(int32) y`.
    // The `cast(int32, y)` call-style spelling would be a CallExpr on `cast` —
    // but `cast` is a keyword, so this instead exercises the real form below
    let _ = src;
    let src = "f :: proc() {\n    x := cast(int32) y;\n}\n";
    let root = parse_ok(src);
    assert!(has_node(&root, SyntaxKind::CastExpr));
    assert_lossless(src);
}

#[test]
fn alloc_and_try_alloc_expressions() {
    let src = "f :: proc() {\n    a := alloc Node;\n    b := try_alloc Node;\n}\n";
    let root = parse_ok(src);
    let allocs: Vec<_> = node_kinds(&root)
        .into_iter()
        .filter(|k| *k == SyntaxKind::AllocExpr)
        .collect();
    assert_eq!(allocs.len(), 2);
    assert_lossless(src);
}

// ── trivia & losslessness under comments/whitespace ──────────────────────────

#[test]
fn comments_are_preserved_in_tree() {
    let src = "// header\nmain :: proc() {\n    // inside\n    return; // trailing\n}\n";
    assert_lossless(src);
    // The parse should be clean despite the comments.
    let p = parse(src);
    assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
}

#[test]
fn weird_but_valid_whitespace_round_trips() {
    let src = "main::proc()->int32{return 1+2;}";
    assert_lossless(src);
}

#[test]
fn multiple_top_level_decls() {
    let src = "c_header(\"stdio.h\");\n\nfoo :: proc() {\n}\n\nbar :: proc() -> int32 {\n    return 0;\n}\n";
    let root = parse_ok(src);
    let procs: Vec<_> = node_kinds(&root)
        .into_iter()
        .filter(|k| *k == SyntaxKind::ProcDef)
        .collect();
    assert_eq!(procs.len(), 2);
    assert!(root.child_of_kind(SyntaxKind::CHeaderDecl).is_some());
    assert_lossless(src);
}

// ── error recovery ───────────────────────────────────────────────────────────

#[test]
fn missing_semicolon_reports_but_recovers() {
    let src = "f :: proc() {\n    return 1\n}\n";
    let p = parse(src);
    assert!(!p.errors.is_empty(), "should report the missing ';'");
    assert_eq!(p.root.text(), src);
    assert!(p.root.child_of_kind(SyntaxKind::ProcDef).is_some());
}

#[test]
fn garbage_token_becomes_error_node_and_parsing_continues() {
    let src = "$ main :: proc() {\n}\n";
    let p = parse(src);
    assert!(!p.errors.is_empty());
    assert!(p.root.child_of_kind(SyntaxKind::ProcDef).is_some());
    assert_eq!(p.root.text(), src, "lossless even with an error node");
}

#[test]
fn unclosed_paren_in_expr_recovers() {
    let src = "f :: proc() {\n    x := (1 + 2;\n}\n";
    let p = parse(src);
    assert!(!p.errors.is_empty());
    assert_eq!(p.root.text(), src);
}

#[test]
fn lex_error_is_carried_into_tree_and_reported() {
    let src = "f :: proc() {\n    x := \"unterminated\n}\n";
    let p = parse(src);
    assert!(!p.errors.is_empty());
    assert!(
        p.errors.iter().any(|e| e.message.contains("string")),
        "errors: {:?}",
        p.errors
    );
    assert_eq!(p.root.text(), src);
}

#[test]
fn struct_decl_parses() {
    let src = "Node :: struct {\n    value: int32,\n}\n\nmain :: proc() {\n}\n";
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "struct should parse cleanly: {:?}",
        p.errors
    );
    assert_eq!(p.root.text(), src, "lossless");
    assert!(
        p.root.child_of_kind(SyntaxKind::StructDef).is_some(),
        "should have a StructDef\n{}",
        debug_tree(&p.root)
    );
    assert!(p.root.child_of_kind(SyntaxKind::ProcDef).is_some());
}

#[test]
fn does_not_infinite_loop_on_lone_operators() {
    for src in ["+++", "}}}", "::::", "((((", "proc proc proc"] {
        let p = parse(src);
        assert_eq!(p.root.text(), src, "failed round-trip on {src:?}");
    }
}

#[test]
fn extern_proc_decl() {
    let src = "puts :: extern \"puts\" proc(s: *int8) -> int32;\n";
    let root = parse_ok(src);
    let ext = root
        .child_of_kind(SyntaxKind::ExternProcDecl)
        .expect("an ExternProcDecl");
    assert!(ext.child_of_kind(SyntaxKind::ParamList).is_some());
    assert!(ext.child_of_kind(SyntaxKind::RetType).is_some());
    assert_lossless(src);
}

#[test]
fn extern_proc_decl_pub_and_no_return() {
    let src = "pub exit :: extern \"exit\" proc(code: int32);\n";
    let root = parse_ok(src);
    assert!(root.child_of_kind(SyntaxKind::ExternProcDecl).is_some());
    assert_lossless(src);
}

#[test]
fn extern_requires_trailing_semicolon() {
    let src = "puts :: extern \"puts\" proc(s: *int8) -> int32\n";
    let p = parse(src);
    assert!(!p.errors.is_empty(), "missing ';' should be reported");
    assert_eq!(p.root.text(), src);
}

#[test]
fn assignment_all_operators() {
    let ops = [
        "=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>=",
    ];
    for op in ops {
        let src = format!("f :: proc() {{\n    x {op} 1;\n}}\n");
        let root = parse_ok(&src);
        assert!(
            has_node(&root, SyntaxKind::AssignStmt),
            "op {op} should yield an AssignStmt"
        );
        assert_eq!(root.text(), src, "lossless for op {op}");
    }
}

#[test]
fn assignment_to_field_and_index() {
    let src = "f :: proc() {\n    h.prev = n;\n    arr[i] = 0;\n}\n";
    let root = parse_ok(src);
    let assigns: Vec<_> = node_kinds(&root)
        .into_iter()
        .filter(|k| *k == SyntaxKind::AssignStmt)
        .collect();
    assert_eq!(assigns.len(), 2);
    assert!(has_node(&root, SyntaxKind::FieldExpr));
    assert!(has_node(&root, SyntaxKind::IndexExpr));
    assert_lossless(src);
}

#[test]
fn expression_statement_is_not_mistaken_for_assignment() {
    let src = "f :: proc() {\n    doThing(1);\n}\n";
    let root = parse_ok(src);
    assert!(has_node(&root, SyntaxKind::ExprStmt));
    assert!(!has_node(&root, SyntaxKind::AssignStmt));
    assert_lossless(src);
}

#[test]
fn explicit_type_var_decls() {
    // all three post-annotation binders: `=`, `:=`, `:`
    let src = "f :: proc() {\n    a: int32 = 5;\n    b: @Node := n;\n    c: int32 : 3;\n}\n";
    let root = parse_ok(src);
    let decls: Vec<_> = node_kinds(&root)
        .into_iter()
        .filter(|k| *k == SyntaxKind::VarDecl)
        .collect();
    assert_eq!(decls.len(), 3);
    assert!(has_node(&root, SyntaxKind::NameType));
    assert!(has_node(&root, SyntaxKind::RcPointerType));
    assert_lossless(src);
}

#[test]
fn bare_and_typed_var_decls_coexist() {
    let src = "f :: proc() {\n    a := 1;\n    b: int32 = 2;\n}\n";
    let root = parse_ok(src);
    assert_eq!(
        node_kinds(&root)
            .into_iter()
            .filter(|k| *k == SyntaxKind::VarDecl)
            .count(),
        2
    );
    assert_lossless(src);
}

#[test]
fn if_statement_simple() {
    let src = "f :: proc() {\n    if x > 0 {\n        return;\n    }\n}\n";
    let root = parse_ok(src);
    let if_stmt = node_first(&root, SyntaxKind::IfStmt).expect("an IfStmt");
    assert!(if_stmt.child_of_kind(SyntaxKind::Condition).is_some());
    assert!(if_stmt.child_of_kind(SyntaxKind::Block).is_some());
    assert_lossless(src);
}

#[test]
fn if_else_chain() {
    let src = "f :: proc() {\n    if a {\n        return;\n    } else if b {\n        break;\n    } else {\n        continue;\n    }\n}\n";
    let root = parse_ok(src);
    assert!(has_node(&root, SyntaxKind::ElseClause));
    let ifs: Vec<_> = node_kinds(&root)
        .into_iter()
        .filter(|k| *k == SyntaxKind::IfStmt)
        .collect();
    assert_eq!(ifs.len(), 2, "outer if + else-if");
    assert_lossless(src);
}

#[test]
fn if_with_init_clause() {
    let src = "f :: proc() {\n    if y := compute(); y > 0 {\n        return;\n    }\n}\n";
    let root = parse_ok(src);
    let if_stmt = node_first(&root, SyntaxKind::IfStmt).unwrap();
    assert!(
        if_stmt.child_of_kind(SyntaxKind::VarDecl).is_some(),
        "init clause"
    );
    assert!(if_stmt.child_of_kind(SyntaxKind::Condition).is_some());
    assert_lossless(src);
}

#[test]
fn for_infinite() {
    let src = "f :: proc() {\n    for {\n        break;\n    }\n}\n";
    let root = parse_ok(src);
    let for_stmt = node_first(&root, SyntaxKind::ForStmt).unwrap();
    assert!(for_stmt.child_of_kind(SyntaxKind::Condition).is_none());
    assert!(for_stmt.child_of_kind(SyntaxKind::Block).is_some());
    assert_lossless(src);
}

#[test]
fn for_while_style() {
    let src = "f :: proc() {\n    for x < 100 {\n        x *= 2;\n    }\n}\n";
    let root = parse_ok(src);
    let for_stmt = node_first(&root, SyntaxKind::ForStmt).unwrap();
    assert!(for_stmt.child_of_kind(SyntaxKind::Condition).is_some());
    assert!(for_stmt.child_of_kind(SyntaxKind::VarDecl).is_none());
    assert_lossless(src);
}

#[test]
fn for_c_style() {
    let src = "f :: proc() {\n    for i := 0; i < 10; i += 1 {\n        sum += i;\n    }\n}\n";
    let root = parse_ok(src);
    let for_stmt = node_first(&root, SyntaxKind::ForStmt).unwrap();
    assert!(
        for_stmt.child_of_kind(SyntaxKind::VarDecl).is_some(),
        "init"
    );
    assert!(
        for_stmt.child_of_kind(SyntaxKind::Condition).is_some(),
        "cond"
    );
    assert!(
        for_stmt.child_of_kind(SyntaxKind::AssignStmt).is_some(),
        "post"
    );
    assert_lossless(src);
}

#[test]
fn for_c_style_empty_init() {
    let src = "f :: proc() {\n    for ; i < 10; i += 1 {\n        x += 1;\n    }\n}\n";
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "errors: {:?}\n{}",
        p.errors,
        debug_tree(&p.root)
    );
    assert_eq!(p.root.text(), src);
}

#[test]
fn for_range_style() {
    let src = "f :: proc() {\n    for c in items {\n        use(c);\n    }\n}\n";
    let root = parse_ok(src);
    assert!(has_node(&root, SyntaxKind::ForStmt));
    assert_lossless(src);
}

#[test]
fn for_range_with_index() {
    let src = "f :: proc() {\n    for c, [i] in items {\n        use(c);\n    }\n}\n";
    let root = parse_ok(src);
    assert!(has_node(&root, SyntaxKind::ForStmt));
    assert_lossless(src);
}

#[test]
fn nested_control_flow() {
    let src = "\
f :: proc() {\n\
    for i := 0; i < n; i += 1 {\n\
        if i > 5 {\n\
            total += i;\n\
        } else {\n\
            total -= i;\n\
        }\n\
    }\n\
}\n";
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "errors: {:?}\n{}",
        p.errors,
        debug_tree(&p.root)
    );
    assert_eq!(p.root.text(), src);
}

#[test]
fn functional_program_end_to_end() {
    // The milestone target: a whole program mixing extern C interop, control
    // flow, assignment, and returns parses with zero errors and round-trips
    let src = "\
c_header(\"stdio.h\");\n\
\n\
puts :: extern \"puts\" proc(s: *int8) -> int32;\n\
\n\
main :: proc() -> int32 {\n\
    sum := 0;\n\
    for i := 0; i < 10; i += 1 {\n\
        sum += i;\n\
    }\n\
    if sum > 20 {\n\
        puts(\"big\");\n\
        return 1;\n\
    }\n\
    return 0;\n\
}\n";
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "functional program should parse cleanly, errors: {:?}\n{}",
        p.errors,
        debug_tree(&p.root)
    );
    assert_eq!(p.root.text(), src, "must be lossless");
    assert!(p.root.child_of_kind(SyntaxKind::CHeaderDecl).is_some());
    assert!(p.root.child_of_kind(SyntaxKind::ExternProcDecl).is_some());
    assert!(p.root.child_of_kind(SyntaxKind::ProcDef).is_some());
}

/// Depth-first: the first node of `kind` anywhere in the subtree.
fn node_first(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    if node.kind() == kind {
        return Some(node.clone());
    }
    for child in node.children() {
        if let Some(found) = node_first(&child, kind) {
            return Some(found);
        }
    }
    None
}

#[test]
fn enum_and_switch_parse() {
    let src = "Shape :: enum {\n    Circle(int32),\n    Rectangle(int32, int32),\n}\n\nmain :: proc() -> int32 {\n    c := Shape.Circle(4);\n    switch c {\n    case Shape.Circle(r):\n        return r;\n    case Shape.Rectangle(w, h):\n        return w + h;\n    }\n}\n";
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "enum/switch should parse cleanly: {:?}",
        p.errors
    );
    assert_eq!(p.root.text(), src, "lossless");
    assert!(
        p.root.child_of_kind(SyntaxKind::EnumDef).is_some(),
        "should have an EnumDef\n{}",
        debug_tree(&p.root)
    );
}

#[test]
fn ellipsis_in_a_dray_proc_gets_one_clear_error() {
    let p = parse("weighted :: proc(xs: []int32, ...) -> int32 {\n    return 0;\n}\n");
    assert_eq!(p.errors.len(), 1, "{:?}", p.errors);
    assert!(
        p.errors[0].message.contains("not allowed in a Dray proc"),
        "{:?}",
        p.errors
    );
}

#[test]
fn ellipsis_is_accepted_on_an_extern() {
    let p = parse("printf :: extern \"printf\" proc(fmt: *int8, ...) -> int32;\n");
    assert!(p.errors.is_empty(), "{:?}", p.errors);
}
