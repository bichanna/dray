// SPDX-License-Identifier: Apache-2.0

//! HIR lowering tests: name resolution, type inference, and the errors both raise

use dray_hir::{DefKind, ExprKind, Item, Stmt, Ty, dump_hir, lower};
use dray_syntax::parse;

/// Parse + lower, asserting no resolution errors, returning the HIR
fn hir(src: &str) -> dray_hir::Hir {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let (hir, errs) = lower(&parsed.root);
    assert!(errs.is_empty(), "unexpected resolve errors: {errs:?}");
    hir
}

fn resolve_errors(src: &str) -> Vec<String> {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let (_, errs) = lower(&parsed.root);
    errs.into_iter().map(|e| e.message).collect()
}

// ── resolution ───────────────────────────────────────────────────────────────

#[test]
fn resolves_a_local_reference() {
    let h = hir("f :: proc() -> int32 {\n    x := 5;\n    return x;\n}\n");
    let Item::Proc(p) = &h.items[0] else { panic!() };
    // return x -> x resolves to the local's DefId
    let Stmt::Return(Some(e)) = &p.body[1] else {
        panic!("expected return")
    };
    match &e.kind {
        ExprKind::Name { def, .. } => {
            assert_eq!(h.def(*def).kind, DefKind::Local);
        }
        other => panic!("expected resolved name, got {other:?}"),
    }
}

#[test]
fn resolves_forward_function_reference() {
    // main calls helper which is defined *after* main
    let h = hir(
        "main :: proc() -> int32 {\n    return helper();\n}\n\nhelper :: proc() -> int32 {\n    return 3;\n}\n",
    );
    let dump = dump_hir(&h);
    // helper resolves to a proc def id, referenced from main
    assert!(dump.contains("helper#"), "helper should resolve:\n{dump}");
}

#[test]
fn resolves_parameters() {
    let h = hir("add :: proc(a: int32, b: int32) -> int32 {\n    return a + b;\n}\n");
    let Item::Proc(p) = &h.items[0] else { panic!() };
    assert_eq!(p.params.len(), 2);
    assert!(
        p.params
            .iter()
            .all(|pp| h.def(pp.def).kind == DefKind::Param)
    );
}

#[test]
fn undefined_variable_is_an_error() {
    let errs = resolve_errors("f :: proc() -> int32 {\n    return nope;\n}\n");
    assert!(errs.iter().any(|m| m.contains("nope")), "{errs:?}");
}

#[test]
fn undefined_function_is_an_error() {
    let errs = resolve_errors("f :: proc() {\n    ghost();\n}\n");
    assert!(errs.iter().any(|m| m.contains("ghost")), "{errs:?}");
}

#[test]
fn block_scoping_is_respected() {
    let errs = resolve_errors(
        "f :: proc() -> int32 {\n    if 1 == 1 {\n        inner := 5;\n    }\n    return inner;\n}\n",
    );
    assert!(errs.iter().any(|m| m.contains("inner")), "{errs:?}");
}

#[test]
fn for_init_binding_visible_in_body_and_post() {
    let src = "f :: proc() -> int32 {\n    total := 0;\n    for i := 0; i < 5; i += 1 {\n        total += i;\n    }\n    return total;\n}\n";
    let errs = resolve_errors(src);
    assert!(
        errs.is_empty(),
        "for-init should resolve everywhere: {errs:?}"
    );
}

// ── type inference ───────────────────────────────────────────────────────────

fn let_ty(h: &dray_hir::Hir, proc_idx: usize, stmt_idx: usize) -> Ty {
    let Item::Proc(p) = &h.items[proc_idx] else {
        panic!()
    };
    match &p.body[stmt_idx] {
        Stmt::Let { ty, .. } => ty.clone(),
        other => panic!("expected a let, got {other:?}"),
    }
}

#[test]
fn infers_int_literal_as_int32() {
    let h = hir("f :: proc() {\n    x := 5;\n}\n");
    assert_eq!(let_ty(&h, 0, 0), Ty::i32());
}

#[test]
fn infers_float_literal_as_float64() {
    let h = hir("f :: proc() {\n    x := 1.5;\n}\n");
    assert_eq!(let_ty(&h, 0, 0), Ty::f64());
}

#[test]
fn infers_bool_literal() {
    let h = hir("f :: proc() {\n    x := true;\n}\n");
    assert_eq!(let_ty(&h, 0, 0), Ty::Bool);
}

#[test]
fn explicit_type_overrides_inference() {
    let h = hir("f :: proc() {\n    x: int64 = 5;\n}\n");
    assert_eq!(let_ty(&h, 0, 0), Ty::i64());
}

#[test]
fn infers_from_call_return_type() {
    // y := helper() where helper -> int32
    let h = hir(
        "helper :: proc() -> int32 {\n    return 1;\n}\n\nf :: proc() {\n    y := helper();\n}\n",
    );
    // f is items[1]; its first stmt is the let
    assert_eq!(let_ty(&h, 1, 0), Ty::i32());
}

#[test]
fn comparison_infers_bool() {
    let h = hir("f :: proc() {\n    b := 3 < 4;\n}\n");
    assert_eq!(let_ty(&h, 0, 0), Ty::Bool);
}

// ── extern symbol aliasing ───────────────────────────────────────────────────

#[test]
fn extern_carries_linked_symbol() {
    let h = hir("my_abs :: extern \"abs\" proc(x: int32) -> int32;\n");
    let Item::ExternProc(e) = &h.items[0] else {
        panic!()
    };
    assert_eq!(e.name, "my_abs");
    assert_eq!(e.symbol, "abs");
    assert_eq!(
        h.def(e.def).kind,
        DefKind::ExternProc {
            symbol: "abs".to_string()
        }
    );
}

// ── deferred constructs are clean errors ─────────────────────────────────────

#[test]
fn alloc_lowers_cleanly() {
    let errs = resolve_errors("f :: proc() {\n    x := alloc int32;\n    *x = 1;\n}\n");
    assert!(errs.is_empty(), "alloc should lower cleanly: {errs:?}");
}

#[test]
fn try_alloc_is_still_a_clean_error() {
    let errs = resolve_errors("f :: proc() {\n    x := try_alloc int32;\n}\n");
    assert!(errs.iter().any(|m| m.contains("try_alloc")), "{errs:?}");
}

#[test]
fn range_for_is_a_clean_error() {
    let errs = resolve_errors("f :: proc() {\n    for c in items {\n        use(c);\n    }\n}\n");
    assert!(errs.iter().any(|m| m.contains("range")), "{errs:?}");
}

#[test]
fn comments_are_not_folded_into_names() {
    let src = "f :: proc() -> int32 {\n    acc := 1;\n    for i := 2; i <= 3; i += 1 {\n        // a comment on its own line\n        acc *= i;\n    }\n    return acc;\n}\n";
    let errs = resolve_errors(src);
    assert!(
        errs.is_empty(),
        "comments must not break name resolution: {errs:?}"
    );
}

#[test]
fn comment_before_a_type_name_is_ignored() {
    let src = "f :: proc() {\n    x: // trailing comment\n       int32 = 5;\n}\n";
    let errs = resolve_errors(src);
    assert!(
        errs.is_empty(),
        "comment before a type must not break lowering: {errs:?}"
    );
}

#[test]
fn escapes_are_decoded() {
    let errs = resolve_errors("f :: proc() {\n    s := \"a\\nb\";\n}\n");
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn struct_defines_a_type_and_fields() {
    let errs = resolve_errors(
        "Node :: struct {\n    value: int32,\n    next: @Node,\n}\n\nmain :: proc() -> int32 {\n    n := alloc Node{ value: 1 };\n    return n.value;\n}\n",
    );
    assert!(errs.is_empty(), "struct program should resolve: {errs:?}");
}

#[test]
fn unknown_field_is_an_error() {
    let errs = resolve_errors(
        "Node :: struct {\n    value: int32,\n}\n\nmain :: proc() {\n    n := alloc Node{ nope: 1 };\n}\n",
    );
    assert!(errs.iter().any(|m| m.contains("no field")), "{errs:?}");
}

#[test]
fn enum_and_switch_resolve() {
    let errs = resolve_errors(
        "Shape :: enum {\n    Circle(int32),\n    Unit,\n}\n\nf :: proc(s: Shape) -> int32 {\n    switch s {\n    case Shape.Circle(r):\n        return r;\n    case Shape.Unit:\n        return 0;\n    }\n}\n",
    );
    assert!(errs.is_empty(), "enum program should resolve: {errs:?}");
}

#[test]
fn nonexistent_enum_variant_is_an_error() {
    let errs = resolve_errors(
        "Maybe :: enum(comptime T: type) {\n    Some(T),\n    None,\n}\n\nmain :: proc() -> int32 {\n    x := Maybe(int32).Nope;\n    return 0;\n}\n",
    );
    assert!(errs.iter().any(|m| m.contains("no variant")), "{errs:?}");
}

#[test]
fn variant_payload_arity_is_checked() {
    // `Some` takes one value; using it with none (as a unit variant) is an error.
    let errs = resolve_errors(
        "Maybe :: enum(comptime T: type) {\n    Some(T),\n    None,\n}\n\nmain :: proc() -> int32 {\n    x := Maybe(int32).Some;\n    return 0;\n}\n",
    );
    assert!(errs.iter().any(|m| m.contains("takes 1 value")), "{errs:?}");
}

#[test]
fn pattern_variant_binding_count_is_checked() {
    let errs = resolve_errors(
        "Maybe :: enum(comptime T: type) {\n    Some(T),\n    None,\n}\n\nmain :: proc() -> int32 {\n    m := Maybe(int32).None;\n    switch m {\n    case Maybe.Some(a, b):\n        return 0;\n    case Maybe.None:\n        return 0;\n    }\n}\n",
    );
    assert!(errs.iter().any(|m| m.contains("takes 1 value")), "{errs:?}");
}

#[test]
fn nonexistent_field_read_is_an_error() {
    let errs = resolve_errors(
        "P :: struct {\n    x: int32,\n}\n\nmain :: proc() -> int32 {\n    p := alloc P{ x: 1 };\n    return p.nope;\n}\n",
    );
    assert!(errs.iter().any(|m| m.contains("no field")), "{errs:?}");
}

#[test]
fn proc_call_arity_is_checked() {
    let errs = resolve_errors(
        "add :: proc(a: int32, b: int32) -> int32 {\n    return a;\n}\n\nmain :: proc() -> int32 {\n    return add(1);\n}\n",
    );
    assert!(
        errs.iter().any(|m| m.contains("takes 2 argument")),
        "{errs:?}"
    );
}

#[test]
fn duplicate_declarations_are_errors() {
    let field = resolve_errors("P :: struct {\n    x: int32,\n    x: bool,\n}\n");
    assert!(
        field.iter().any(|m| m.contains("duplicate field")),
        "{field:?}"
    );

    let variant = resolve_errors("E :: enum {\n    A,\n    A,\n}\n");
    assert!(
        variant.iter().any(|m| m.contains("duplicate variant")),
        "{variant:?}"
    );

    let param = resolve_errors("f :: proc(a: int32, a: int32) -> int32 {\n    return a;\n}\n");
    assert!(
        param.iter().any(|m| m.contains("duplicate parameter")),
        "{param:?}"
    );

    let decl = resolve_errors(
        "f :: proc() -> int32 {\n    return 1;\n}\n\nf :: proc() -> int32 {\n    return 2;\n}\n",
    );
    assert!(
        decl.iter().any(|m| m.contains("declared more than once")),
        "{decl:?}"
    );
}

#[test]
fn sizeof_and_static_assert_are_validated() {
    let bad_sizeof = resolve_errors(
        "main :: proc() -> int32 {\n    n := sizeof(int32, bool);\n    return 0;\n}\n",
    );
    assert!(
        bad_sizeof
            .iter()
            .any(|m| m.contains("exactly 1 type argument")),
        "{bad_sizeof:?}"
    );

    let bad_assert =
        resolve_errors("main :: proc() -> int32 {\n    static_assert(true);\n    return 0;\n}\n");
    assert!(
        bad_assert
            .iter()
            .any(|m| m.contains("condition and a message")),
        "{bad_assert:?}"
    );
}
