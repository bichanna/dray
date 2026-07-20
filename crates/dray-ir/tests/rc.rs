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
        let s = match s {
            Stmt::Located { stmt, .. } => stmt.as_ref(),
            other => other,
        };
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

#[test]
fn returning_a_fresh_rc_local_transfers_ownership_without_release() {
    let ir = ir_of(
        "N :: struct { v: int32 }\n\
         mk :: proc() -> @N {\n    n := alloc N{v: 1};\n    return n;\n}\n",
    );
    let s = shapes(body_of(&ir, "mk"));
    assert!(
        !s.iter().any(|l| l == "release n"),
        "n must not be released on the transfer path: {s:?}"
    );
    assert!(
        s.iter().any(|l| l == "return"),
        "should still return: {s:?}"
    );
}

#[test]
fn composite_alloc_field_of_rc_local_is_retained() {
    let ir = ir_of(
        "Inner :: struct { v: int32 }\n\
         N :: struct { v: int32, inner: @Inner }\n\
         f :: proc() {\n    a := alloc Inner{v: 1};\n    b := alloc N{v: 2, inner: a};\n}\n",
    );
    let s = shapes(body_of(&ir, "f"));
    let a_idx = s
        .iter()
        .position(|l| l == "let a")
        .expect(&format!("{s:?}"));
    let retain_idx = s
        .iter()
        .position(|l| l == "retain a")
        .expect(&format!("retain for composite lit field missing: {s:?}"));
    let b_idx = s
        .iter()
        .position(|l| l == "let b")
        .expect(&format!("{s:?}"));
    assert!(
        a_idx < retain_idx && retain_idx <= b_idx,
        "retain(a) must sit between `let a` and `let b`: {s:?}"
    );
}

#[test]
fn assigning_to_rc_local_releases_old_value() {
    let ir = ir_of(
        "N :: struct { v: int32 }\n\
         f :: proc() {\n    a := alloc N{v: 1};\n    a = alloc N{v: 2};\n}\n",
    );
    let s = shapes(body_of(&ir, "f"));
    let releases = s.iter().filter(|l| l.starts_with("release ")).count();
    assert!(
        releases >= 2,
        "expected a release for the old value plus the scope-exit release: {s:?}"
    );
    assert!(
        s.iter().any(|l| l == "assign"),
        "expected an assignment: {s:?}"
    );
    // The old-value release must land before the scope-exit release of `a` itself.
    let last_release_a = s.iter().rposition(|l| l == "release a").unwrap();
    let assign_idx = s.iter().position(|l| l == "assign").unwrap();
    assert!(
        s[assign_idx..last_release_a]
            .iter()
            .any(|l| l.starts_with("release __rc_tmp_")),
        "expected the old-value temp to be released between the assign and scope-exit: {s:?}"
    );
}

#[test]
fn rc_local_reassigned_to_another_rc_local_retains_new_and_releases_old() {
    let ir = ir_of(
        "N :: struct { v: int32 }\n\
         f :: proc() {\n    a := alloc N{v: 1};\n    b := alloc N{v: 2};\n    a = b;\n}\n",
    );
    let s = shapes(body_of(&ir, "f"));
    let assign_idx = s
        .iter()
        .position(|l| l == "assign")
        .expect(&format!("{s:?}"));
    assert!(
        s[assign_idx..].iter().any(|l| l == "retain a"),
        "target should be retained after the assignment: {s:?}"
    );
    let releases = s.iter().filter(|l| l.starts_with("release ")).count();
    // Two scope-exit releases (b, a) plus the old-value release from the assign
    assert!(releases >= 3, "want at least three releases total: {s:?}");
}

#[test]
fn reassigning_rc_local_to_field_retains_new_before_releasing_old() {
    let ir = ir_of(
        "N :: struct { v: int32, next: @N }\n\
         walk :: proc(head: @N) {\n    n := head;\n    n = n.next;\n}\n",
    );
    let s = shapes(body_of(&ir, "walk"));
    let assign_idx = s
        .iter()
        .position(|l| l == "assign")
        .expect(&format!("{s:?}"));
    let retain_after_assign = s[assign_idx..]
        .iter()
        .position(|l| l == "retain n")
        .expect(&format!("no retain(n) after the assign: {s:?}"));
    let release_old = s[assign_idx..]
        .iter()
        .position(|l| l.starts_with("release __rc_tmp_"))
        .expect(&format!("no release of the saved-old temp: {s:?}"));
    assert!(
        retain_after_assign < release_old,
        "retain(n) must precede release of the old temp: {s:?}"
    );
}
