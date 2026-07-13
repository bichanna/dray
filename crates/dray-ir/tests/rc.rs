// SPDX-License-Identifier: Apache-2.0

//! RC insertion pass tests: retain on copy, release at scope exit, fresh allocs
//! not retained

use dray_hir::lower as lower_hir;
use dray_ir::{Item, Stmt, lower};
use dray_syntax::parse;

fn ir_of(src: &str) -> dray_ir::Ir {
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let (hir, errs) = lower_hir(&parsed.root);
    assert!(errs.is_empty(), "resolve: {errs:?}");
    lower(&hir)
}

fn shapes(body: &[Stmt]) -> Vec<String> {
    let mut out = Vec::new();
    for s in body {
        match s {
            Stmt::Let { name, .. } => out.push(format!("let {name}")),
            Stmt::Retain(n) => out.push(format!("retain {n}")),
            Stmt::Release(n) => out.push(format!("release {n}")),
            Stmt::Assign { .. } => out.push("assign".into()),
            Stmt::Return(_) => out.push("return".into()),
            Stmt::If { then_branch, .. } => {
                out.push("if".into());
                out.extend(shapes(then_branch).into_iter().map(|s| format!("  {s}")));
            }
            other => out.push(format!("{other:?}").chars().take(8).collect()),
        }
    }
    out
}

fn body_of<'a>(ir: &'a dray_ir::Ir, name: &str) -> &'a [Stmt] {
    ir.items
        .iter()
        .find_map(|it| match it {
            Item::Proc(p) if p.name == name => Some(p.body.as_slice()),
            _ => None,
        })
        .expect("proc not found")
}

#[test]
fn fresh_alloc_is_not_retained_but_is_released() {
    let ir = ir_of("f :: proc() {\n    a := alloc int32;\n}\n");
    assert!(ir.uses_rc);
    let s = shapes(body_of(&ir, "f"));
    // a is allocated (owned), no retain, released at the end of the proc.
    assert_eq!(s, vec!["let a", "release a"], "{s:?}");
}

#[test]
fn copy_of_rc_local_is_retained() {
    let ir = ir_of("f :: proc() {\n    a := alloc int32;\n    b := a;\n}\n");
    let s = shapes(body_of(&ir, "f"));
    // b copies a → retain b; releases happen in reverse declaration order.
    assert_eq!(
        s,
        vec!["let a", "let b", "retain b", "release b", "release a"],
        "{s:?}"
    );
}

#[test]
fn release_happens_at_inner_block_exit() {
    let ir = ir_of("f :: proc() {\n    if true {\n        a := alloc int32;\n    }\n}\n");
    let s = shapes(body_of(&ir, "f"));
    // The release sits *inside* the if-block, not after it.
    assert_eq!(s, vec!["if", "  let a", "  release a"], "{s:?}");
}

#[test]
fn no_rc_means_no_runtime() {
    let ir = ir_of("f :: proc() -> int32 {\n    return 1;\n}\n");
    assert!(
        !ir.uses_rc,
        "a program with no @T should not pull in the RC runtime"
    );
}
