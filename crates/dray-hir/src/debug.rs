// SPDX-License-Identifier: Apache-2.0

//! A readable dump of the HIR, for `dray dump-hir` and for tests.

use std::fmt::Write;

use crate::hir::*;

/// Render a whole HIR module to a string.
pub fn dump_hir(hir: &Hir) -> String {
    let mut out = String::new();
    for item in &hir.items {
        dump_item(item, &mut out);
    }
    out
}

fn dump_item(item: &Item, out: &mut String) {
    match item {
        Item::Include(h) => {
            let _ = writeln!(out, "include <{h}>");
        }
        Item::ExternProc(e) => {
            let _ = writeln!(
                out,
                "extern proc {} (symbol \"{}\") {} -> {} [#{}]",
                e.name,
                e.symbol,
                params(&e.params),
                ty(&e.ret),
                e.def.0
            );
        }
        Item::Proc(p) => {
            let _ = writeln!(
                out,
                "proc {} {} -> {} [#{}]",
                p.name,
                params(&p.params),
                ty(&p.ret),
                p.def.0
            );
            for s in &p.body {
                dump_stmt(s, 1, out);
            }
        }
        Item::Enum(ed) => {
            let _ = writeln!(out, "enum {} [#{}] {{", ed.name, ed.def.0);
            for v in &ed.variants {
                let ps: Vec<String> = v.payload.iter().map(ty).collect();
                if ps.is_empty() {
                    let _ = writeln!(out, "  {}", v.name);
                } else {
                    let _ = writeln!(out, "  {}({})", v.name, ps.join(", "));
                }
            }
            let _ = writeln!(out, "}}");
        }
        Item::Struct(sd) => {
            let _ = writeln!(out, "struct {} [#{}] {{", sd.name, sd.def.0);
            for f in &sd.fields {
                let _ = writeln!(out, "  {}: {}", f.name, ty(&f.ty));
            }
            let _ = writeln!(out, "}}");
        }
    }
}

fn dump_stmt(s: &Stmt, depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    match s {
        Stmt::Let {
            def,
            name,
            ty: t,
            init,
        } => {
            let _ = writeln!(
                out,
                "{pad}let {name}: {} = {} [#{}]",
                ty(t),
                expr(init),
                def.0
            );
        }
        Stmt::Assign { target, op, value } => {
            let _ = writeln!(out, "{pad}{} {} {}", expr(target), assign(*op), expr(value));
        }
        Stmt::Return(Some(e)) => {
            let _ = writeln!(out, "{pad}return {}", expr(e));
        }
        Stmt::Return(None) => {
            let _ = writeln!(out, "{pad}return");
        }
        Stmt::Break => {
            let _ = writeln!(out, "{pad}break");
        }
        Stmt::Continue => {
            let _ = writeln!(out, "{pad}continue");
        }
        Stmt::Expr(e) => {
            let _ = writeln!(out, "{pad}{}", expr(e));
        }
        Stmt::If {
            cond,
            then_branch,
            else_branch,
        } => {
            let _ = writeln!(out, "{pad}if {}", expr(cond));
            for st in then_branch {
                dump_stmt(st, depth + 1, out);
            }
            if let Some(eb) = else_branch {
                let _ = writeln!(out, "{pad}else");
                for st in eb {
                    dump_stmt(st, depth + 1, out);
                }
            }
        }
        Stmt::While { cond, body } => {
            let _ = writeln!(out, "{pad}while {}", expr(cond));
            for st in body {
                dump_stmt(st, depth + 1, out);
            }
        }
        Stmt::CFor {
            init,
            cond,
            post,
            body,
        } => {
            let c = cond.as_ref().map(expr).unwrap_or_else(|| "true".into());
            let _ = writeln!(out, "{pad}cfor (init; {c}; post)");
            if let Some(i) = init {
                let _ = write!(out, "{pad}  init: ");
                dump_stmt(i, 0, out);
            }
            if let Some(p) = post {
                let _ = write!(out, "{pad}  post: ");
                dump_stmt(p, 0, out);
            }
            for st in body {
                dump_stmt(st, depth + 1, out);
            }
        }
        Stmt::Loop { body } => {
            let _ = writeln!(out, "{pad}loop");
            for st in body {
                dump_stmt(st, depth + 1, out);
            }
        }
        Stmt::Switch { scrutinee, arms } => {
            let _ = writeln!(out, "{pad}switch {}", expr(scrutinee));
            for arm in arms {
                match &arm.pattern {
                    Pattern::Enum {
                        enum_name,
                        variant,
                        bindings,
                    } => {
                        let b = if bindings.is_empty() {
                            String::new()
                        } else {
                            format!("({})", bindings.join(", "))
                        };
                        let _ = writeln!(out, "{pad}  case {enum_name}.{variant}{b}:");
                    }
                    Pattern::Value(e) => {
                        let _ = writeln!(out, "{pad}  case {}:", expr(e));
                    }
                }
                for st in &arm.body {
                    dump_stmt(st, depth + 2, out);
                }
            }
        }
    }
}

pub fn expr(e: &Expr) -> String {
    let inner = match &e.kind {
        ExprKind::Int(v) => v.to_string(),
        ExprKind::Float(v) => v.to_string(),
        ExprKind::Str(s) => format!("{s:?}"),
        ExprKind::Char(c) => format!("'{c}'"),
        ExprKind::Bool(b) => b.to_string(),
        ExprKind::Name { def, name } => format!("{name}#{}", def.0),
        ExprKind::Unresolved(n) => format!("<unresolved {n}>"),
        ExprKind::Unary { op, operand } => format!("({}{})", un(*op), expr(operand)),
        ExprKind::Binary { op, lhs, rhs } => {
            format!("({} {} {})", expr(lhs), bin(*op), expr(rhs))
        }
        ExprKind::Call { callee, args } => {
            let a: Vec<String> = args.iter().map(expr).collect();
            format!("{}({})", expr(callee), a.join(", "))
        }
        ExprKind::Field { recv, member } => format!("{}.{member}", expr(recv)),
        ExprKind::Index { base, index } => format!("{}[{}]", expr(base), expr(index)),
        ExprKind::Cast { ty: t, operand } => format!("cast({}) {}", ty(t), expr(operand)),
        ExprKind::EnumInit {
            enum_name,
            variant,
            args,
        } => {
            if args.is_empty() {
                format!("{enum_name}.{variant}")
            } else {
                let a: Vec<String> = args.iter().map(expr).collect();
                format!("{enum_name}.{variant}({})", a.join(", "))
            }
        }
        ExprKind::Alloc { ty: t, fields } => {
            if fields.is_empty() {
                format!("alloc {}", ty(t))
            } else {
                let fs: Vec<String> = fields
                    .iter()
                    .map(|(n, e)| format!("{n}: {}", expr(e)))
                    .collect();
                format!("alloc {}{{ {} }}", ty(t), fs.join(", "))
            }
        }
        ExprKind::Paren(inner) => format!("({})", expr(inner)),
    };
    format!("{inner}:{}", ty(&e.ty))
}

fn params(ps: &[Param]) -> String {
    let inner: Vec<String> = ps
        .iter()
        .map(|p| format!("{}: {} #{}", p.name, ty(&p.ty), p.def.0))
        .collect();
    format!("({})", inner.join(", "))
}

pub fn ty(t: &Ty) -> String {
    match t {
        Ty::Void => "void".into(),
        Ty::Bool => "bool".into(),
        Ty::Int { bits, signed } => {
            let w = match bits {
                IntWidth::W8 => "8",
                IntWidth::W16 => "16",
                IntWidth::W32 => "32",
                IntWidth::W64 => "64",
                IntWidth::Size => "size",
            };
            format!("{}{}", if *signed { "int" } else { "uint" }, w)
        }
        Ty::Float { bits } => format!("float{bits}"),
        Ty::Ptr(inner) => format!("*{}", ty(inner)),
        Ty::Rc(inner) => format!("@{}", ty(inner)),
        Ty::Named(n) => n.clone(),
        Ty::App(n, args) => {
            let a: Vec<String> = args.iter().map(ty).collect();
            format!("{n}({})", a.join(", "))
        }
        Ty::Infer => "?".into(),
    }
}

fn un(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "-",
        UnOp::LogicNot => "!",
        UnOp::BitNot => "~",
        UnOp::AddrOf => "&",
        UnOp::Deref => "*",
    }
}

fn bin(op: BinOp) -> &'static str {
    use BinOp::*;
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        Rem => "%",
        Eq => "==",
        Ne => "!=",
        Lt => "<",
        Le => "<=",
        Gt => ">",
        Ge => ">=",
        And => "&&",
        Or => "||",
        BitAnd => "&",
        BitOr => "|",
        BitXor => "^",
        Shl => "<<",
        Shr => ">>",
    }
}

fn assign(op: AssignOp) -> &'static str {
    use AssignOp::*;
    match op {
        Assign => "=",
        Add => "+=",
        Sub => "-=",
        Mul => "*=",
        Div => "/=",
        Rem => "%=",
        BitAnd => "&=",
        BitOr => "|=",
        BitXor => "^=",
        Shl => "<<=",
        Shr => ">>=",
    }
}
