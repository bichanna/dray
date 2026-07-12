// SPDX-License-Identifier: Apache-2.0

use dray_codegen::hir_to_c;
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
    hir_to_c(&hir).unwrap_or_else(|e| panic!("codegen failed: {e}"))
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
    let out = c("my_abs :: extern \"abs\" proc(x: int32) -> int32;\n\nmain :: proc() -> int32 {\n    return my_abs(-3);\n}\n");
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
    let out = c("f :: proc() -> int32 {\n    x := 1;\n    if x == 1 {\n        return 1;\n    } else {\n        return 2;\n    }\n}\n");
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
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    if Command::new(&cc).arg("--version").output().is_err() {
        return None;
    }
    let stamp = format!(
        "dray_cg_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
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
