#[test]
fn source_to_c_produces_c() {
    let c = dray_driver::source_to_c("main :: proc() -> int32 {\n    return 0;\n}\n").unwrap();
    assert!(c.contains("int32_t main(void)"));
    assert!(c.contains("#include <stdint.h>"));
}
#[test]
fn source_to_c_reports_parse_errors() {
    let e = dray_driver::source_to_c("main :: proc( {\n}\n").unwrap_err();
    assert!(matches!(e, dray_driver::BuildError::Parse(_)));
}
