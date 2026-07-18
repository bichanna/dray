// SPDX-License-Identifier: Apache-2.0

use dray_codegen::ir_to_c;
use dray_hir::lower;
use dray_syntax::parse;

fn c(src: &str) -> String {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let (hir, errs) = lower(&parsed.root);
    assert!(errs.is_empty(), "resolve errors: {errs:?}");
    let ir = dray_ir::lower(&dray_hir::monomorphize(hir).expect("monomorphize"));
    ir_to_c(&ir).unwrap_or_else(|e| panic!("codegen failed: {e}"))
}

#[test]
fn empty_main_is_void() {
    assert!(c("main :: proc() {\n}\n").contains("void main(void)"));
}

#[test]
fn main_with_int_return() {
    let out = c("main :: proc() -> int32 {\n    return 0;\n}\n");
    assert!(out.contains("int32_t main(void)"), "{out}");
    assert!(out.contains("return 0;"));
}

#[test]
fn stdint_is_always_included() {
    assert!(c("main :: proc() -> int32 {\n    return 0;\n}\n").contains("#include <stdint.h>"));
}

#[test]
fn c_header_becomes_include() {
    assert!(c("c_header(\"stdio.h\");\n\nmain :: proc() {\n}\n").contains("#include <stdio.h>"));
}

#[test]
fn params_lower_with_types() {
    let out = c("add :: proc(a: int32, b: int32) -> int32 {\n    return a + b;\n}\n");
    assert!(out.contains("int32_t add(int32_t a, int32_t b)"), "{out}");
}

#[test]
fn inferred_int_var_is_int32_not_plain_int() {
    let out = c("f :: proc() {\n    x := 5;\n}\n");
    assert!(out.contains("int32_t x = 5;"), "{out}");
}

#[test]
fn inferred_float_var_is_double() {
    let out = c("f :: proc() {\n    r := 1.5;\n}\n");
    assert!(out.contains("double r = 1.5;"), "{out}");
}

#[test]
fn extern_prototype_uses_linked_symbol_not_binding_name() {
    // `my_abs :: extern "abs"` must emit `abs`, so it links
    let out = c("my_abs :: extern \"abs\" proc(x: int32) -> int32;\n");
    assert!(out.contains("int32_t abs(int32_t x);"), "{out}");
    assert!(
        !out.contains("my_abs"),
        "binding name must not leak into C:\n{out}"
    );
}

#[test]
fn call_to_aliased_extern_uses_symbol() {
    let out = c(
        "my_abs :: extern \"abs\" proc(x: int32) -> int32;\n\nmain :: proc() -> int32 {\n    return my_abs(-3);\n}\n",
    );
    assert!(
        out.contains("return abs("),
        "call should use the symbol:\n{out}"
    );
}

// ── control flow lowering ────────────────────────────────────────────────────

#[test]
fn for_c_style_lowers_to_c_for() {
    let out = c("f :: proc() {\n    for i := 0; i < 10; i += 1 {\n        i += 0;\n    }\n}\n");
    assert!(out.contains("for (int32_t i = 0; i < 10; i += 1)"), "{out}");
}

#[test]
fn for_while_lowers_to_while() {
    let out = c("f :: proc() {\n    x := 0;\n    for x < 100 {\n        x += 1;\n    }\n}\n");
    assert!(out.contains("while (x < 100)"), "{out}");
}

#[test]
fn for_infinite_lowers_to_forever() {
    let out = c("f :: proc() {\n    for {\n        break;\n    }\n}\n");
    assert!(out.contains("for (;;)"), "{out}");
}

#[test]
fn if_else_lowers() {
    let out = c(
        "f :: proc() -> int32 {\n    x := 1;\n    if x == 1 {\n        return 1;\n    } else {\n        return 2;\n    }\n}\n",
    );
    assert!(out.contains("if (x == 1)") && out.contains("else"), "{out}");
}

// ── errors ───────────────────────────────────────────────────────────────────

#[test]
fn unresolved_name_never_reaches_valid_c() {
    let parsed = parse("f :: proc() -> int32 {\n    return ghost;\n}\n");
    let (_, errs) = lower(&parsed.root);
    assert!(!errs.is_empty());
}

// ── end-to-end: compile and run ──────────────────────────────────────────────

fn compile_and_run(c_src: &str) -> Option<i32> {
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    if Command::new(&cc).arg("--version").output().is_err() {
        return None;
    }

    // Tests run in parallel within one process, so the filename must be unique per
    // call. a timestamp alone coud collide between threads, which would let one
    // test run another's binary and read back the wrong exit code. A monotonic
    // counter guarantees uniqueness :) Hehe I'm smart.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let stamp = format!(
        "dray_cg_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let dir = std::env::temp_dir();
    let c_path = dir.join(format!("{stamp}.c"));
    let bin = dir.join(&stamp);
    std::fs::write(&c_path, c_src).unwrap();
    assert!(
        Command::new(&cc)
            .arg(&c_path)
            .arg("-o")
            .arg(&bin)
            .status()
            .unwrap()
            .success(),
        "cc failed:\n{c_src}"
    );
    let code = Command::new(&bin).status().unwrap().code().unwrap_or(-1);
    let _ = std::fs::remove_file(&c_path);
    let _ = std::fs::remove_file(&bin);
    Some(code)
}

#[test]
fn e2e_collatz_step_sum() {
    let src = "collatz_steps :: proc(start: int32) -> int32 {\n    n := start;\n    steps := 0;\n    for n > 1 {\n        if n % 2 == 0 {\n            n /= 2;\n        } else {\n            n = 3 * n + 1;\n        }\n        steps += 1;\n    }\n    return steps;\n}\n\nmain :: proc() -> int32 {\n    total := 0;\n    for i := 1; i < 10; i += 1 {\n        total += collatz_steps(i);\n    }\n    return total;\n}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 61);
    }
}

#[test]
fn e2e_extern_aliasing_links_and_runs() {
    let src = "my_abs :: extern \"abs\" proc(x: int32) -> int32;\n\nmain :: proc() -> int32 {\n    n := -7;\n    return my_abs(n);\n}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 7, "aliased extern must link and run");
    }
}

#[test]
fn e2e_prime_count() {
    let src = "is_prime :: proc(n: int32) -> int32 {\n    if n < 2 {\n        return 0;\n    }\n    for d := 2; d * d <= n; d += 1 {\n        if n % d == 0 {\n            return 0;\n        }\n    }\n    return 1;\n}\n\nmain :: proc() -> int32 {\n    count := 0;\n    for i := 2; i < 50; i += 1 {\n        if is_prime(i) == 1 {\n            count += 1;\n        }\n    }\n    return count;\n}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 15);
    }
}

#[test]
fn struct_emits_definition_constructor_and_drop() {
    let out = c(
        "Node :: struct {\n    value: int32,\n    next: @Node,\n}\n\nmain :: proc() -> int32 {\n    n := alloc Node{ value: 1 };\n    return n.value;\n}\n",
    );
    assert!(out.contains("struct Node;"), "forward decl: {out}");
    assert!(out.contains("Node *dray_new_Node("), "constructor: {out}");
    // Node has an @Node field, so it needs drop glue that releases it.
    assert!(out.contains("void dray_drop_Node"), "drop glue: {out}");
    assert!(
        out.contains("dray_rc_release((*self).next)"),
        "field release: {out}"
    );
}

#[test]
fn composite_alloc_calls_constructor_in_field_order() {
    let out = c(
        "P :: struct {\n    a: int32,\n    b: int32,\n}\n\nmain :: proc() -> int32 {\n    p := alloc P{ b: 2, a: 1 };\n    return p.a;\n}\n",
    );
    // Fields are reordered to declaration order (a, b) at the call site.
    assert!(out.contains("dray_new_P(1, 2)"), "field order: {out}");
    // P has no @T fields, so no drop function and a NULL drop pointer.
    assert!(
        !out.contains("dray_drop_P"),
        "no drop for scalar-only struct: {out}"
    );
}

#[test]
fn field_access_through_pointer_uses_deref() {
    let out = c(
        "N :: struct {\n    v: int32,\n}\n\nmain :: proc() -> int32 {\n    n := alloc N{ v: 7 };\n    return n.v;\n}\n",
    );
    assert!(out.contains("(*n).v"), "pointer field access: {out}");
}

#[test]
fn e2e_return_of_fresh_rc_transfers_ownership() {
    let src = "\
Box :: struct { value: int32 }\n\
rc_live :: extern \"dray_rc_live\" proc() -> int64;\n\
\n\
mk :: proc() -> @Box {\n\
    b := alloc Box{ value: 42 };\n\
    return b;\n\
}\n\
\n\
inner :: proc() -> int32 {\n\
    b := mk();\n\
    return b.value;\n\
}\n\
\n\
main :: proc() -> int32 {\n\
    v := inner();\n\
    return v + cast(int32)(rc_live());\n\
}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(
            code, 42,
            "returned-fresh-@T ownership must transfer without a stray release; got {code}"
        );
    }
}

#[test]
fn e2e_composite_lit_field_retains_source() {
    let src = "\
Node :: struct { value: int32, next: @Node }\n\
rc_live :: extern \"dray_rc_live\" proc() -> int64;\n\
\n\
build :: proc() {\n\
    a := alloc Node{ value: 1 };\n\
    b := alloc Node{ value: 2, next: a };\n\
}\n\
\n\
main :: proc() -> int32 {\n\
    build();\n\
    return cast(int32)(rc_live());\n\
}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(
            code, 0,
            "storing an @T into a fresh composite field must retain the source; got live={code}"
        );
    }
}

#[test]
fn e2e_reassigning_rc_local_releases_old() {
    let src = "\
Box :: struct { value: int32 }\n\
rc_live :: extern \"dray_rc_live\" proc() -> int64;\n\
\n\
churn :: proc() {\n\
    a := alloc Box{ value: 1 };\n\
    a = alloc Box{ value: 2 };\n\
    a = alloc Box{ value: 3 };\n\
}\n\
\n\
main :: proc() -> int32 {\n\
    churn();\n\
    return cast(int32)(rc_live());\n\
}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(
            code, 0,
            "reassigning an @T local must release the old value; got live={code}"
        );
    }
}

#[test]
fn generic_struct_monomorphizes_to_concrete_c() {
    let out = c("Box :: struct(comptime T: type) { value: T }\n\
                 main :: proc() -> int32 { b := alloc Box(int32){ value: 42 }; return b.value; }\n");
    // The concrete instantiation is emitted with a mangled name; the template is not.
    assert!(out.contains("struct Box_int32"), "concrete struct: {out}");
    assert!(out.contains("dray_new_Box_int32"), "concrete ctor: {out}");
    assert!(!out.contains("struct Box "), "template leaked: {out}");
}

#[test]
fn generic_enum_monomorphizes_to_concrete_c() {
    let out = c("Maybe :: enum(comptime T: type) { Some(T), None }\n\
                 main :: proc() -> int32 {\n\
                     m := Maybe(int32).Some(42);\n\
                     switch m { case Maybe.Some(v): return v; case Maybe.None: return 0; }\n\
                 }\n");
    assert!(out.contains("enum Maybe_int32_Tag"), "tag: {out}");
    assert!(out.contains("dray_new_Maybe_int32_Some"), "ctor: {out}");
    assert!(
        out.contains("case Maybe_int32_Some"),
        "switch uses concrete tag: {out}"
    );
    assert!(
        !out.contains("dray_new_Maybe_Some"),
        "template ctor leaked: {out}"
    );
}

#[test]
fn sizeof_lowers_to_c_sizeof() {
    let out = c("P :: struct { a: int32, b: int32 }\n\
                 main :: proc() -> int32 { n := sizeof(P); return cast(int32) n; }\n");
    assert!(out.contains("sizeof(struct P)"), "{out}");
}

#[test]
fn sizeof_of_generic_uses_the_concrete_type() {
    let out = c("Box :: struct(comptime T: type) { value: T }\n\
                 main :: proc() -> int32 { n := sizeof(Box(int32)); return cast(int32) n; }\n");
    assert!(out.contains("sizeof(struct Box_int32)"), "{out}");
}

#[test]
fn static_assert_lowers_and_leaves_no_runtime_code() {
    let out = c("main :: proc() -> int32 {\n\
                     static_assert(sizeof(int32) == 4, \"int32 is 4 bytes\");\n\
                     return 0;\n\
                 }\n");
    assert!(out.contains("_Static_assert("), "{out}");
    assert!(out.contains("\"int32 is 4 bytes\""), "{out}");
}

#[test]
fn e2e_sizeof_and_static_assert() {
    let src = "P :: struct { a: int32, b: int32 }\n\
               main :: proc() -> int32 {\n\
                   static_assert(sizeof(P) == 8, \"P is two int32s\");\n\
                   return cast(int32)(sizeof(int32) + sizeof(P));\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 12); // 4 + 8
    }
}
