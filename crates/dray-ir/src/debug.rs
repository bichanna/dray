// SPDX-License-Identifier: Apache-2.0

//! A readable dump of the IR, for `dray dump-ir`.

use crate::{Ir, Item, Proc, Stmt};
use dray_hir::debug::expr as expr_str;
use dray_hir::debug::ty as ty_str;

/// Render a whole IR module as text.
pub fn dump_ir(ir: &Ir) -> String {
    let mut out = String::new();
    if ir.uses_rc {
        out.push_str("// uses_rc: RC runtime will be emitted\n");
    }
    for sd in &ir.structs {
        let has_rc = sd
            .fields
            .iter()
            .any(|f| matches!(f.ty, dray_hir::Ty::Rc(_)));
        let note = if has_rc {
            "  // has @T fields -> gets drop glue"
        } else {
            ""
        };
        out.push_str(&format!("struct {} {{{}\n", sd.name, note));
        for f in &sd.fields {
            out.push_str(&format!("  {}: {}\n", f.name, ty_str(&f.ty)));
        }
        out.push_str("}\n");
    }
    for item in &ir.items {
        dump_item(item, &mut out);
    }
    out
}

fn dump_item(item: &Item, out: &mut String) {
    match item {
        Item::Include(h) => out.push_str(&format!("include <{h}>\n")),
        Item::ExternProc(e) => {
            out.push_str(&format!("extern \"{}\" proc {}(", e.symbol, e.name));
            out.push_str(&params(&e.params));
            out.push_str(&format!(") -> {}\n", ty_str(&e.ret)));
        }
        Item::Proc(p) => dump_proc(p, out),
    }
}

fn dump_proc(p: &Proc, out: &mut String) {
    out.push_str(&format!(
        "proc {}({}) -> {} {{\n",
        p.name,
        params(&p.params),
        ty_str(&p.ret)
    ));
    for s in &p.body {
        dump_stmt(s, 1, out);
    }
    out.push_str("}\n");
}

fn dump_stmt(s: &Stmt, depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    match s {
        Stmt::Let { name, ty, init } => out.push_str(&format!(
            "{pad}let {name}: {} = {}\n",
            ty_str(ty),
            expr_str(init)
        )),
        Stmt::Assign { target, op, value } => out.push_str(&format!(
            "{pad}{} {} {}\n",
            expr_str(target),
            assign_glyph(op),
            expr_str(value)
        )),
        Stmt::Return(Some(e)) => out.push_str(&format!("{pad}return {}\n", expr_str(e))),
        Stmt::Return(None) => out.push_str(&format!("{pad}return\n")),
        Stmt::Break => out.push_str(&format!("{pad}break\n")),
        Stmt::Continue => out.push_str(&format!("{pad}continue\n")),
        Stmt::Expr(e) => out.push_str(&format!("{pad}{}\n", expr_str(e))),
        Stmt::If {
            cond,
            then_branch,
            else_branch,
        } => {
            out.push_str(&format!("{pad}if {} {{\n", expr_str(cond)));
            for st in then_branch {
                dump_stmt(st, depth + 1, out);
            }
            if let Some(eb) = else_branch {
                out.push_str(&format!("{pad}}} else {{\n"));
                for st in eb {
                    dump_stmt(st, depth + 1, out);
                }
            }
            out.push_str(&format!("{pad}}}\n"));
        }
        Stmt::While { cond, body } => {
            out.push_str(&format!("{pad}while {} {{\n", expr_str(cond)));
            for st in body {
                dump_stmt(st, depth + 1, out);
            }
            out.push_str(&format!("{pad}}}\n"));
        }
        Stmt::Loop { body } => {
            out.push_str(&format!("{pad}loop {{\n"));
            for st in body {
                dump_stmt(st, depth + 1, out);
            }
            out.push_str(&format!("{pad}}}\n"));
        }
        Stmt::Switch { scrutinee, arms } => {
            out.push_str(&format!("{pad}switch {} {{\n", expr_str(scrutinee)));
            for arm in arms {
                match &arm.pattern {
                    dray_hir::Pattern::Enum {
                        enum_name,
                        variant,
                        bindings,
                    } => {
                        let b = if bindings.is_empty() {
                            String::new()
                        } else {
                            format!("({})", bindings.join(", "))
                        };
                        out.push_str(&format!("{pad}  case {enum_name}.{variant}{b}:\n"));
                    }
                    dray_hir::Pattern::Value(e) => {
                        out.push_str(&format!("{pad}  case {}:\n", expr_str(e)));
                    }
                }
                for st in &arm.body {
                    dump_stmt(st, depth + 2, out);
                }
            }
            out.push_str(&format!("{pad}}}\n"));
        }
        Stmt::CFor {
            init,
            cond,
            post,
            body,
        } => {
            out.push_str(&format!("{pad}for {{\n"));
            if let Some(i) = init {
                dump_stmt(i, depth + 1, out);
            }
            if let Some(c) = cond {
                out.push_str(&format!("{pad}  cond {}\n", expr_str(c)));
            }
            if let Some(p) = post {
                dump_stmt(p, depth + 1, out);
            }
            for st in body {
                dump_stmt(st, depth + 1, out);
            }
            out.push_str(&format!("{pad}}}\n"));
        }
        // The RC ops the pass inserted — the whole reason this dump exists.
        Stmt::Retain(n) => out.push_str(&format!("{pad}retain {n}\n")),
        Stmt::Release(n) => out.push_str(&format!("{pad}release {n}\n")),
        Stmt::Free(n) => out.push_str(&format!("{pad}free {n}\n")),
    }
}

fn params(ps: &[crate::Param]) -> String {
    ps.iter()
        .map(|p| format!("{}: {}", p.name, ty_str(&p.ty)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn assign_glyph(op: &crate::AssignOp) -> &'static str {
    use crate::AssignOp as A;
    match op {
        A::Assign => "=",
        A::Add => "+=",
        A::Sub => "-=",
        A::Mul => "*=",
        A::Div => "/=",
        A::Rem => "%=",
        A::BitAnd => "&=",
        A::BitOr => "|=",
        A::BitXor => "^=",
        A::Shl => "<<=",
        A::Shr => ">>=",
    }
}
