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
fn for_in_over_an_array_or_slice_resolves() {
    let errs = resolve_errors(
        "f :: proc(xs: []int32) -> int32 {\n    total := 0;\n    for n in xs {\n        total = total + n;\n    }\n    return total;\n}\n\nmain :: proc() -> int32 {\n    ys: [2]int32 = { 1, 2 };\n    sum := 0;\n    for v, [i] in ys {\n        sum = sum + v + i;\n    }\n    return sum;\n}\n",
    );
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn for_in_over_a_non_sequence_is_an_error() {
    // custom iterables need receiver methods, which do not exist yet, so
    // anything that is not an array or slice is rejected for now
    let errs = resolve_errors(
        "main :: proc() -> int32 {\n    x := 5;\n    for c in x {\n        return c;\n    }\n    return 0;\n}\n",
    );
    assert!(
        errs.iter()
            .any(|m| m.contains("array or slice can be iterated")),
        "{errs:?}"
    );
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
        "Maybe :: enum(comptime T: type) {\n    Some(T),\n    None,\n}\n\nNode :: struct {\n    value: int32,\n    next: Maybe(@Node),\n}\n\nmain :: proc() -> int32 {\n    n := alloc Node{ value: 1 };\n    return n.value;\n}\n",
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

#[test]
fn generic_proc_type_parameter_is_inferred() {
    let errs = resolve_errors(
        "identity :: proc(comptime T: type, x: T) -> T {\n    return x;\n}\n\nmain :: proc() -> int32 {\n    return identity(42);\n}\n",
    );
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn generic_proc_accepts_explicit_type_arguments() {
    let errs = resolve_errors(
        "identity :: proc(comptime T: type, x: T) -> T {\n    return x;\n}\n\nmain :: proc() -> int32 {\n    return identity(int32, 42);\n}\n",
    );
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn uninferable_type_parameter_is_an_error() {
    let errs = resolve_errors(
        "nothing :: proc(comptime T: type) -> int32 {\n    return 0;\n}\n\nmain :: proc() -> int32 {\n    return nothing();\n}\n",
    );
    assert!(errs.iter().any(|m| m.contains("cannot infer")), "{errs:?}");
}

#[test]
fn generic_proc_call_arity_is_checked() {
    let errs = resolve_errors(
        "identity :: proc(comptime T: type, x: T) -> T {\n    return x;\n}\n\nmain :: proc() -> int32 {\n    return identity(1, 2, 3);\n}\n",
    );
    assert!(
        errs.iter().any(|m| m.contains("takes 1 argument")),
        "{errs:?}"
    );
}

#[test]
fn type_parameter_is_in_scope_inside_the_proc_body() {
    let errs = resolve_errors(
        "pack :: proc(comptime T: type, value: T) -> int32 {\n    static_assert(sizeof(T) == 4, \"4-byte only\");\n    return 0;\n}\n\nmain :: proc() -> int32 {\n    return pack(1);\n}\n",
    );
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn omitted_field_takes_its_zero_value() {
    let errs = resolve_errors(
        "P :: struct {\n    x: int32,\n    flag: bool,\n}\n\nmain :: proc() -> int32 {\n    p := P{ x: 1 };\n    return p.x;\n}\n",
    );
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn omitted_non_nullable_pointer_field_is_an_error() {
    // Spec §4.3: there is no zero value for "non-nullable, but absent".
    let errs = resolve_errors(
        "Node :: struct {\n    value: int32,\n    next: @Node,\n}\n\nmain :: proc() -> int32 {\n    n := alloc Node{ value: 1 };\n    return n.value;\n}\n",
    );
    assert!(
        errs.iter().any(|m| m.contains("non-nullable pointer")),
        "{errs:?}"
    );
}

#[test]
fn omitted_enum_field_without_a_payload_less_variant_is_an_error() {
    let errs = resolve_errors(
        "E :: enum {\n    A(int32),\n    B(int32),\n}\n\nP :: struct {\n    e: E,\n    x: int32,\n}\n\nmain :: proc() -> int32 {\n    p := P{ x: 1 };\n    return p.x;\n}\n",
    );
    assert!(
        errs.iter().any(|m| m.contains("no payload-less variant")),
        "{errs:?}"
    );
}

#[test]
fn bare_composite_literal_needs_a_target_type() {
    let errs = resolve_errors(
        "P :: struct {\n    x: int32,\n}\n\nmain :: proc() -> int32 {\n    p := { x: 1 };\n    return 0;\n}\n",
    );
    assert!(errs.iter().any(|m| m.contains("cannot tell")), "{errs:?}");
}

#[test]
fn target_type_propagates_into_nested_bare_literals() {
    let errs = resolve_errors(
        "A :: struct {\n    v: int32,\n}\n\nB :: struct {\n    a: A,\n}\n\nmain :: proc() -> int32 {\n    b: B = { a: { v: 1 } };\n    return b.a.v;\n}\n",
    );
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn a_field_given_twice_is_an_error() {
    let errs = resolve_errors(
        "P :: struct {\n    x: int32,\n}\n\nmain :: proc() -> int32 {\n    p := P{ x: 1, x: 2 };\n    return p.x;\n}\n",
    );
    assert!(
        errs.iter().any(|m| m.contains("more than once")),
        "{errs:?}"
    );
}

#[test]
fn by_value_struct_cycles_are_rejected() {
    let direct = resolve_errors("P :: struct {\n    x: int32,\n    p: P,\n}\n");
    assert!(
        direct
            .iter()
            .any(|m| m.contains("contains itself by value")),
        "{direct:?}"
    );

    let mutual = resolve_errors("A :: struct {\n    b: B,\n}\n\nB :: struct {\n    a: A,\n}\n");
    assert!(
        mutual
            .iter()
            .any(|m| m.contains("contains itself by value")),
        "{mutual:?}"
    );
}

#[test]
fn recursion_through_a_pointer_is_allowed() {
    let errs = resolve_errors(
        "Maybe :: enum(comptime T: type) {\n    Some(T),\n    None,\n}\n\nNode :: struct {\n    value: int32,\n    next: Maybe(@Node),\n}\n\nmain :: proc() -> int32 {\n    n := alloc Node{ value: 1 };\n    return n.value;\n}\n",
    );
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn a_switch_must_cover_every_variant() {
    let errs = resolve_errors(
        "Maybe :: enum(comptime T: type) {\n    Some(T),\n    None,\n}\n\nmain :: proc() -> int32 {\n    m := Maybe(int32).None;\n    switch m {\n    case Maybe.Some(x):\n        return x;\n    }\n}\n",
    );
    assert!(
        errs.iter()
            .any(|m| m.contains("does not cover every variant") && m.contains("Maybe.None")),
        "{errs:?}"
    );
}

#[test]
fn a_switch_naming_every_variant_is_accepted() {
    let errs = resolve_errors(
        "E :: enum {\n    A,\n    B,\n    C,\n}\n\nmain :: proc() -> int32 {\n    e := E.A;\n    switch e {\n    case E.A:\n        return 1;\n    case E.B:\n        return 2;\n    case E.C:\n        return 3;\n    }\n}\n",
    );
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn a_switch_reports_all_missing_variants() {
    let errs = resolve_errors(
        "E :: enum {\n    A,\n    B,\n    C,\n}\n\nmain :: proc() -> int32 {\n    e := E.A;\n    switch e {\n    case E.A:\n        return 1;\n    }\n}\n",
    );
    let msg = errs.join(" ");
    assert!(msg.contains("E.B") && msg.contains("E.C"), "{errs:?}");
}

#[test]
fn a_value_must_match_the_type_of_its_location() {
    let init =
        resolve_errors("main :: proc() -> int32 {\n    x: int32 = true;\n    return x;\n}\n");
    assert!(
        init.iter().any(|m| m.contains("expects `int32`")),
        "{init:?}"
    );

    let field = resolve_errors(
        "P :: struct {\n    x: int32,\n}\n\nmain :: proc() -> int32 {\n    p := P{ x: true };\n    return p.x;\n}\n",
    );
    assert!(field.iter().any(|m| m.contains("field `x`")), "{field:?}");

    let ret = resolve_errors("f :: proc() -> int32 {\n    return true;\n}\n");
    assert!(ret.iter().any(|m| m.contains("`return`")), "{ret:?}");

    let arg = resolve_errors(
        "f :: proc(a: int32) -> int32 {\n    return a;\n}\n\nmain :: proc() -> int32 {\n    return f(true);\n}\n",
    );
    assert!(arg.iter().any(|m| m.contains("argument 1")), "{arg:?}");

    let assign =
        resolve_errors("main :: proc() -> int32 {\n    x := 1;\n    x = true;\n    return x;\n}\n");
    assert!(
        assign.iter().any(|m| m.contains("assignment")),
        "{assign:?}"
    );
}

#[test]
fn an_untyped_literal_coerces_to_its_location() {
    // §3.3: a literal takes the width of where it is stored, at no runtime cost.
    let errs = resolve_errors(
        "main :: proc() -> int32 {\n    a: int64 = 5;\n    b: uint8 = 42;\n    c: float32 = 1.5;\n    return 0;\n}\n",
    );
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn widening_a_typed_value_needs_a_cast() {
    // No implicit widening (§2.2) — unlike a literal, a typed value must be cast.
    let errs = resolve_errors(
        "f :: proc(a: int64) -> int64 {\n    return a;\n}\n\nmain :: proc() -> int32 {\n    x: int32 = 1;\n    return cast(int32) f(x);\n}\n",
    );
    assert!(
        errs.iter().any(|m| m.contains("expects `int64`")),
        "{errs:?}"
    );
}

#[test]
fn return_value_presence_must_match_the_signature() {
    let missing = resolve_errors("f :: proc() -> int32 {\n    return;\n}\n");
    assert!(
        missing.iter().any(|m| m.contains("needs a value")),
        "{missing:?}"
    );

    let extra = resolve_errors("f :: proc() {\n    return 5;\n}\n");
    assert!(
        extra.iter().any(|m| m.contains("takes no value")),
        "{extra:?}"
    );
}

#[test]
fn only_a_place_can_be_assigned_to() {
    let proc_target = resolve_errors(
        "f :: proc() -> int32 {\n    return 1;\n}\n\nmain :: proc() -> int32 {\n    f = 5;\n    return 0;\n}\n",
    );
    assert!(
        proc_target
            .iter()
            .any(|m| m.contains("cannot assign to it")),
        "{proc_target:?}"
    );

    let type_target = resolve_errors(
        "P :: struct {\n    x: int32,\n}\n\nmain :: proc() -> int32 {\n    P = 5;\n    return 0;\n}\n",
    );
    assert!(
        type_target
            .iter()
            .any(|m| m.contains("cannot assign to it")),
        "{type_target:?}"
    );
}

#[test]
fn array_and_slice_types_resolve() {
    let errs = resolve_errors(
        "sum :: proc(xs: []int32) -> int32 {\n    total := 0;\n    for i := 0; i < xs.len; i += 1 {\n        total = total + xs[i];\n    }\n    return total;\n}\n\nmain :: proc() -> int32 {\n    nums: [3]int32 = { 20, 20, 2 };\n    return sum(nums[:]);\n}\n",
    );
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn array_literal_length_and_element_types_are_checked() {
    let too_many = resolve_errors(
        "main :: proc() -> int32 {\n    xs: [2]int32 = { 1, 2, 3 };\n    return xs[0];\n}\n",
    );
    assert!(
        too_many.iter().any(|m| m.contains("holds 2 element")),
        "{too_many:?}"
    );

    let wrong_elem = resolve_errors(
        "main :: proc() -> int32 {\n    xs: [2]int32 = { 1, true };\n    return xs[0];\n}\n",
    );
    assert!(
        wrong_elem.iter().any(|m| m.contains("array element")),
        "{wrong_elem:?}"
    );

    let named = resolve_errors(
        "main :: proc() -> int32 {\n    xs: [2]int32 = { a: 1, b: 2 };\n    return xs[0];\n}\n",
    );
    assert!(named.iter().any(|m| m.contains("positional")), "{named:?}");
}

#[test]
fn arrays_and_slices_are_distinct_types() {
    // A `[]T` parameter does not accept a `[N]T` — the array must be sliced first.
    let errs = resolve_errors(
        "f :: proc(xs: []int32) -> int32 {\n    return xs.len;\n}\n\nmain :: proc() -> int32 {\n    ys: [2]int32 = { 1, 2 };\n    return f(ys);\n}\n",
    );
    assert!(
        errs.iter().any(|m| m.contains("expects `[]int32`")),
        "{errs:?}"
    );
}

#[test]
fn a_slice_has_only_len_and_ptr() {
    let errs = resolve_errors("f :: proc(xs: []int32) -> int32 {\n    return xs.nope;\n}\n");
    assert!(
        errs.iter().any(|m| m.contains("only `len` and `ptr`")),
        "{errs:?}"
    );
}

#[test]
fn indexing_and_slicing_require_the_right_types() {
    let bad_base = resolve_errors("main :: proc() -> int32 {\n    x := 5;\n    return x[0];\n}\n");
    assert!(
        bad_base.iter().any(|m| m.contains("cannot be indexed")),
        "{bad_base:?}"
    );

    let bad_index = resolve_errors(
        "main :: proc() -> int32 {\n    xs: [2]int32 = { 1, 2 };\n    return xs[true];\n}\n",
    );
    assert!(
        bad_index
            .iter()
            .any(|m| m.contains("index must be an integer")),
        "{bad_index:?}"
    );

    let bad_slice = resolve_errors(
        "main :: proc() -> int32 {\n    x := 5;\n    y := x[:];\n    return 0;\n}\n",
    );
    assert!(
        bad_slice.iter().any(|m| m.contains("can be sliced")),
        "{bad_slice:?}"
    );
}

#[test]
fn conditions_and_logical_operators_need_bools() {
    let cond = resolve_errors(
        "main :: proc() -> int32 {\n    if 5 {\n        return 1;\n    }\n    return 0;\n}\n",
    );
    assert!(
        cond.iter().any(|m| m.contains("condition needs a `bool`")),
        "{cond:?}"
    );

    let logical = resolve_errors(
        "main :: proc() -> int32 {\n    if 1 && 2 {\n        return 1;\n    }\n    return 0;\n}\n",
    );
    assert!(
        logical.iter().any(|m| m.contains("needs a `bool`")),
        "{logical:?}"
    );

    let not = resolve_errors(
        "main :: proc() -> int32 {\n    x := 5;\n    if !x {\n        return 1;\n    }\n    return 0;\n}\n",
    );
    assert!(
        not.iter().any(|m| m.contains("`!` needs a `bool`")),
        "{not:?}"
    );
}

#[test]
fn only_a_pointer_can_be_dereferenced() {
    let errs = resolve_errors("main :: proc() -> int32 {\n    x := 5;\n    return *x;\n}\n");
    assert!(
        errs.iter()
            .any(|m| m.contains("only a pointer can be dereferenced")),
        "{errs:?}"
    );
}
