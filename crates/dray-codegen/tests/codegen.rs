// SPDX-License-Identifier: Apache-2.0

//! Codegen tests: Dray source -> C text, plus end-to-end compile-and-run tests
//! that shell out to a C compiler

use dray_codegen::compile_to_c;

/// Lower and return the generated C, asserting success.
fn c(src: &str) -> String {
    compile_to_c(src).unwrap_or_else(|e| panic!("codegen failed: {e}\n--- src ---\n{src}"))
}

// ── shape of generated C ─────────────────────────────────────────────────────

#[test]
fn empty_main_returns_void_style() {
    let out = c("main :: proc() {\n}\n");
    assert!(out.contains("void main(void)"), "got:\n{out}");
}

#[test]
fn main_with_int_return() {
    let out = c("main :: proc() -> int32 {\n    return 0;\n}\n");
    assert!(out.contains("int32_t main(void)"), "got:\n{out}");
    assert!(out.contains("return 0;"), "got:\n{out}");
}

#[test]
fn stdint_is_always_included() {
    let out = c("main :: proc() -> int32 {\n    return 0;\n}\n");
    assert!(out.contains("#include <stdint.h>"), "got:\n{out}");
}

#[test]
fn c_header_becomes_include() {
    let out = c("c_header(\"stdio.h\");\n\nmain :: proc() {\n}\n");
    assert!(out.contains("#include <stdio.h>"), "got:\n{out}");
}

#[test]
fn extern_becomes_prototype() {
    let out = c("puts :: extern \"puts\" proc(s: *int8) -> int32;\n");
    assert!(out.contains("int32_t puts(int8_t *s);"), "got:\n{out}");
}

#[test]
fn params_lower_with_types() {
    let out = c("add :: proc(a: int32, b: int32) -> int32 {\n    return a + b;\n}\n");
    assert!(
        out.contains("int32_t add(int32_t a, int32_t b)"),
        "got:\n{out}"
    );
    assert!(out.contains("return a + b;"), "got:\n{out}");
}

#[test]
fn precedence_is_preserved() {
    let out = c("f :: proc() -> int32 {\n    return 1 + 2 * 3;\n}\n");
    assert!(out.contains("1 + 2 * 3"), "got:\n{out}");
}

#[test]
fn for_c_style_lowers_to_c_for() {
    let out = c("f :: proc() {\n    for i := 0; i < 10; i += 1 {\n        x += i;\n    }\n}\n");
    assert!(
        out.contains("for (int i = 0; i < 10; i += 1)"),
        "got:\n{out}"
    );
}

#[test]
fn for_while_lowers_to_while() {
    let out = c("f :: proc() {\n    for x < 100 {\n        x *= 2;\n    }\n}\n");
    assert!(out.contains("while (x < 100)"), "got:\n{out}");
}

#[test]
fn for_infinite_lowers_to_for_ever() {
    let out = c("f :: proc() {\n    for {\n        break;\n    }\n}\n");
    assert!(out.contains("for (;;)"), "got:\n{out}");
}

#[test]
fn if_else_chain_lowers() {
    let out = c(
        "f :: proc() -> int32 {\n    if a {\n        return 1;\n    } else if b {\n        return 2;\n    } else {\n        return 3;\n    }\n    return 0;\n}\n",
    );
    assert!(out.contains("if (a)"), "got:\n{out}");
    assert!(out.contains("else"), "got:\n{out}");
}

#[test]
fn typed_var_decl_uses_declared_type() {
    let out = c("f :: proc() {\n    x: int32 = 5;\n}\n");
    assert!(out.contains("int32_t x = 5;"), "got:\n{out}");
}

#[test]
fn assignment_operators_lower() {
    let out = c("f :: proc() {\n    x = 1;\n    x += 2;\n    x <<= 3;\n}\n");
    assert!(out.contains("x = 1;"), "got:\n{out}");
    assert!(out.contains("x += 2;"), "got:\n{out}");
    assert!(out.contains("x <<= 3;"), "got:\n{out}");
}

// ── errors ───────────────────────────────────────────────────────────────────

#[test]
fn parse_errors_block_codegen() {
    let err = compile_to_c("main :: proc() {\n    return 1\n}\n").unwrap_err();
    assert!(err.to_string().contains("parse error"), "got: {err}");
}

#[test]
fn deferred_construct_is_a_clean_error_not_bad_c() {
    // alloc implies RC allocation, deferred to the IR stage
    let err = compile_to_c("f :: proc() {\n    x := alloc Node;\n}\n").unwrap_err();
    assert!(err.to_string().contains("alloc"), "got: {err}");
}

// ── end-to-end: compile the generated C and run it ───────────────────────────

/// Compile `c_src` with `cc` and run it, returning the process exit code.
/// Returns `None` if no C compiler is available (so the test self-skips).
fn compile_and_run(c_src: &str) -> Option<i32> {
    use std::process::Command;
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    if Command::new(&cc).arg("--version").output().is_err() {
        eprintln!("skipping run test: no C compiler");
        return None;
    }
    let dir = std::env::temp_dir();
    let stamp = format!(
        "dray_cg_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let c_path = dir.join(format!("{stamp}.c"));
    let bin_path = dir.join(&stamp);
    std::fs::write(&c_path, c_src).unwrap();
    let ok = Command::new(&cc)
        .arg(&c_path)
        .arg("-o")
        .arg(&bin_path)
        .status()
        .unwrap()
        .success();
    assert!(ok, "cc failed to compile:\n{c_src}");
    let code = Command::new(&bin_path)
        .status()
        .unwrap()
        .code()
        .unwrap_or(-1);
    let _ = std::fs::remove_file(&c_path);
    let _ = std::fs::remove_file(&bin_path);
    Some(code)
}

#[test]
fn e2e_compute_loop_exit_code() {
    let src = "main :: proc() -> int32 {\n    sum := 0;\n    for i := 0; i < 10; i += 1 {\n        sum += i;\n    }\n    if sum > 20 {\n        return 1;\n    }\n    return 0;\n}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 1, "expected exit 1 (sum=45>20)");
    }
}

#[test]
fn e2e_arithmetic_precedence_exit_code() {
    let src = "main :: proc() -> int32 {\n    x := 3;\n    y := 4;\n    if x * x + y * y == 25 {\n        return 42;\n    }\n    return 0;\n}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 42, "expected exit 42");
    }
}

#[test]
fn e2e_extern_call_matching_symbol() {
    let src = "abs :: extern \"abs\" proc(x: int32) -> int32;\n\nmain :: proc() -> int32 {\n    n := -7;\n    return abs(n);\n}\n";
    if let Some(code) = compile_and_run(&c(src)) {
        assert_eq!(code, 7, "expected exit 7 (abs(-7))");
    }
}
