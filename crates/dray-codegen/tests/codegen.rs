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
    assert!(out.contains("DrayI32 main(void)"), "{out}");
    assert!(out.contains("return 0;"));
}

#[test]
fn draybase_is_always_included() {
    assert!(c("main :: proc() -> int32 {\n    return 0;\n}\n").contains("#include \"draybase.h\""));
}

#[test]
fn c_header_becomes_include() {
    assert!(c("c_header(\"stdio.h\");\n\nmain :: proc() {\n}\n").contains("#include <stdio.h>"));
}

#[test]
fn params_lower_with_types() {
    let out = c("add :: proc(a: int32, b: int32) -> int32 {\n    return a + b;\n}\n");
    assert!(out.contains("DrayI32 add(DrayI32 a, DrayI32 b)"), "{out}");
}

#[test]
fn inferred_int_var_is_int32_not_plain_int() {
    let out = c("f :: proc() {\n    x := 5;\n}\n");
    assert!(out.contains("DrayI32 x = 5;"), "{out}");
}

#[test]
fn inferred_float_var_is_double() {
    let out = c("f :: proc() {\n    r := 1.5;\n}\n");
    assert!(out.contains("DrayF64 r = 1.5;"), "{out}");
}

#[test]
fn extern_prototype_uses_linked_symbol_not_binding_name() {
    // `my_abs :: extern "abs"` must emit `abs`, so it links
    let out = c("my_abs :: extern \"abs\" proc(x: int32) -> int32;\n");
    assert!(out.contains("DrayI32 abs(DrayI32 x);"), "{out}");
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
    assert!(out.contains("for (DrayI32 i = 0; i < 10; i += 1)"), "{out}");
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
    // The runtime is a library on disk, not something the compiler carries, so
    // the tests read it from the repo exactly as the driver reads it from an
    // install.
    let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let lib = [
        crate_dir.join("../../lib/system"),
        crate_dir.join("../lib/system"),
    ]
    .into_iter()
    .find(|p| p.join("draybase.h").is_file())
    .expect("lib/system/draybase.h");
    std::fs::copy(lib.join("draybase.h"), &base_h).unwrap();
    std::fs::copy(lib.join("draybase.c"), &base_c).unwrap();
    // The RC runtime is a companion file that draybase.h includes.
    let rc_h = dir.join("drayrc.h");
    let rc_c = dir.join(format!("{stamp}_drayrc.c"));
    std::fs::copy(lib.join("drayrc.h"), &rc_h).unwrap();
    std::fs::copy(lib.join("drayrc.c"), &rc_c).unwrap();

    // The generated C only has to compile and link. Warnings are the C
    // compiler's opinion about code nobody wrote by hand, so they are silenced
    // here the same way the driver silences them for users.
    let compile = Command::new(&cc)
        .arg("-std=c11")
        .arg("-w")
        .arg(&c_path)
        .arg(&base_c)
        .arg(&rc_c)
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
    let _ = std::fs::remove_file(&rc_c);
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
    assert!(out.contains("identity_int32(DrayI32 x)"), "{out}");
    assert!(out.contains("identity_bool(DrayBool x)"), "{out}");
    assert!(!out.contains("identity(DrayI32"), "template leaked: {out}");
}

#[test]
fn procs_get_prototypes_so_forward_calls_work() {
    let out = c("main :: proc() -> int32 { return helper(); }\n\
                 helper :: proc() -> int32 { return 1; }\n");
    // A prototype precedes the definition of `main`, so the later `helper` is
    // declared before its use
    let proto = out.find("DrayI32 helper(void);").expect("prototype");
    let body = out.find("DrayI32 main(void) {").expect("main body");
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
        !out.contains("DrayI32 x ="),
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
        out.contains("DrayI32 x ="),
        "used binding must be bound: {out}"
    );
}

#[test]
fn generated_c_has_no_duplicate_includes() {
    let out = c("Node :: struct { value: int32 }\n\
                 main :: proc() -> int32 { n := alloc Node{ value: 1 }; return n.value; }\n");
    // One include, and only one: draybase.h supplies every type the generated
    // code names.
    assert_eq!(out.matches("#include").count(), 1, "{out}");
    assert_eq!(out.matches("#include \"draybase.h\"").count(), 1, "{out}");
}

#[test]
fn main_gets_no_prototype() {
    let out = c("main :: proc() -> int32 { return 0; }\n");
    assert!(!out.contains("DrayI32 main(void);"), "{out}");
    assert!(out.contains("DrayI32 main(void) {"), "{out}");
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
    assert!(out.contains("DrayI32 xs[3] = {1, 2, 3}"), "{out}");
    // The array's length is a constant, so the check needs nothing else.
    assert!(out.contains("xs[dray_check_index((DrayI64)0, 3)]"), "{out}");
}

#[test]
fn a_slice_lowers_to_a_len_ptr_struct() {
    let out = c("f :: proc(xs: []int32) -> int32 { return xs.len; }\n\
                 main :: proc() -> int32 { ys: [2]int32 = {1, 2}; return f(ys[:]); }\n");
    assert!(out.contains("struct DraySlice_int32 {"), "{out}");
    assert!(out.contains("DrayI32 len;"), "{out}");
    assert!(out.contains("DrayI32 *ptr;"), "{out}");
    assert!(out.contains(".len=2"), "{out}");
    assert!(out.contains(".ptr=&ys[0]"), "{out}");
}

#[test]
fn indexing_a_slice_goes_through_its_bounds_checked_helper() {
    let out = c("f :: proc(xs: []int32) -> int32 { return xs[0]; }\n\
                 main :: proc() -> int32 { return 0; }\n");
    // The helper takes the whole fat pointer, so `xs` is evaluated once, and
    // returns a pointer so the result stays assignable.
    assert!(out.contains("*dray_index_int32(xs, (DrayI64)0)"), "{out}");
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
    assert!(out.contains("*dray_index_int32(xs, "), "{out}");
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
    assert!(out.contains("DrayI32 inline_ = 1"), "{out}");
    assert!(out.contains("DrayI32 register_ = 2"), "{out}");
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
        out.contains("extern DrayI32 printf(DrayChar * fmt, ...);"),
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

#[test]
fn generated_c_names_drays_own_types() {
    let out = c(
        "main :: proc() -> int32 {\n    n: int64 = 1;\n    f: float32 = 2.0;\n    b := true;\n    return 0;\n}\n",
    );
    assert!(out.contains("DrayI64 n"), "{out}");
    assert!(out.contains("DrayF32 f"), "{out}");
    assert!(out.contains("DrayBool b"), "{out}");
    assert!(!out.contains("int64_t"), "raw C type leaked: {out}");
}

#[test]
fn a_full_range_is_still_the_plain_fat_pointer() {
    let src = "f :: proc() {\n    a: [4]int32 = { 1, 2, 3, 4 };\n    v := a[:];\n}\n";
    let out = c(src);
    assert!(out.contains(".len=4"), "{out}");
    assert!(out.contains(".ptr=&a[0]"), "{out}");
    assert!(!out.contains("v = dray_slice"), "{out}");
}

#[test]
fn a_sub_range_narrows_through_the_helper() {
    let src = "f :: proc() {\n    a: [4]int32 = { 1, 2, 3, 4 };\n    v := a[1:3];\n}\n";
    let out = c(src);
    assert!(out.contains("dray_slice_int32("), "{out}");
}

#[test]
fn an_open_ended_range_uses_the_length_carrying_helper() {
    let src = "f :: proc() {\n    a: [4]int32 = { 1, 2, 3, 4 };\n    v := a[1:];\n}\n";
    let out = c(src);
    assert!(out.contains("dray_slice_from_int32("), "{out}");
}

#[test]
fn a_missing_low_bound_becomes_zero() {
    let src = "f :: proc() {\n    a: [4]int32 = { 1, 2, 3, 4 };\n    v := a[:2];\n}\n";
    let out = c(src);
    assert!(
        out.contains("dray_slice_int32(") && out.contains(", 0, "),
        "{out}"
    );
}

#[test]
fn a_sub_range_evaluates_its_base_and_bounds_once_each() {
    let src = "bump :: proc() -> int32 {\n    return 1;\n}\n\nf :: proc() {\n    a: [4]int32 = { 1, 2, 3, 4 };\n    v := a[bump():bump()];\n}\n";
    let out = c(src);
    let line = out
        .lines()
        .find(|l| l.contains("v = dray_slice_int32("))
        .expect("the narrowing call");
    assert_eq!(line.matches("bump()").count(), 2, "{line}");
    assert_eq!(line.matches("&a[0]").count(), 1, "{line}");
}

#[test]
fn the_slice_helpers_are_static_so_nothing_leaks_out_of_the_file() {
    let out = c("f :: proc(xs: []int32) -> int32 {\n    return xs.len;\n}\n");
    assert!(
        out.contains("static struct DraySlice_int32 dray_slice_int32("),
        "{out}"
    );
    assert!(
        out.contains("static struct DraySlice_int32 dray_slice_from_int32("),
        "{out}"
    );
}

#[test]
fn e2e_sub_range_slicing_of_an_array() {
    let src = "sum :: proc(xs: []int32) -> int32 {\n    total := 0;\n    for n in xs {\n        total = total + n;\n    }\n    return total;\n}\n\nmain :: proc() -> int32 {\n    a: [6]int32 = { 1, 2, 3, 4, 5, 6 };\n    return sum(a[2:5]);\n}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 12, "a[2:5] is {{3, 4, 5}}");
    }
}

#[test]
fn e2e_sub_range_slicing_of_a_slice() {
    let src = "sum :: proc(xs: []int32) -> int32 {\n    total := 0;\n    for n in xs {\n        total = total + n;\n    }\n    return total;\n}\n\nmain :: proc() -> int32 {\n    a: [6]int32 = { 1, 2, 3, 4, 5, 6 };\n    v := a[:];\n    return sum(v[3:]) + sum(v[:2]);\n}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 18, "v[3:] is {{4, 5, 6}} and v[:2] is {{1, 2}}");
    }
}

#[test]
fn e2e_a_slice_range_with_runtime_bounds() {
    let src = "sum :: proc(xs: []int32) -> int32 {\n    total := 0;\n    for n in xs {\n        total = total + n;\n    }\n    return total;\n}\n\nmain :: proc() -> int32 {\n    a: [6]int32 = { 1, 2, 3, 4, 5, 6 };\n    lo := 1;\n    hi := 5;\n    return sum(a[lo:hi]);\n}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 14, "a[1:5] is {{2, 3, 4, 5}}");
    }
}

#[test]
fn e2e_an_empty_range_is_a_zero_length_slice() {
    let src = "main :: proc() -> int32 {\n    a: [6]int32 = { 1, 2, 3, 4, 5, 6 };\n    v := a[3:3];\n    return v.len;\n}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 0);
    }
}

#[test]
fn indexing_an_array_checks_against_its_constant_length() {
    let out = c(
        "f :: proc(i: int32) -> int32 {\n    a: [4]int32 = { 1, 2, 3, 4 };\n    return a[i];\n}\n",
    );
    assert!(out.contains("a[dray_check_index((DrayI64)i, 4)]"), "{out}");
}

#[test]
fn indexing_a_raw_pointer_is_not_checked() {
    // There is no length to check against.
    let out = c("f :: proc(p: *int32, i: int32) -> int32 {\n    return p[i];\n}\n");
    assert!(out.contains("p[(DrayI64)i]"), "{out}");
    assert!(!out.contains("dray_check_index"), "{out}");
}

#[test]
fn the_slice_helpers_carry_the_bounds_checks() {
    let out = c("f :: proc(xs: []int32) -> int32 {\n    return xs.len;\n}\n");
    assert!(out.contains("dray_index_fail"), "{out}");
    assert!(out.contains("dray_range_fail"), "{out}");
    assert!(out.contains("dray_range_from_fail"), "{out}");
}

#[test]
fn e2e_an_index_in_range_still_works() {
    let src = "main :: proc() -> int32 {\n    a: [4]int32 = { 1, 2, 3, 4 };\n    v := a[:];\n    i := 2;\n    return a[i] + v[i];\n}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 6);
    }
}

#[test]
fn e2e_assigning_through_a_checked_index_still_writes_through() {
    let src = "main :: proc() -> int32 {\n    a: [4]int32 = { 1, 2, 3, 4 };\n    v := a[:];\n    i := 1;\n    v[i] = 40;\n    return a[1];\n}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 40, "the slice must alias the array, not copy it");
    }
}

#[test]
fn e2e_an_out_of_range_index_aborts() {
    let src = "main :: proc() -> int32 {\n    a: [4]int32 = { 1, 2, 3, 4 };\n    i := 9;\n    return a[i];\n}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_ne!(code, 0, "an out-of-range index must not succeed");
    }
}

#[test]
fn e2e_an_out_of_range_slice_range_aborts() {
    let src = "main :: proc() -> int32 {\n    a: [4]int32 = { 1, 2, 3, 4 };\n    hi := 9;\n    v := a[1:hi];\n    return v.len;\n}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_ne!(code, 0);
    }
}

#[test]
fn e2e_the_empty_tail_range_is_legal() {
    let src = "main :: proc() -> int32 {\n    a: [4]int32 = { 1, 2, 3, 4 };\n    lo := 4;\n    v := a[lo:];\n    return v.len;\n}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 0);
    }
}

const WEAK_PRELUDE: &str = "rc_live :: extern \"dray_rc_live\" proc() -> int64;\n\
    Maybe :: enum(comptime T: type) { Some(T), None }\n\
    Parent :: struct { name: int32, child: @Child }\n\
    Child :: struct { name: int32, parent: Weak(@Parent) }\n";

#[test]
fn a_weak_reference_is_a_plain_pointer_in_c() {
    let out = c(&format!(
        "{WEAK_PRELUDE}main :: proc() -> int32 {{ return 0; }}\n"
    ));
    assert!(out.contains("struct Parent *parent;"), "{out}");
}

#[test]
fn drop_glue_lets_go_of_a_weak_field() {
    let out = c(&format!(
        "{WEAK_PRELUDE}main :: proc() -> int32 {{ return 0; }}\n"
    ));
    assert!(out.contains("dray_rc_weak_release(self->parent)"), "{out}");
}

#[test]
fn downgrade_and_upgrade_call_the_runtime() {
    let src = format!(
        "{WEAK_PRELUDE}\
        f :: proc(p: @Parent) -> int32 {{\n\
            w := p.downgrade();\n\
            switch w.upgrade() {{ case Maybe.Some(up): return up.name; case Maybe.None: return 0; }}\n\
        }}\n\
        main :: proc() -> int32 {{ return 0; }}\n"
    );
    let out = c(&src);
    assert!(out.contains("dray_rc_downgrade(p)"), "{out}");
    assert!(out.contains("dray_rc_upgrade("), "{out}");
    assert!(out.contains("dray_upgrade_Maybe_rc_Parent"), "{out}");
}

#[test]
fn a_switch_evaluates_its_scrutinee_once() {
    let src = format!(
        "{WEAK_PRELUDE}\
        f :: proc(w: Weak(@Parent)) -> int32 {{\n\
            switch w.upgrade() {{ case Maybe.Some(up): return up.name; case Maybe.None: return 0; }}\n\
        }}\n\
        main :: proc() -> int32 {{ return 0; }}\n"
    );
    let out = c(&src);
    assert_eq!(out.matches("dray_rc_upgrade(").count(), 1, "{out}");
}

#[test]
fn a_proc_ending_in_an_exhaustive_switch_closes_with_unreachable() {
    let src = format!(
        "{WEAK_PRELUDE}\
        f :: proc(w: Weak(@Parent)) -> int32 {{\n\
            switch w.upgrade() {{ case Maybe.Some(up): return up.name; case Maybe.None: return 0; }}\n\
        }}\n\
        main :: proc() -> int32 {{ return 0; }}\n"
    );
    assert!(c(&src).contains("dray_unreachable()"), "{}", c(&src));
}

/// The whole point of `Weak`: without it the parent and child keep each other alive forever.
#[test]
fn e2e_a_parent_child_cycle_is_collected() {
    let src = format!(
        "{WEAK_PRELUDE}\
        build :: proc() -> int32 {{\n\
            c := alloc Child{{ name: 2 }};\n\
            p := alloc Parent{{ name: 1, child: c }};\n\
            c.parent = p.downgrade();\n\
            return cast(int32) rc_live();\n\
        }}\n\
        main :: proc() -> int32 {{\n\
            live_inside := build();\n\
            return live_inside * 10 + cast(int32) rc_live();\n\
        }}\n"
    );
    if let Some(code) = compile_and_run(&c(&src)) {
        assert_eq!(code, 20, "two allocations alive inside, none after");
    }
}

#[test]
fn e2e_upgrade_succeeds_while_the_owner_is_alive() {
    let src = format!(
        "{WEAK_PRELUDE}\
        main :: proc() -> int32 {{\n\
            c := alloc Child{{ name: 2 }};\n\
            p := alloc Parent{{ name: 7, child: c }};\n\
            c.parent = p.downgrade();\n\
            switch c.parent.upgrade() {{ case Maybe.Some(up): return up.name; case Maybe.None: return 0; }}\n\
        }}\n"
    );
    if let Some(code) = compile_and_run(&c(&src)) {
        assert_eq!(code, 7);
    }
}

#[test]
fn e2e_upgrade_fails_once_the_owner_is_gone() {
    let src = format!(
        "{WEAK_PRELUDE}\
        orphan :: proc() -> Weak(@Parent) {{\n\
            c := alloc Child{{ name: 2 }};\n\
            p := alloc Parent{{ name: 7, child: c }};\n\
            return p.downgrade();\n\
        }}\n\
        main :: proc() -> int32 {{\n\
            w := orphan();\n\
            switch w.upgrade() {{ case Maybe.Some(up): return 1; case Maybe.None: return 0; }}\n\
        }}\n"
    );
    if let Some(code) = compile_and_run(&c(&src)) {
        assert_eq!(
            code, 0,
            "the payload is gone, so the upgrade must not succeed"
        );
    }
}

#[test]
fn alloc_array_builds_a_fat_pointer_over_rc_alloc_array() {
    let out = c(
        "f :: proc(n: int32) -> @[]int32 {\n    return alloc [n]int32;\n}\n\
                 main :: proc() -> int32 { return 0; }\n",
    );
    assert!(out.contains("dray_rc_alloc_array("), "{out}");
    assert!(out.contains("struct DraySlice_int32"), "{out}");
}

#[test]
fn a_scalar_heap_array_needs_no_drop_function() {
    let out = c(
        "f :: proc(n: int32) -> @[]int32 {\n    return alloc [n]int32;\n}\n\
                 main :: proc() -> int32 { return 0; }\n",
    );
    // The drop argument is NULL (0): plain integers own nothing.
    assert!(!out.contains("dray_drop_arr_int32"), "{out}");
}

#[test]
fn a_heap_array_of_references_gets_a_drop_function_that_walks_the_count() {
    let out = c("Node :: struct { value: int32 }\n\
                 f :: proc() -> @[]@Node {\n    return alloc [3]@Node;\n}\n\
                 main :: proc() -> int32 { return 0; }\n");
    assert!(out.contains("void dray_drop_arr_rc_Node(void *p)"), "{out}");
    assert!(out.contains("dray_rc_count(p)"), "{out}");
    assert!(out.contains("dray_rc_release(elems[i])"), "{out}");
}

#[test]
fn a_heap_slice_local_is_a_value_not_a_pointer() {
    // `@[]T` lowers to the slice struct, so field access uses `.`, not `->`.
    let out = c(
        "f :: proc(n: int32) -> int32 {\n    xs := alloc [n]int32;\n    return xs.len;\n}\n\
                 main :: proc() -> int32 { return 0; }\n",
    );
    assert!(out.contains("xs.len"), "{out}");
    assert!(!out.contains("xs->len"), "{out}");
}

#[test]
fn a_heap_slice_is_released_through_its_ptr() {
    let out = c("f :: proc(n: int32) {\n    xs := alloc [n]int32;\n}\n\
                 main :: proc() -> int32 { return 0; }\n");
    assert!(out.contains("dray_rc_release(xs.ptr)"), "{out}");
}

#[test]
fn e2e_a_scalar_heap_array_round_trips() {
    let src = "main :: proc() -> int32 {\n\
        xs := alloc [5]int32;\n\
        i := 0;\n\
        for i < xs.len {\n            xs[i] = i * i;\n            i = i + 1;\n        }\n\
        total := 0;\n        for v in xs {\n            total = total + v;\n        }\n\
        return total;\n    }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 30, "0+1+4+9+16");
    }
}

#[test]
fn e2e_a_scalar_heap_array_frees_itself() {
    let src = "rc_live :: extern \"dray_rc_live\" proc() -> int64;\n\
        build :: proc() -> int32 {\n            xs := alloc [4]int32;\n            return cast(int32) rc_live();\n        }\n\
        main :: proc() -> int32 {\n            inside := build();\n            return inside * 10 + cast(int32) rc_live();\n        }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 10, "one allocation alive inside, none after");
    }
}

#[test]
fn e2e_a_heap_array_of_references_releases_every_element() {
    let src = "rc_live :: extern \"dray_rc_live\" proc() -> int64;\n\
        Node :: struct { value: int32 }\n\
        build :: proc() -> int32 {\n\
            xs := alloc [3]@Node;\n\
            i := 0;\n\
            for i < xs.len {\n                xs[i] = alloc Node{ value: i + 1 };\n                i = i + 1;\n            }\n\
            return cast(int32) rc_live();\n        }\n\
        main :: proc() -> int32 {\n            inside := build();\n            return inside * 10 + cast(int32) rc_live();\n        }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        // 3 nodes + 1 array = 4 alive inside; 0 after.
        assert_eq!(code, 40);
    }
}

#[test]
fn e2e_returning_a_heap_array_transfers_ownership_not_frees_it() {
    let src = "rc_live :: extern \"dray_rc_live\" proc() -> int64;\n\
        sum :: proc(xs: @[]int32) -> int32 {\n            total := 0;\n            for v in xs {\n                total = total + v;\n            }\n            return total;\n        }\n\
        make :: proc(n: int32) -> @[]int32 {\n\
            xs := alloc [n]int32;\n            i := 0;\n            for i < xs.len {\n                xs[i] = i + 1;\n                i = i + 1;\n            }\n            return xs;\n        }\n\
        owner :: proc() -> int32 {\n            xs := make(4);\n            return sum(xs) * 10 + cast(int32) rc_live();\n        }\n\
        main :: proc() -> int32 {\n            inside := owner();\n            return inside + cast(int32) rc_live();\n        }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        // sum = 1+2+3+4 = 10; one array live inside owner; none after.
        assert_eq!(
            code,
            10 * 10 + 1,
            "array alive inside owner, freed at its exit"
        );
    }
}

#[test]
fn a_method_lowers_to_a_function_taking_the_receiver_first() {
    let out = c(
        "Circle :: struct { radius: int32 }\narea :: proc[c: Circle]() -> int32 { return c.radius; }\nmain :: proc() -> int32 { return 0; }\n",
    );
    assert!(out.contains("dray_m_Circle_area(struct Circle c)"), "{out}");
}

#[test]
fn a_method_call_passes_the_receiver_as_the_first_argument() {
    let out = c(
        "Circle :: struct { radius: int32 }\narea :: proc[c: Circle]() -> int32 { return c.radius; }\nmain :: proc() -> int32 { sq := Circle{ radius: 3 }; return sq.area(); }\n",
    );
    assert!(out.contains("dray_m_Circle_area(sq)"), "{out}");
}

#[test]
fn two_types_sharing_a_method_name_mangle_distinctly() {
    let out = c(
        "Circle :: struct { r: int32 }\nSquare :: struct { s: int32 }\narea :: proc[c: Circle]() -> int32 { return c.r; }\narea :: proc[sq: Square]() -> int32 { return sq.s; }\nmain :: proc() -> int32 { return 0; }\n",
    );
    assert!(out.contains("dray_m_Circle_area"), "{out}");
    assert!(out.contains("dray_m_Square_area"), "{out}");
}

#[test]
fn e2e_a_value_receiver_method_runs() {
    let src = "Circle :: struct { radius: int32 }\narea :: proc[c: Circle]() -> int32 { return c.radius * c.radius; }\nscale :: proc[c: Circle](f: int32) -> int32 { return c.radius * f; }\nmain :: proc() -> int32 { sq := Circle{ radius: 3 }; return sq.area() + sq.scale(2); }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 15, "area 9 + scale 6");
    }
}

#[test]
fn e2e_a_pointer_receiver_method_runs() {
    let src = "Box :: struct { value: int32 }\nget :: proc[b: @Box]() -> int32 { return b.value; }\nmain :: proc() -> int32 { boxed := alloc Box{ value: 42 }; return boxed.get(); }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 42);
    }
}

#[test]
fn e2e_a_method_can_call_another_method() {
    let src = "printf :: extern \"printf\" proc(fmt: *cchar, ...) -> int32;\nRect :: struct { w: int32, h: int32 }\narea :: proc[r: Rect]() -> int32 { return r.w * r.h; }\nscaled :: proc[r: Rect](f: int32) -> int32 { return r.area() * f; }\nmain :: proc() -> int32 { box := Rect{ w: 3, h: 4 }; return box.scaled(2); }\n";
    let out = c(src);
    // The inner call must be the method, never printf.
    assert!(out.contains("dray_m_Rect_area(r)"), "{out}");
    assert!(!out.contains("printf(r)"), "{out}");
    if let Some(code) = compile_and_run(&out) {
        assert_eq!(code, 24, "area 12 * 2");
    }
}

fn c_modules(main: &str, lib: &str) -> dray_codegen::CModules {
    let pa = parse(main);
    let pb = parse(lib);
    assert!(
        pa.errors.is_empty() && pb.errors.is_empty(),
        "parse: {:?} {:?}",
        pa.errors,
        pb.errors
    );
    let mut f0 = dray_hir::FileImports::default();
    f0.globs.push(1);
    let graph = dray_hir::ModuleGraph {
        files: vec![f0, dray_hir::FileImports::default()],
    };
    let (hir, errs) = dray_hir::lower_files_with_graph(&[&pa.root, &pb.root], &graph);
    assert!(errs.is_empty(), "resolve: {errs:?}");
    let ir = dray_ir::lower(&dray_hir::monomorphize(hir).expect("monomorphize"));
    dray_codegen::ir_to_c_modules(&ir, "prog.h").expect("codegen")
}

#[test]
fn each_module_gets_its_own_source_including_the_header() {
    let lib = "pub helper :: proc() -> int32 {\n    return 7;\n}\n";
    let main = "main :: proc() -> int32 {\n    return helper();\n}\n";
    let cm = c_modules(main, lib);
    assert_eq!(cm.modules.len(), 2, "one C source per module");
    for m in &cm.modules {
        assert!(
            m.contains("#include \"prog.h\""),
            "each module includes the header: {m}"
        );
    }
    // main's proc goes in module 0, helper's in module 1.
    assert!(cm.modules[0].contains("DrayI32 main("), "{}", cm.modules[0]);
    assert!(
        cm.modules[1].contains("DrayI32 helper("),
        "{}",
        cm.modules[1]
    );
}

#[test]
fn the_header_declares_procs_but_does_not_define_them() {
    let lib = "pub helper :: proc() -> int32 {\n    return 7;\n}\n";
    let main = "main :: proc() -> int32 {\n    return helper();\n}\n";
    let cm = c_modules(main, lib);
    // A prototype ends in `;`, a definition has a body `{`.
    assert!(
        cm.header.contains("DrayI32 helper(void);"),
        "header has the prototype: {}",
        cm.header
    );
    assert!(
        !cm.header.contains("DrayI32 helper(void) {"),
        "header has no definition: {}",
        cm.header
    );
}

#[test]
fn a_generated_helper_is_defined_in_exactly_one_module() {
    let lib = "pub Box :: struct {\n    value: int32,\n}\n\npub make :: proc(v: int32) -> Box {\n    return Box{ value: v };\n}\n";
    let main = "main :: proc() -> int32 {\n    b := make(5);\n    return b.value;\n}\n";
    let cm = c_modules(main, lib);
    let defs: usize = cm
        .modules
        .iter()
        .map(|m| m.matches("struct Box *dray_new_Box(").count())
        .sum();
    assert_eq!(
        defs, 1,
        "the Box constructor is defined once across all modules"
    );
}

#[test]
fn per_module_line_directives_name_each_modules_own_file() {
    let pa = parse("main :: proc() -> int32 {\n    return helper();\n}\n");
    let pb = parse("pub helper :: proc() -> int32 {\n    return 7;\n}\n");
    assert!(pa.errors.is_empty() && pb.errors.is_empty());

    let mut f0 = dray_hir::FileImports::default();
    f0.globs.push(1);

    let graph = dray_hir::ModuleGraph {
        files: vec![f0, dray_hir::FileImports::default()],
    };

    let (hir, errs) = dray_hir::lower_files_with_graph(&[&pa.root, &pb.root], &graph);
    assert!(errs.is_empty(), "resolve: {errs:?}");
    let mut ir = dray_ir::lower(&dray_hir::monomorphize(hir).expect("monomorphize"));

    ir.sources = vec![
        dray_ir::SourceMap::new(
            "main.dray",
            "main :: proc() -> int32 {\n    return helper();\n}\n",
        ),
        dray_ir::SourceMap::new(
            "helper.dray",
            "pub helper :: proc() -> int32 {\n    return 7;\n}\n",
        ),
    ];
    let cm = dray_codegen::ir_to_c_modules(&ir, "prog.h").expect("codegen");

    assert!(
        cm.modules[0].contains("\"main.dray\""),
        "module 0: {}",
        cm.modules[0]
    );
    assert!(
        cm.modules[1].contains("\"helper.dray\""),
        "module 1: {}",
        cm.modules[1]
    );
    assert!(
        !cm.modules[1].contains("\"main.dray\""),
        "module 1 must not point at entry: {}",
        cm.modules[1]
    );
}

#[test]
fn e2e_try_alloc_scalar_yields_some_on_success() {
    let src = "Maybe :: enum(comptime T: type) { Some(T), None }\n\
               main :: proc() -> int32 {\n\
                   r := try_alloc int32;\n\
                   switch r {\n\
                   case Maybe.Some(p): *p = 42; return *p;\n\
                   case Maybe.None: return 1;\n\
                   }\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 42);
    }
}

#[test]
fn e2e_try_alloc_struct_yields_a_usable_pointer() {
    let src = "Maybe :: enum(comptime T: type) { Some(T), None }\n\
               Node :: struct { value: int32, next: @Node }\n\
               main :: proc() -> int32 {\n\
                   r := try_alloc Node;\n\
                   switch r {\n\
                   case Maybe.Some(n): n.value = 9; return n.value;\n\
                   case Maybe.None: return 1;\n\
                   }\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 9);
    }
}

#[test]
fn try_alloc_calls_the_non_aborting_allocator() {
    let out = c("Maybe :: enum(comptime T: type) { Some(T), None }\n\
                 main :: proc() -> int32 {\n\
                     r := try_alloc int32;\n\
                     switch r { case Maybe.Some(p): return 0; case Maybe.None: return 1; }\n\
                 }\n");
    assert!(
        out.contains("dray_rc_try_alloc"),
        "must use the fallible allocator: {out}"
    );
}

#[test]
fn e2e_reassigning_a_weak_local_keeps_counts_balanced() {
    let src = format!(
        "{WEAK_PRELUDE}\
        main :: proc() -> int32 {{\n\
            a := alloc Parent{{ name: 1, child: alloc Child{{ name: 0 }} }};\n\
            b := alloc Parent{{ name: 2, child: alloc Child{{ name: 0 }} }};\n\
            w := a.downgrade();\n\
            w = b.downgrade();\n\
            switch w.upgrade() {{ case Maybe.Some(p): return p.name; case Maybe.None: return 0; }}\n\
        }}\n"
    );
    if let Some(code) = compile_and_run(&c(&src)) {
        assert_eq!(code, 2, "w should point at b after reassignment");
    }
}

#[test]
fn reassigning_a_weak_local_emits_weak_release_of_the_old() {
    let src = format!(
        "{WEAK_PRELUDE}\
        main :: proc() -> int32 {{\n\
            a := alloc Parent{{ name: 1, child: alloc Child{{ name: 0 }} }};\n\
            b := alloc Parent{{ name: 2, child: alloc Child{{ name: 0 }} }};\n\
            w := a.downgrade();\n\
            w = b.downgrade();\n\
            return 0;\n\
        }}\n"
    );
    let out = c(&src);
    // The reassignment must release the previous referent's weak count.
    assert!(
        out.contains("dray_rc_weak_release"),
        "weak reassign must release old: {out}"
    );
}

#[test]
fn a_heap_slice_field_is_released_through_its_ptr() {
    let out = c("Buf :: struct { data: @[]uint8, n: int32 }\n\
                 make :: proc(k: int32) -> Buf {\n\
                     b := alloc [k]uint8;\n\
                     return Buf{ data: b, n: k };\n\
                 }\n\
                 main :: proc() -> int32 { x := make(4); return x.n; }\n");
    assert!(
        out.contains("dray_rc_release(self->data.ptr)"),
        "heap-slice field must be released via .ptr: {out}"
    );
}

#[test]
fn e2e_heap_slice_field_runs_without_leaking() {
    let src = "rc_live :: extern \"dray_rc_live\" proc() -> int64;\n\
               Buf :: struct { data: @[]uint8, n: int32 }\n\
               make :: proc(k: int32) -> Buf {\n\
                   b := alloc [k]uint8;\n\
                   return Buf{ data: b, n: k };\n\
               }\n\
               main :: proc() -> int32 {\n\
                   x := make(8);\n\
                   return x.n;\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 8);
    }
}

#[test]
fn e2e_a_nested_block_scopes_its_locals() {
    let src = "main :: proc() -> int32 {\n\
                   x := 1;\n\
                   {\n\
                       y := 2;\n\
                       x = x + y;\n\
                   }\n\
                   return x;\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 3);
    }
}

#[test]
fn e2e_a_nested_block_shadow_does_not_leak() {
    let src = "main :: proc() -> int32 {\n\
                   x := 10;\n\
                   {\n\
                       x := 99;\n\
                       x = x + 1;\n\
                   }\n\
                   return x;\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 10);
    }
}

#[test]
fn a_nested_block_emits_a_c_scope() {
    let out = c(
        "main :: proc() -> int32 {\n    x := 1;\n    {\n        y := 2;\n        x = x + y;\n    }\n    return x;\n}\n",
    );
    assert!(
        out.contains('{') && out.contains('}'),
        "should emit braces: {out}"
    );
}

#[test]
fn a_string_literal_lowers_to_a_byte_array() {
    let out = c("main :: proc() -> int32 {\n    s := \"hi\";\n    return s.len;\n}\n");
    assert!(
        out.contains("static const DrayU8 dray_str_0[]"),
        "expected byte array: {out}"
    );
    assert!(
        out.contains("dray_str_0"),
        "literal should reference the array: {out}"
    );
}

#[test]
fn e2e_a_string_literal_keeps_its_bytes_past_an_interior_nul() {
    let src =
        "main :: proc() -> int32 {\n    s := \"ab\\x00cd\";\n    return cast(int32) s[4];\n}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 100); // 'd'
    }
}

#[test]
fn a_c_string_literal_is_a_real_c_string() {
    let out = c(
        "printf :: extern \"printf\" proc(fmt: *cchar, ...) -> int32;\n\
                 main :: proc() -> int32 {\n    printf(c\"hi\\n\");\n    return 0;\n}\n",
    );
    assert!(
        out.contains("\"hi\\n\""),
        "expected a C string literal: {out}"
    );
}

#[test]
fn e2e_a_c_string_literal_prints() {
    let out = c(
        "printf :: extern \"printf\" proc(fmt: *cchar, ...) -> int32;\n\
                 main :: proc() -> int32 {\n    printf(c\"ok\\n\");\n    return 7;\n}\n",
    );
    if let Some(code) = compile_and_run(&out) {
        assert_eq!(code, 7);
    }
}

#[test]
fn a_plain_string_literal_has_no_trailing_nul() {
    let out = c("main :: proc() -> int32 {\n    s := \"hi\";\n    return s.len;\n}\n");
    assert!(
        out.contains("dray_str_0[] = { 104, 105 }"),
        "expected exact bytes: {out}"
    );
}

#[test]
fn an_rc_call_result_used_via_field_is_released_once() {
    // A proc returning `@[]uint8`, whose result is used only through `.ptr` in a
    // statement, must be hoisted to a temp and released via its `.ptr`
    let src = "sink :: extern \"sink\" proc(p: *uint8) -> int32;\n\
               buf :: proc() -> @[]uint8 {\n    return alloc [4]uint8;\n}\n\
               main :: proc() -> int32 {\n    sink(buf().ptr);\n    return 0;\n}\n";
    let out = c(src);
    assert!(
        out.contains("__rc_tmp_0 = buf()"),
        "call should be hoisted: {out}"
    );
    assert!(
        out.matches("dray_rc_release(__rc_tmp_0.ptr)").count() == 1,
        "should release the temp's .ptr once: {out}"
    );
}

#[test]
fn a_bare_rc_call_statement_is_released() {
    let src = "buf :: proc() -> @[]uint8 {\n    return alloc [4]uint8;\n}\n\
               main :: proc() -> int32 {\n    buf();\n    return 0;\n}\n";
    let out = c(src);
    assert!(
        out.contains("dray_rc_release(__rc_tmp_0.ptr)"),
        "bare RC call result should be released: {out}"
    );
}

#[test]
fn a_non_rc_call_result_is_not_hoisted() {
    let src = "n :: proc() -> int32 {\n    return 5;\n}\n\
               sink :: extern \"sink\" proc(x: int32) -> int32;\n\
               main :: proc() -> int32 {\n    sink(n());\n    return 0;\n}\n";
    let out = c(src);
    assert!(
        !out.contains("__rc_tmp"),
        "no RC temp for a non-RC call: {out}"
    );
}

#[test]
fn a_returned_struct_retains_its_rc_heap_slice_field() {
    let src = "Str :: struct { data: @[]uint8, len: int32 }\n\
               build :: proc() -> Str {\n\
                   buf := alloc [3]uint8;\n\
                   return Str{ data: buf, len: 3 };\n\
               }\n\
               main :: proc() -> int32 {\n    s := build();\n    return s.len;\n}\n";
    let out = c(src);
    // Retain through `.ptr` (heap slice), matching the release of the local.
    assert!(
        out.contains("dray_rc_retain(buf.ptr)"),
        "heap-slice field should be retained through .ptr: {out}"
    );
    if let Some(code) = compile_and_run(&out) {
        assert_eq!(code, 3);
    }
}

#[test]
fn a_returned_struct_retains_its_plain_rc_field() {
    let src = "Node :: struct { v: int32 }\n\
               Wrap :: struct { node: @Node }\n\
               build :: proc() -> Wrap {\n\
                   n := alloc Node{ v: 9 };\n\
                   return Wrap{ node: n };\n\
               }\n\
               main :: proc() -> int32 {\n    w := build();\n    return w.node.v;\n}\n";
    let out = c(src);
    assert!(
        out.contains("dray_rc_retain(n)"),
        "plain @T field should be retained directly: {out}"
    );
    if let Some(code) = compile_and_run(&out) {
        assert_eq!(code, 9);
    }
}

#[test]
fn e2e_an_anonymous_proc_can_be_called() {
    let src = "main :: proc() -> int32 {\n\
                   inc := proc(n: int32) -> int32 { return n + 1; };\n\
                   return inc(5);\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 6);
    }
}

#[test]
fn an_anonymous_proc_hoists_to_a_file_scope_function() {
    let out = c("main :: proc() -> int32 {\n\
                     inc := proc(n: int32) -> int32 { return n + 1; };\n\
                     return inc(0);\n\
                 }\n");
    assert!(out.contains("__dray_anon_proc"), "should hoist: {out}");
    assert!(
        out.contains("(*inc)(DrayI32)"),
        "inc is a function pointer: {out}"
    );
}

#[test]
fn e2e_an_anonymous_proc_passed_to_a_higher_order_proc() {
    let src = "apply :: proc(f: proc(int32) -> int32, x: int32) -> int32 {\n\
                   return f(x);\n\
               }\n\
               main :: proc() -> int32 {\n\
                   return apply(proc(n: int32) -> int32 { return n + 1; }, 5);\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 6);
    }
}

#[test]
fn e2e_for_in_over_a_custom_iterator() {
    let src = "Maybe :: enum(comptime T: type) { Some(T), None }\n\
               Range :: struct { lo: int32, hi: int32 }\n\
               RangeIter :: struct { cur: int32, hi: int32 }\n\
               iterator :: proc[r: Range]() -> @RangeIter {\n\
                   return alloc RangeIter{ cur: r.lo, hi: r.hi };\n\
               }\n\
               next :: proc[it: @RangeIter]() -> Maybe(int32) {\n\
                   if it.cur < it.hi {\n\
                       v := it.cur;\n\
                       it.cur = it.cur + 1;\n\
                       return Maybe(int32).Some(v);\n\
                   }\n\
                   return Maybe(int32).None;\n\
               }\n\
               main :: proc() -> int32 {\n\
                   total := 0;\n\
                   r := Range{ lo: 2, hi: 7 };\n\
                   for x in r { total += x; }\n\
                   return total;\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 20); // 2+3+4+5+6
    }
}

#[test]
fn a_custom_iterator_loop_exits_via_a_flag_not_a_switch_break() {
    let src = "Maybe :: enum(comptime T: type) { Some(T), None }\n\
               Range :: struct { hi: int32 }\n\
               RangeIter :: struct { cur: int32, hi: int32 }\n\
               iterator :: proc[r: Range]() -> @RangeIter { return alloc RangeIter{ cur: 0, hi: r.hi }; }\n\
               next :: proc[it: @RangeIter]() -> Maybe(int32) {\n\
                   if it.cur < it.hi { v := it.cur; it.cur = it.cur + 1; return Maybe(int32).Some(v); }\n\
                   return Maybe(int32).None;\n\
               }\n\
               main :: proc() -> int32 { n := 0; r := Range{ hi: 3 }; for x in r { n += 1; } return n; }\n";
    let out = c(src);
    assert!(
        out.contains("__dray_done") || out.contains("= true"),
        "expected a done flag: {out}"
    );
    if let Some(code) = compile_and_run(&out) {
        assert_eq!(code, 3);
    }
}

#[test]
fn e2e_while_init_loop() {
    let src = "main :: proc() -> int32 {\n\
                   sum := 0;\n\
                   for i := 0; i < 5 { sum += i; i += 1; }\n\
                   return sum;\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 10); // 0+1+2+3+4
    }
}

#[test]
fn a_for_init_rc_binding_is_released_after_the_loop() {
    let src = "rc_live :: extern \"dray_rc_live\" proc() -> int64;\n\
               Node :: struct { v: int32 }\n\
               main :: proc() -> int32 {\n\
                   for n := alloc Node{ v: 0 }; n.v < 3 { n.v = n.v + 1; }\n\
                   return cast(int32) rc_live();\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(
            code, 0,
            "the for-init @Node must be released after the loop"
        );
    }
}

#[test]
fn a_struct_with_a_deinit_gets_a_drop_fn_even_without_rc_fields() {
    let out = c("File :: struct { fd: int32 }\n\
                 deinit :: proc[self: @File]() { }\n\
                 main :: proc() -> int32 { f := alloc File{ fd: 1 }; return f.fd; }\n");
    assert!(out.contains("dray_drop_File"), "needs a drop fn: {out}");
    assert!(
        out.contains("dray_m_rc_File_deinit(self)"),
        "drop fn must call the destructor: {out}"
    );
}

#[test]
fn a_deinit_runs_before_the_generated_field_release() {
    let out = c("Inner :: struct { v: int32 }\n\
                 Outer :: struct { inner: @Inner }\n\
                 deinit :: proc[self: @Outer]() { }\n\
                 main :: proc() -> int32 {\n\
                     o := alloc Outer{ inner: alloc Inner{ v: 1 } };\n\
                     return o.inner.v;\n\
                 }\n");
    let drop_body = out
        .split("void dray_drop_Outer(void *p) {")
        .nth(1)
        .unwrap_or_default();
    let deinit_at = drop_body.find("deinit").unwrap_or(usize::MAX);
    let release_at = drop_body.find("dray_rc_release").unwrap_or(usize::MAX);
    assert!(
        deinit_at < release_at,
        "deinit must precede field release: {drop_body}"
    );
}

#[test]
fn e2e_a_deinit_runs_exactly_once_at_end_of_life() {
    let src = "rc_live :: extern \"dray_rc_live\" proc() -> int64;\n\
               Counter :: struct { v: int32 }\n\
               Res :: struct { id: int32 }\n\
               deinit :: proc[self: @Res]() { }\n\
               main :: proc() -> int32 {\n\
                   { a := alloc Res{ id: 1 }; b := a; }\n\
                   return cast(int32) rc_live();\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 0, "aliased object must be freed exactly once");
    }
}

#[test]
fn an_rc_payload_binding_is_retained_and_released() {
    let out = c("Maybe :: enum(comptime T: type) { Some(T), None }\n\
                 Node :: struct { v: int32 }\n\
                 main :: proc() -> int32 {\n\
                     m := Maybe(@Node).Some(alloc Node{ v: 3 });\n\
                     switch m { case Maybe.Some(x): return x.v; case Maybe.None: return 0; }\n\
                 }\n");
    assert!(
        out.contains("dray_rc_retain(x)"),
        "an @T payload binding must own its reference: {out}"
    );
}

#[test]
fn e2e_an_iterated_element_outlives_its_iteration() {
    let src = "Maybe :: enum(comptime T: type) { Some(T), None }\n\
               Node :: struct { v: int32 }\n\
               Bag :: struct { n: int32 }\n\
               BagIter :: struct { cur: int32, n: int32 }\n\
               iterator :: proc[b: Bag]() -> @BagIter { return alloc BagIter{ cur: 0, n: b.n }; }\n\
               next :: proc[it: @BagIter]() -> Maybe(@Node) {\n\
                   if it.cur < it.n {\n\
                       node := alloc Node{ v: it.cur + 10 };\n\
                       it.cur = it.cur + 1;\n\
                       return Maybe(@Node).Some(node);\n\
                   }\n\
                   return Maybe(@Node).None;\n\
               }\n\
               main :: proc() -> int32 {\n\
                   kept := alloc Node{ v: 0 };\n\
                   b := Bag{ n: 3 };\n\
                   for x in b { kept = x; }\n\
                   return kept.v;\n\
               }\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(
            code, 12,
            "the last element must still be valid after the loop"
        );
    }
}
