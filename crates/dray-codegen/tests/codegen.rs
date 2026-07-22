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
    let base_h = dir.join("draybase.h");
    let base_c = dir.join(format!("{stamp}_draybase.c"));
    std::fs::write(&base_h, dray_codegen::DRAYBASE_H).unwrap();
    std::fs::write(&base_c, dray_codegen::DRAYBASE_C).unwrap();

    // The generated C only has to compile and link. Warnings are the C
    // compiler's opinion about code nobody wrote by hand, so they are silenced
    // here the same way the driver silences them for users.
    let compile = Command::new(&cc)
        .arg("-std=c11")
        .arg("-w")
        .arg(&c_path)
        .arg(&base_c)
        .arg(format!("-I{}", dir.display()))
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap();

    assert!(
        compile.status.success(),
        "cc failed:\n{}\n--- generated C ---\n{c_src}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let code = Command::new(&bin).status().unwrap().code().unwrap_or(-1);
    let _ = std::fs::remove_file(&c_path);
    let _ = std::fs::remove_file(&base_c);
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
        "Inner :: struct {\n    value: int32,\n}\n\nNode :: struct {\n    value: int32,\n    inner: @Inner,\n}\n\nmain :: proc() -> int32 {\n    i := alloc Inner{ value: 1 };\n    n := alloc Node{ value: 1, inner: i };\n    return n.value;\n}\n",
    );

    assert!(out.contains("struct Inner;"), "forward decl: {out}");
    assert!(
        !out.contains("struct Node;"),
        "needless forward decl: {out}"
    );
    assert!(out.contains("Node *dray_new_Node("), "constructor: {out}");
    // Node has an @Inner field, so it needs drop glue that releases it.
    assert!(out.contains("void dray_drop_Node"), "drop glue: {out}");
    assert!(
        out.contains("dray_rc_release(self->inner)"),
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
    assert!(out.contains("n->v"), "pointer field access: {out}");
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
Inner :: struct { value: int32 }\n\
Node :: struct { value: int32, inner: @Inner }\n\
rc_live :: extern \"dray_rc_live\" proc() -> int64;\n\
\n\
build :: proc() {\n\
    a := alloc Inner{ value: 1 };\n\
    b := alloc Node{ value: 2, inner: a };\n\
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

#[test]
fn generic_proc_monomorphizes_per_instantiation() {
    let out = c(
        "identity :: proc(comptime T: type, x: T) -> T { return x; }\n\
                 main :: proc() -> int32 {\n\
                     a := identity(1);\n\
                     b := identity(true);\n\
                     return a;\n\
                 }\n",
    );
    assert!(out.contains("identity_int32(int32_t x)"), "{out}");
    assert!(out.contains("identity_bool(bool x)"), "{out}");
    assert!(!out.contains("identity(int32_t"), "template leaked: {out}");
}

#[test]
fn procs_get_prototypes_so_forward_calls_work() {
    let out = c("main :: proc() -> int32 { return helper(); }\n\
                 helper :: proc() -> int32 { return 1; }\n");
    // A prototype precedes the definition of `main`, so the later `helper` is
    // declared before its use
    let proto = out.find("int32_t helper(void);").expect("prototype");
    let body = out.find("int32_t main(void) {").expect("main body");
    assert!(proto < body, "prototype must precede definitions: {out}");
}

#[test]
fn e2e_mutual_recursion() {
    let src = "is_even :: proc(n: int32) -> bool {\n\
                   if n == 0 { return true; }\n\
                   return is_odd(n - 1);\n\
               }\n\
               is_odd :: proc(n: int32) -> bool {\n\
                   if n == 0 { return false; }\n\
                   return is_even(n - 1);\n\
               }\n\
               main :: proc() -> int32 {\n\
                   if is_even(10) { return 42; }\n\
                   return 0;\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 42);
    }
}

#[test]
fn e2e_generic_proc_with_inference() {
    let src = "identity :: proc(comptime T: type, x: T) -> T { return x; }\n\
               first :: proc(comptime T: type, a: T, b: T) -> T { return a; }\n\
               main :: proc() -> int32 {\n\
                   return identity(40) + first(2, 99);\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 42);
    }
}

#[test]
fn struct_literal_lowers_to_a_compound_literal() {
    let out = c("P :: struct { x: int32, y: int32 }\n\
                 main :: proc() -> int32 { p := P{x: 1, y: 2}; return p.x; }\n");
    assert!(out.contains("(struct P){"), "compound literal: {out}");
    assert!(out.contains(".x=1"), "designated init: {out}");
}

#[test]
fn omitted_fields_are_filled_with_zero_values() {
    let out = c("P :: struct { x: int32, flag: bool }\n\
                 main :: proc() -> int32 { p := P{x: 1}; return p.x; }\n");
    // Every field is present in the emitted initializer, the omitted one zeroed.
    assert!(out.contains(".flag=false"), "zeroed field: {out}");
}

#[test]
fn e2e_stack_struct_literal_and_zero_values() {
    let src = "P :: struct { x: int32, y: int32 }\n\
               Outer :: struct { p: P, extra: int32 }\n\
               main :: proc() -> int32 {\n\
                   o: Outer = { p: { x: 40, y: 2 } };\n\
                   return o.p.x + o.p.y + o.extra;\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 42);
    }
}

#[test]
fn e2e_by_value_generic_nesting() {
    let src = "Box :: struct(comptime T: type) { value: T }\n\
               main :: proc() -> int32 {\n\
                   b := Box(Box(int32)){ value: Box(int32){ value: 42 } };\n\
                   return b.value.value;\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 42);
    }
}

#[test]
fn e2e_omitted_maybe_field_defaults_to_none() {
    let src = "Maybe :: enum(comptime T: type) { Some(T), None }\n\
               Node :: struct { value: int32, next: Maybe(@Node) }\n\
               main :: proc() -> int32 {\n\
                   n := alloc Node{ value: 42 };\n\
                   switch n.next {\n\
                   case Maybe.Some(x): return 0;\n\
                   case Maybe.None: return n.value;\n\
                   }\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 42);
    }
}

#[test]
fn e2e_rc_local_stored_in_an_enum_payload_is_retained() {
    let src = "Node :: struct { value: int32 }\n\
               Maybe :: enum(comptime T: type) { Some(T), None }\n\
               rc_live :: extern \"dray_rc_live\" proc() -> int64;\n\
               main :: proc() -> int32 {\n\
                   m := Maybe(@Node).None;\n\
                   if true {\n\
                       a := alloc Node{ value: 7 };\n\
                       m = Maybe(@Node).Some(a);\n\
                   }\n\
                   switch m {\n\
                   case Maybe.Some(n): return cast(int32) rc_live();\n\
                   case Maybe.None: return 0;\n\
                   }\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 1, "the referenced node must still be alive");
    }
}

#[test]
fn e2e_by_value_struct_releases_its_rc_fields() {
    // A by-value aggregate owns the `@T` it holds; when it dies, that reference
    // must be given up or the object leaks.
    let src = "Node :: struct { value: int32 }\n\
               Holder :: struct { n: @Node }\n\
               rc_live :: extern \"dray_rc_live\" proc() -> int64;\n\
               main :: proc() -> int32 {\n\
                   if true {\n\
                       a := alloc Node{ value: 7 };\n\
                       h := Holder{ n: a };\n\
                   }\n\
                   return cast(int32) rc_live();\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 0, "by-value struct leaked its @Node");
    }
}

#[test]
fn e2e_by_value_enum_releases_its_payload() {
    let src = "Node :: struct { value: int32 }\n\
               Maybe :: enum(comptime T: type) { Some(T), None }\n\
               rc_live :: extern \"dray_rc_live\" proc() -> int64;\n\
               main :: proc() -> int32 {\n\
                   if true {\n\
                       a := alloc Node{ value: 7 };\n\
                       m := Maybe(@Node).Some(a);\n\
                   }\n\
                   return cast(int32) rc_live();\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 0, "by-value enum leaked its payload");
    }
}

#[test]
fn e2e_nested_by_value_aggregates_release_transitively() {
    let src = "Node :: struct { value: int32 }\n\
               Inner :: struct { n: @Node }\n\
               Outer :: struct { inner: Inner }\n\
               rc_live :: extern \"dray_rc_live\" proc() -> int64;\n\
               main :: proc() -> int32 {\n\
                   if true {\n\
                       a := alloc Node{ value: 7 };\n\
                       o := alloc Outer{ inner: Inner{ n: a } };\n\
                   }\n\
                   return cast(int32) rc_live();\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 0, "nested by-value aggregate leaked");
    }
}

#[test]
fn enum_with_an_rc_payload_gets_drop_glue() {
    let out = c("Node :: struct { value: int32 }\n\
                 Maybe :: enum(comptime T: type) { Some(T), None }\n\
                 main :: proc() -> int32 {\n\
                     a := alloc Node{ value: 1 };\n\
                     m := Maybe(@Node).Some(a);\n\
                     return 0;\n\
                 }\n");
    assert!(
        out.contains("void dray_drop_Maybe_rc_Node"),
        "enum drop glue: {out}"
    );

    assert!(out.contains("switch (self->tag)"), "tag switch: {out}");
}

#[test]
fn enum_drop_glue_handles_every_tag_value() {
    let out = c("Node :: struct { value: int32 }\n\
                 Maybe :: enum(comptime T: type) { Some(T), None }\n\
                 main :: proc() -> int32 {\n\
                     a := alloc Node{ value: 1 };\n\
                     m := Maybe(@Node).Some(a);\n\
                     return 0;\n\
                 }\n");
    assert!(out.contains("void dray_drop_Maybe_rc_Node"), "{out}");
    assert!(
        out.contains("default:"),
        "drop switch needs a default: {out}"
    );
}

#[test]
fn generated_functions_are_declared_before_use() {
    let out = c("Maybe :: enum(comptime T: type) { Some(T), None }\n\
                 Node :: struct { value: int32, next: Maybe(@Node) }\n\
                 main :: proc() -> int32 {\n\
                     n := alloc Node{ value: 1 };\n\
                     return n.value;\n\
                 }\n");
    let proto = out
        .find("void dray_drop_Maybe_rc_Node(void *p);")
        .expect("enum drop prototype");
    let caller = out
        .find("void dray_drop_Node(void *p) {")
        .expect("struct drop definition");
    assert!(proto < caller, "prototype must precede the caller: {out}");
}

#[test]
fn an_unused_switch_binding_emits_no_local() {
    let out = c("Maybe :: enum(comptime T: type) { Some(T), None }\n\
                 main :: proc() -> int32 {\n\
                     m := Maybe(int32).Some(1);\n\
                     switch m {\n\
                     case Maybe.Some(x): return 7;\n\
                     case Maybe.None: return 0;\n\
                     }\n\
                 }\n");
    assert!(
        !out.contains("int32_t x ="),
        "unused binding materialized: {out}"
    );
}

#[test]
fn a_used_switch_binding_is_still_emitted() {
    let out = c("Maybe :: enum(comptime T: type) { Some(T), None }\n\
                 main :: proc() -> int32 {\n\
                     m := Maybe(int32).Some(1);\n\
                     switch m {\n\
                     case Maybe.Some(x): return x;\n\
                     case Maybe.None: return 0;\n\
                     }\n\
                 }\n");
    assert!(
        out.contains("int32_t x ="),
        "used binding must be bound: {out}"
    );
}

#[test]
fn generated_c_has_no_duplicate_includes() {
    let out = c("Node :: struct { value: int32 }\n\
                 main :: proc() -> int32 { n := alloc Node{ value: 1 }; return n.value; }\n");
    assert_eq!(out.matches("#include <stdint.h>").count(), 1, "{out}");
    // `stdlib.h` belongs to the runtime, which now lives in its own header rather
    // than being copied into every generated file.
    assert_eq!(out.matches("#include <stdlib.h>").count(), 0, "{out}");
    assert_eq!(out.matches("#include \"draybase.h\"").count(), 1, "{out}");
}

#[test]
fn main_gets_no_prototype() {
    let out = c("main :: proc() -> int32 { return 0; }\n");
    assert!(!out.contains("int32_t main(void);"), "{out}");
    assert!(out.contains("int32_t main(void) {"), "{out}");
}

#[test]
fn pointer_field_access_uses_the_arrow_operator() {
    let out = c("Node :: struct { value: int32 }\n\
                 main :: proc() -> int32 { n := alloc Node{ value: 1 }; return n.value; }\n");
    assert!(out.contains("n->value"), "{out}");
    assert!(!out.contains("(*n).value"), "{out}");
}

#[test]
fn a_struct_without_rc_fields_passes_null_as_its_drop() {
    let out = c("P :: struct { x: int32 }\n\
                 main :: proc() -> int32 { p := alloc P{ x: 1 }; return p.x; }\n");
    assert!(out.contains("sizeof(struct P), NULL"), "{out}");
}

#[test]
fn only_pointed_to_aggregates_are_forward_declared() {
    // A recursive type needs the stub; a standalone one does not.
    let recursive = c("Maybe :: enum(comptime T: type) { Some(T), None }\n\
                       Node :: struct { value: int32, next: Maybe(@Node) }\n\
                       main :: proc() -> int32 { n := alloc Node{ value: 1 }; return n.value; }\n");
    assert!(recursive.contains("struct Node;"), "{recursive}");

    let plain = c("P :: struct { x: int32 }\n\
                   main :: proc() -> int32 { p := alloc P{ x: 1 }; return p.x; }\n");
    assert!(
        !plain.contains("struct P;"),
        "needless forward decl: {plain}"
    );
}

#[test]
fn a_fixed_array_lowers_to_a_c_array() {
    let out = c("main :: proc() -> int32 { xs: [3]int32 = {1, 2, 3}; return xs[0]; }\n");
    assert!(out.contains("int32_t xs[3] = {1, 2, 3}"), "{out}");
    assert!(out.contains("xs[0]"), "{out}");
}

#[test]
fn a_slice_lowers_to_a_len_ptr_struct() {
    let out = c("f :: proc(xs: []int32) -> int32 { return xs.len; }\n\
                 main :: proc() -> int32 { ys: [2]int32 = {1, 2}; return f(ys[:]); }\n");
    assert!(out.contains("struct DraySlice_int32 {"), "{out}");
    assert!(out.contains("int32_t len;"), "{out}");
    assert!(out.contains("int32_t *ptr;"), "{out}");
    assert!(out.contains(".len=2"), "{out}");
    assert!(out.contains(".ptr=&ys[0]"), "{out}");
}

#[test]
fn indexing_a_slice_goes_through_its_data_pointer() {
    let out = c("f :: proc(xs: []int32) -> int32 { return xs[0]; }\n\
                 main :: proc() -> int32 { return 0; }\n");
    assert!(out.contains("xs.ptr[0]"), "{out}");
}

#[test]
fn one_slice_struct_is_emitted_per_element_type() {
    let out = c(
        "f :: proc(a: []int32, b: []int32) -> int32 { return a.len + b.len; }\n\
                 main :: proc() -> int32 { return 0; }\n",
    );
    assert_eq!(out.matches("struct DraySlice_int32 {").count(), 1, "{out}");
}

#[test]
fn e2e_arrays_and_slices() {
    let src = "sum :: proc(xs: []int32) -> int32 {\n\
                   total := 0;\n\
                   for i := 0; i < xs.len; i += 1 {\n\
                       total = total + xs[i];\n\
                   }\n\
                   return total;\n\
               }\n\
               main :: proc() -> int32 {\n\
                   nums: [3]int32 = { 20, 20, 2 };\n\
                   return sum(nums[:]);\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 42);
    }
}

#[test]
fn e2e_omitted_array_elements_are_zeroed() {
    let src = "main :: proc() -> int32 {\n\
                   xs: [4]int32 = { 42 };\n\
                   return xs[0] + xs[1] + xs[2] + xs[3];\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 42);
    }
}

#[test]
fn for_in_over_a_slice_lowers_to_an_indexed_loop() {
    let out = c("sum :: proc(xs: []int32) -> int32 {\n\
                     total := 0;\n\
                     for n in xs { total = total + n; }\n\
                     return total;\n\
                 }\n\
                 main :: proc() -> int32 { return 0; }\n");
    assert!(out.contains("< xs.len"), "{out}");
    assert!(out.contains("= xs.ptr["), "{out}");
}

#[test]
fn for_in_over_an_array_does_not_copy_it() {
    // C has no array assignment, so the loop must index the original array
    let out = c("main :: proc() -> int32 {\n\
                     ys: [3]int32 = {1, 2, 3};\n\
                     t := 0;\n\
                     for v in ys { t = t + v; }\n\
                     return t;\n\
                 }\n");
    assert!(out.contains("= ys["), "{out}");
    assert!(!out.contains("__dray_seq"), "array was copied: {out}");
}

#[test]
fn e2e_for_in_over_arrays_and_slices() {
    let src = "sum :: proc(xs: []int32) -> int32 {\n\
                   total := 0;\n\
                   for n in xs { total = total + n; }\n\
                   return total;\n\
               }\n\
               main :: proc() -> int32 {\n\
                   nums: [4]int32 = { 10, 20, 4, 3 };\n\
                   indexed := 0;\n\
                   for v, [i] in nums { indexed = indexed + v + i; }\n\
                   return sum(nums[:]) + indexed - 37 - 6;\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 37);
    }
}

#[test]
fn e2e_assigning_an_array() {
    let src = "main :: proc() -> int32 {\n\
                   a: [3]int32 = { 1, 2, 3 };\n\
                   b: [3]int32 = { 20, 20, 2 };\n\
                   a = { 10, 10, 1 };\n\
                   a = b;\n\
                   return a[0] + a[1] + a[2];\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 42);
    }
}

#[test]
fn e2e_typed_array_literal_in_expression_position() {
    let src = "main :: proc() -> int32 {\n\
                   nums := [4]int32{ 20, 20, 2, 0 };\n\
                   return nums[0] + nums[1] + nums[2];\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 42);
    }
}

#[test]
fn a_slice_typed_local_gets_its_struct_emitted() {
    let out = c("main :: proc() -> int32 {\n\
                     n: [4]uint8 = { 1, 2, 3, 4 };\n\
                     s := n[:];\n\
                     return s.len;\n\
                 }\n");
    assert!(out.contains("struct DraySlice_uint8 {"), "{out}");
}

#[test]
fn identifiers_that_are_c_keywords_are_renamed() {
    let out = c("main :: proc() -> int32 {\n\
                     inline := 1;\n\
                     register := 2;\n\
                     return inline + register;\n\
                 }\n");
    assert!(out.contains("int32_t inline_ = 1"), "{out}");
    assert!(out.contains("int32_t register_ = 2"), "{out}");
    assert!(out.contains("return inline_ + register_"), "{out}");
}

#[test]
fn an_extern_symbol_is_never_renamed() {
    let out = c("free :: extern \"free\" proc(p: *int8) -> void;\n\
                 main :: proc() -> int32 { return 0; }\n");
    assert!(out.contains("free("), "{out}");
    assert!(!out.contains("free_("), "{out}");
}

#[test]
fn a_variadic_extern_declares_its_ellipsis() {
    let out = c(
        "printf :: extern \"printf\" proc(fmt: *cchar, ...) -> int32;\n\
                 main :: proc() -> int32 { printf(cast(*cchar) \"hi\\n\".ptr); return 0; }\n",
    );
    assert!(
        out.contains("extern int32_t printf(char * fmt, ...);"),
        "{out}"
    );
}

#[test]
fn a_non_variadic_extern_is_unchanged() {
    let out = c("puts :: extern \"puts\" proc(s: *cchar) -> int32;\n\
                 main :: proc() -> int32 { return 0; }\n");
    assert!(out.contains("puts("), "{out}");
    assert!(!out.contains("..."), "{out}");
}

#[test]
fn e2e_calling_a_variadic_c_function() {
    let src = "printf :: extern \"printf\" proc(fmt: *cchar, ...) -> int32;\n\
               main :: proc() -> int32 {\n\
                   printf(cast(*cchar) \"%d and %d\\n\".ptr, 40, 2);\n\
                   return 0;\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 0);
    }
}

#[test]
fn an_empty_struct_still_has_a_member() {
    let out = c("E :: struct { }\nmain :: proc() -> int32 { e := alloc E{}; return 0; }\n");
    assert!(out.contains("char _dray_empty;"), "{out}");
}
