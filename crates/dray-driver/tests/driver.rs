// SPDX-License-Identifier: Apache-2.0

//! Driver tests: the parse -> HIR -> codegen front pipeline.

use dray_driver::{BuildError, source_to_c};

#[test]
fn source_to_c_produces_c() {
    let c = source_to_c("main :: proc() -> int32 {\n    return 0;\n}\n").unwrap();
    assert!(c.contains("int32_t main(void)"));
    assert!(c.contains("#include <stdint.h>"));
}

#[test]
fn parse_errors_surface() {
    let e = source_to_c("main :: proc( {\n}\n").unwrap_err();
    assert!(matches!(e, BuildError::Parse(_)));
}

#[test]
fn resolve_errors_surface() {
    // parses fine, but `ghost` doesn't resolve
    let e = source_to_c("f :: proc() -> int32 {\n    return ghost;\n}\n").unwrap_err();
    assert!(matches!(e, BuildError::Resolve(_)), "got {e:?}");
}

#[test]
fn inference_and_aliasing_flow_through() {
    let c = source_to_c(
        "my_abs :: extern \"abs\" proc(x: int32) -> int32;\n\nmain :: proc() -> int32 {\n    v := 5;\n    return my_abs(v);\n}\n",
    )
    .unwrap();
    assert!(c.contains("int32_t v = 5;"), "inference: {c}");
    assert!(
        c.contains("abs(v)") && !c.contains("my_abs"),
        "aliasing: {c}"
    );
}

#[test]
fn line_directives_point_back_at_the_dray_source() {
    let src = "main :: proc() -> int32 {\n    t := 0;\n    t = t + 1;\n    return t;\n}\n";
    let c = dray_driver::source_to_c_from_file(src, "prog.dray").expect("compiles");
    assert!(c.contains("#line 2 \"prog.dray\""), "{c}");
    assert!(c.contains("#line 3 \"prog.dray\""), "{c}");
    assert!(c.contains("#line 4 \"prog.dray\""), "{c}");
}

#[test]
fn without_a_source_file_no_line_directives_are_emitted() {
    let src = "main :: proc() -> int32 {\n    return 0;\n}\n";
    let c = dray_driver::source_to_c(src).expect("compiles");
    assert!(!c.contains("#line"), "{c}");
}
