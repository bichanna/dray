// SPDX-License-Identifier: Apache-2.0

//! Driver tests: the parse -> HIR -> codegen front pipeline.

use dray_driver::{source_to_c, BuildError};

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
