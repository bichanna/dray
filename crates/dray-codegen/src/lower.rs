// SPDX-License-Identifier: Apache-2.0

//! The IR → Tamago lowering. Consumes RC-annotated IR (`dray-ir`); expressions
//! and types are still HIR's (reused unchanged), so only statements gain the RC
//! operations and `alloc`.

use std::collections::{HashMap, HashSet};

use dray_hir::{AssignOp, BinOp, DefId, DefKind, Expr, ExprKind, IntWidth, Ty, UnOp};
use dray_ir::{ExternProc, Ir, Item, Proc, Stmt};
use tamago::{
    self, BaseType, Block, BlockBuilder, EnumBuilder, FieldBuilder, ForBuilder, ForInit,
    FunctionBuilder, GlobalStatement, IfBuilder, IncludeBuilder, ParameterBuilder, Scope,
    ScopeBuilder, StructBuilder, SwitchBuilder, Type, VariableBuilder, VariantBuilder,
    WhileBuilder,
};

use crate::{CodegenError, Result};

/// Lower an IR module to a Tamago `Scope` (a C translation unit).
pub fn lower_ir(ir: &Ir) -> Result<Scope> {
    let mut scope = ScopeBuilder::new();

    scope = scope.global_statement(GlobalStatement::Include(
        IncludeBuilder::new_system_with_str("stdint.h").build(),
    ));
    scope = scope.global_statement(GlobalStatement::Include(
        IncludeBuilder::new_system_with_str("stdbool.h").build(),
    ));
    scope = scope.global_statement(GlobalStatement::Include(
        IncludeBuilder::new_system_with_str("stddef.h").build(),
    ));

    if ir.uses_rc {
        scope = scope.global_statement(GlobalStatement::Include(
            IncludeBuilder::new_system_with_str("stdlib.h").build(),
        ));
        scope = scope.new_line();
        scope = scope.global_statement(GlobalStatement::Raw(crate::RC_RUNTIME.to_string()));
    }

    for gs in aggregate_globals(ir)? {
        scope = scope.new_line();
        scope = scope.global_statement(gs);
    }

    for item in &ir.items {
        if let Item::Proc(p) = item
            && p.name != "main"
        {
            scope = scope.new_line();
            scope = scope.global_statement(GlobalStatement::Function(proc_prototype(p)?));
        }
    }

    for item in &ir.items {
        scope = scope.new_line();
        match item {
            Item::Include(h) => {
                scope = scope.global_statement(GlobalStatement::Include(
                    IncludeBuilder::new_system_with_str(h).build(),
                ));
            }
            Item::ExternProc(e) => {
                scope = scope.global_statement(GlobalStatement::Function(lower_extern(e)?));
            }
            Item::Proc(p) => {
                scope = scope.global_statement(GlobalStatement::Function(lower_proc(ir, p)?));
            }
        }
    }
    Ok(scope.build())
}

fn lower_extern(e: &ExternProc) -> Result<tamago::Function> {
    let mut fb = FunctionBuilder::new_with_str(&e.symbol, lower_ty(&e.ret)?).make_extern();
    for p in &e.params {
        fb = fb.param(ParameterBuilder::new_with_str(&c_ident(&p.name), lower_ty(&p.ty)?).build());
    }
    Ok(fb.build())
}

fn proc_signature(p: &Proc) -> Result<FunctionBuilder> {
    let mut fb = FunctionBuilder::new_with_str(&c_ident(&p.name), lower_ty(&p.ret)?);
    for param in &p.params {
        fb = fb.param(
            ParameterBuilder::new_with_str(&c_ident(&param.name), lower_ty(&param.ty)?).build(),
        );
    }
    Ok(fb)
}

/// `ret name(params);` a declaration with no body
fn proc_prototype(p: &Proc) -> Result<tamago::Function> {
    Ok(proc_signature(p)?.build())
}

fn lower_proc(ir: &Ir, p: &Proc) -> Result<tamago::Function> {
    Ok(proc_signature(p)?.body(lower_body(ir, &p.body)?).build())
}

fn lower_body(ir: &Ir, stmts: &[Stmt]) -> Result<Block> {
    let mut b = BlockBuilder::new();
    for s in stmts {
        let unwrapped = match s {
            Stmt::Located { stmt, .. } => stmt.as_ref(),
            other => other,
        };
        if let Stmt::Block(inner) = unwrapped {
            for st in inner {
                b = b.statement(lower_stmt(ir, st)?);
            }
            continue;
        }
        b = b.statement(lower_stmt(ir, s)?);
    }
    Ok(b.build())
}

fn lower_stmt(ir: &Ir, s: &Stmt) -> Result<tamago::Statement> {
    use tamago::Statement;
    Ok(match s {
        Stmt::Let { name, ty, init } => Statement::Variable(
            VariableBuilder::new_with_str(&c_ident(name), lower_ty(ty)?)
                .value(lower_expr(ir, init)?)
                .build(),
        ),
        Stmt::Assign { target, op, value } => Statement::Expr(tamago::Expr::new_assign(
            lower_expr(ir, target)?,
            assign_op(*op),
            lower_expr(ir, value)?,
        )),
        Stmt::Return(Some(e)) => Statement::Return(Some(lower_expr(ir, e)?)),
        Stmt::Return(None) => Statement::Return(None),
        Stmt::Break => Statement::Break,
        Stmt::Continue => Statement::Continue,
        Stmt::Expr(e) => Statement::Expr(lower_expr(ir, e)?),
        Stmt::Located { offset, stmt } => {
            let inner = lower_stmt(ir, stmt)?;
            return Ok(match ir.source.as_ref().and_then(|m| m.locate(*offset)) {
                Some((file, line)) => inner.located(tamago::SourceLoc::new(file, line)),
                None => inner,
            });
        }
        Stmt::Block(_) => {
            return Err(CodegenError::new(
                "internal: a block statement reached codegen unflattened".to_string(),
            ));
        }

        Stmt::StaticAssert { cond, message } => Statement::StaticAssert(tamago::StaticAssert::new(
            lower_expr(ir, cond)?,
            message.clone(),
        )),
        Stmt::If {
            cond,
            then_branch,
            else_branch,
        } => Statement::If(lower_if(ir, cond, then_branch, else_branch.as_deref())?),
        Stmt::While { cond, body } => Statement::While(
            WhileBuilder::new(lower_expr(ir, cond)?)
                .body(lower_body(ir, body)?)
                .build(),
        ),
        Stmt::Loop { body } => {
            Statement::For(ForBuilder::new().body(lower_body(ir, body)?).build())
        }
        Stmt::CFor {
            init,
            cond,
            post,
            body,
        } => {
            let mut fb = ForBuilder::new();
            if let Some(init) = init {
                fb = fb.init(lower_for_init(ir, init)?);
            }
            if let Some(cond) = cond {
                fb = fb.cond(lower_expr(ir, cond)?);
            }
            if let Some(post) = post {
                fb = fb.step(lower_for_step(ir, post)?);
            }
            Statement::For(fb.body(lower_body(ir, body)?).build())
        }
        Stmt::DropValue { name, ty } => {
            let type_name = match ty {
                Ty::Named(n) => n.clone(),
                other => {
                    return Err(CodegenError::new(format!(
                        "internal: cannot drop a by-value {other:?}"
                    )));
                }
            };
            Statement::Expr(tamago::Expr::new_fn_call(
                tamago::Expr::new_ident(format!("dray_drop_{type_name}")),
                vec![tamago::Expr::new_unary(
                    tamago::Expr::new_ident(c_ident(name)),
                    tamago::UnaryOp::AddrOf,
                )],
            ))
        }
        Stmt::Retain(name) => Statement::Expr(rc_call("dray_rc_retain", name)),
        Stmt::Release(name) | Stmt::Free(name) => Statement::Expr(rc_call("dray_rc_release", name)),
        Stmt::Switch { scrutinee, arms } => lower_switch(ir, scrutinee, arms)?,
    })
}

fn block_uses_name(stmts: &[Stmt], name: &str) -> bool {
    stmts.iter().any(|s| stmt_uses_name(s, name))
}

fn stmt_uses_name(s: &Stmt, name: &str) -> bool {
    match s {
        Stmt::Let { init, .. } => expr_uses_name(init, name),
        Stmt::Assign { target, value, .. } => {
            expr_uses_name(target, name) || expr_uses_name(value, name)
        }
        Stmt::Return(Some(e)) | Stmt::Expr(e) => expr_uses_name(e, name),
        Stmt::StaticAssert { cond, .. } => expr_uses_name(cond, name),
        Stmt::Block(body) => block_uses_name(body, name),
        Stmt::Located { stmt, .. } => stmt_uses_name(stmt, name),
        Stmt::DropValue { name: n, .. } => n == name,
        Stmt::Retain(n) | Stmt::Release(n) | Stmt::Free(n) => n == name,
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => false,
        Stmt::If {
            cond,
            then_branch,
            else_branch,
        } => {
            expr_uses_name(cond, name)
                || block_uses_name(then_branch, name)
                || else_branch
                    .as_ref()
                    .is_some_and(|b| block_uses_name(b, name))
        }
        Stmt::While { cond, body } => expr_uses_name(cond, name) || block_uses_name(body, name),
        Stmt::CFor {
            init,
            cond,
            post,
            body,
        } => {
            init.as_ref().is_some_and(|i| stmt_uses_name(i, name))
                || cond.as_ref().is_some_and(|c| expr_uses_name(c, name))
                || post.as_ref().is_some_and(|p| stmt_uses_name(p, name))
                || block_uses_name(body, name)
        }
        Stmt::Loop { body } => block_uses_name(body, name),
        Stmt::Switch { scrutinee, arms } => {
            expr_uses_name(scrutinee, name) || arms.iter().any(|a| block_uses_name(&a.body, name))
        }
    }
}

fn expr_uses_name(e: &Expr, name: &str) -> bool {
    match &e.kind {
        ExprKind::Name { name: n, .. } => n == name,
        ExprKind::Unary { operand, .. } | ExprKind::Paren(operand) => expr_uses_name(operand, name),
        ExprKind::Binary { lhs, rhs, .. } => expr_uses_name(lhs, name) || expr_uses_name(rhs, name),
        ExprKind::Call { callee, args } => {
            expr_uses_name(callee, name) || args.iter().any(|a| expr_uses_name(a, name))
        }
        ExprKind::Field { recv, .. } => expr_uses_name(recv, name),
        ExprKind::Index { base, index } => {
            expr_uses_name(base, name) || expr_uses_name(index, name)
        }
        ExprKind::Cast { operand, .. } => expr_uses_name(operand, name),
        ExprKind::Alloc { fields, .. } | ExprKind::StructLit { fields, .. } => {
            fields.iter().any(|(_, v)| expr_uses_name(v, name))
        }
        ExprKind::EnumInit { args, .. } | ExprKind::GenericCall { args, .. } => {
            args.iter().any(|a| expr_uses_name(a, name))
        }
        ExprKind::ArrayLit { elements, .. } => elements.iter().any(|e| expr_uses_name(e, name)),
        ExprKind::SliceAll { array } => expr_uses_name(array, name),
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Char(_)
        | ExprKind::Bool(_)
        | ExprKind::SizeOf(_)
        | ExprKind::ZeroValue(_)
        | ExprKind::Unresolved(_) => false,
    }
}

fn lower_switch(
    ir: &Ir,
    scrutinee: &Expr,
    arms: &[dray_ir::SwitchArm],
) -> Result<tamago::Statement> {
    use tamago::Expr as T;
    use tamago::Statement;

    let tag = T::new_mem_access(lower_expr(ir, scrutinee)?, "tag".to_string());
    let mut sw = SwitchBuilder::new(tag);
    for arm in arms {
        let mut b = BlockBuilder::new();
        match &arm.pattern {
            dray_ir::Pattern::Enum {
                enum_name,
                variant,
                bindings,
            } => {
                let concrete = enum_type_name(&scrutinee.ty, enum_name);
                let payload = enum_payload(ir, concrete, variant);
                for (i, bind) in bindings.iter().enumerate() {
                    if !block_uses_name(&arm.body, bind) {
                        continue;
                    }
                    let ty = payload.get(i).cloned().unwrap_or(Ty::Infer);
                    let field =
                        T::new_mem_access(lower_expr(ir, scrutinee)?, payload_field(variant, i));
                    b = b.statement(tamago::Statement::Variable(
                        VariableBuilder::new_with_str(&c_ident(bind), lower_ty(&ty)?)
                            .value(field)
                            .build(),
                    ));
                }
                for st in &arm.body {
                    b = b.statement(lower_stmt(ir, st)?);
                }
                b = b.statement(tamago::Statement::Break);
                sw = sw.case(T::new_ident(tag_const(concrete, variant)), b.build());
            }
            dray_ir::Pattern::Value(e) => {
                for st in &arm.body {
                    b = b.statement(lower_stmt(ir, st)?);
                }
                b = b.statement(tamago::Statement::Break);
                sw = sw.case(lower_expr(ir, e)?, b.build());
            }
        }
    }
    Ok(Statement::Switch(sw.build()))
}

fn enum_type_name<'a>(ty: &'a Ty, template: &'a str) -> &'a str {
    match ty {
        Ty::Named(n) => n,
        _ => template,
    }
}

/// The payload types of `enum_name::variant`, looked up in the IR's enum table
fn enum_payload(ir: &Ir, enum_name: &str, variant: &str) -> Vec<Ty> {
    ir.enums
        .iter()
        .find(|e| e.name == enum_name)
        .and_then(|e| e.variants.iter().find(|v| v.name == variant))
        .map(|v| v.payload.clone())
        .unwrap_or_default()
}

/// `Enum_Variant` — the C tag constant.
fn tag_const(enum_name: &str, variant: &str) -> String {
    format!("{enum_name}_{variant}")
}

/// `Variant_fN` — the flat payload field name for payload slot `n`.
fn payload_field(variant: &str, i: usize) -> String {
    format!("{variant}_f{i}")
}

/// `dray_new_Enum_Variant` — the per-variant constructor name.
fn enum_ctor_name(enum_name: &str, variant: &str) -> String {
    format!("dray_new_{enum_name}_{variant}")
}

fn rc_call(func: &str, arg: &str) -> tamago::Expr {
    use tamago::Expr as T;
    T::new_fn_call(
        T::new_ident(func.to_string()),
        vec![T::new_ident(c_ident(arg))],
    )
}

fn lower_if(
    ir: &Ir,
    cond: &Expr,
    then_branch: &[Stmt],
    else_branch: Option<&[Stmt]>,
) -> Result<tamago::If> {
    let mut b = IfBuilder::new(lower_expr(ir, cond)?).then(lower_body(ir, then_branch)?);
    if let Some(eb) = else_branch {
        b = b.other(lower_body(ir, eb)?);
    }
    Ok(b.build())
}

fn lower_for_init(ir: &Ir, s: &Stmt) -> Result<ForInit> {
    Ok(match s {
        Stmt::Let { name, ty, init } => ForInit::Decl(
            VariableBuilder::new_with_str(&c_ident(name), lower_ty(ty)?)
                .value(lower_expr(ir, init)?)
                .build(),
        ),
        Stmt::Assign { target, op, value } => ForInit::Expr(tamago::Expr::new_assign(
            lower_expr(ir, target)?,
            assign_op(*op),
            lower_expr(ir, value)?,
        )),
        _ => return Err(CodegenError::new("unsupported for-init statement")),
    })
}

fn lower_for_step(ir: &Ir, s: &Stmt) -> Result<tamago::Expr> {
    Ok(match s {
        Stmt::Assign { target, op, value } => tamago::Expr::new_assign(
            lower_expr(ir, target)?,
            assign_op(*op),
            lower_expr(ir, value)?,
        ),
        Stmt::Expr(e) => lower_expr(ir, e)?,
        _ => return Err(CodegenError::new("unsupported for-post statement")),
    })
}

fn lower_expr(ir: &Ir, e: &Expr) -> Result<tamago::Expr> {
    use tamago::Expr as T;
    Ok(match &e.kind {
        ExprKind::Int(v) => T::Int(*v),
        ExprKind::Float(v) => T::Double(*v),
        ExprKind::Str(s) => T::Str(s.clone()),
        ExprKind::Char(c) => T::Char(*c),
        ExprKind::Bool(b) => T::Bool(*b),
        ExprKind::Name { def, name } => T::new_ident(c_name(ir, *def, name)),
        ExprKind::Unresolved(n) => {
            return Err(CodegenError::new(format!(
                "unresolved name `{n}` reached codegen"
            )));
        }
        ExprKind::Unary { op, operand } => T::new_unary(lower_expr(ir, operand)?, un_op(*op)),
        ExprKind::Binary { op, lhs, rhs } => {
            T::new_binary(lower_expr(ir, lhs)?, bin_op(*op), lower_expr(ir, rhs)?)
        }
        ExprKind::Call { callee, args } => {
            let a = args
                .iter()
                .map(|a| lower_expr(ir, a))
                .collect::<Result<Vec<_>>>()?;
            T::new_fn_call(lower_expr(ir, callee)?, a)
        }
        ExprKind::Field { recv, member } => {
            let base = lower_expr(ir, recv)?;
            if matches!(recv.ty, Ty::Rc(_) | Ty::Ptr(_)) {
                T::new_ptr_mem_access(base, member.clone())
            } else {
                T::new_mem_access(base, member.clone())
            }
        }
        ExprKind::Index { base, index } => {
            let indexable = match &base.ty {
                Ty::Slice(_) => T::new_mem_access(lower_expr(ir, base)?, "ptr".to_string()),
                _ => lower_expr(ir, base)?,
            };
            T::new_arr_index(indexable, lower_expr(ir, index)?)
        }
        ExprKind::Cast { ty, operand } => T::new_cast(lower_ty(ty)?, lower_expr(ir, operand)?),
        ExprKind::SizeOf(ty) => T::new_sizeof(lower_ty(ty)?),
        ExprKind::ArrayLit { elements, .. } => {
            let mut values = Vec::with_capacity(elements.len());
            for e in elements {
                values.push(lower_expr(ir, e)?);
            }
            T::new_init_struct_in_order(values)
        }
        ExprKind::ZeroValue(ty) => match ty {
            Ty::Bool => T::Bool(false),
            Ty::Float { .. } => T::Double(0.0),
            Ty::Int { .. } | Ty::Ptr(_) | Ty::Rc(_) => T::Int(0),
            _ => T::new_init_struct_in_order(vec![T::Int(0)]),
        },
        ExprKind::SliceAll { array } => {
            let Ty::Array(elem, n) = &array.ty else {
                // Already a slice: slicing the whole of it is the identity.
                return lower_expr(ir, array);
            };
            let base = lower_expr(ir, array)?;
            T::new_compound_literal(
                Type::base(BaseType::Struct(slice_struct_name(elem))),
                T::new_init_struct_designated(
                    vec!["len".to_string(), "ptr".to_string()],
                    vec![
                        T::Int(*n as i64),
                        T::new_unary(T::new_arr_index(base, T::Int(0)), tamago::UnaryOp::AddrOf),
                    ],
                ),
            )
        }
        ExprKind::StructLit { ty, fields } => {
            let mut names = Vec::with_capacity(fields.len());
            let mut values = Vec::with_capacity(fields.len());

            for (name, value) in fields {
                names.push(name.clone());
                values.push(lower_expr(ir, value)?);
            }

            T::new_compound_literal(lower_ty(ty)?, T::new_init_struct_designated(names, values))
        }
        ExprKind::GenericCall { proc_name, .. } => {
            return Err(CodegenError::new(format!(
                "internal: un-monomorphized call to generic proc `{proc_name}` reached codegen"
            )));
        }
        ExprKind::EnumInit {
            enum_name,
            variant,
            args,
        } => {
            let mut a = Vec::new();
            for arg in args {
                a.push(lower_expr(ir, arg)?);
            }
            let concrete = enum_type_name(&e.ty, enum_name);
            T::new_fn_call(T::new_ident(enum_ctor_name(concrete, variant)), a)
        }
        ExprKind::Alloc { ty, fields } => match ty {
            Ty::Named(name) => {
                let sd = ir.structs.iter().find(|s| &s.name == name);
                let mut args = Vec::new();
                if let Some(sd) = sd {
                    for f in &sd.fields {
                        match fields.iter().find(|(n, _)| n == &f.name) {
                            Some((_, e)) => args.push(lower_expr(ir, e)?),
                            None => args.push(T::Int(0)), // zero/NULL default
                        }
                    }
                }
                T::new_fn_call(T::new_ident(format!("dray_new_{name}")), args)
            }
            _ => {
                // A scalar `@T` allocates directly with no drop function.
                let raw = T::new_fn_call(
                    T::new_ident("dray_rc_alloc".to_string()),
                    vec![T::new_sizeof(lower_ty(ty)?), T::Int(0)],
                );
                T::new_cast(Type::ptr(lower_ty(ty)?), raw)
            }
        },
        ExprKind::Paren(inner) => T::new_parenthesized(lower_expr(ir, inner)?),
    })
}

/// The C name for a resolved reference: an extern's linked symbol, else the
/// Dray name (which is the C name for procs, params, and locals).
fn c_name(ir: &Ir, def: DefId, fallback: &str) -> String {
    match &ir.def(def).kind {
        // An `extern` names a real C symbol, which must be spelled exactly.
        DefKind::ExternProc { symbol } => symbol.clone(),
        _ => c_ident(fallback),
    }
}

fn lower_ty(t: &Ty) -> Result<Type> {
    use tamago::BaseType as B;
    Ok(match t {
        Ty::Void => Type::base(B::Void),
        Ty::Bool => Type::base(B::Bool),
        Ty::Int { bits, signed } => Type::base(int_base(*bits, *signed)),
        Ty::Float { bits } => Type::base(if *bits == 32 { B::Float } else { B::Double }),
        // Both raw and RC pointers are a C `T*`; the RC bookkeeping is separate.
        Ty::Ptr(inner) | Ty::Rc(inner) => Type::ptr(lower_ty(inner)?),
        Ty::Named(n) => Type::base(B::Struct(n.clone())),
        Ty::Array(elem, n) => Type::array(lower_ty(elem)?, Some(tamago::Expr::Int(*n as i64))),
        Ty::Slice(elem) => Type::base(B::Struct(slice_struct_name(elem))),
        Ty::App(name, _) => {
            return Err(CodegenError::new(format!(
                "internal: un-monomorphized generic `{name}` reached codegen"
            )));
        }
        Ty::Infer => Type::base(B::Int32),
    })
}

fn int_base(bits: IntWidth, signed: bool) -> tamago::BaseType {
    use tamago::BaseType as B;
    match (bits, signed) {
        (IntWidth::W8, true) => B::Int8,
        (IntWidth::W16, true) => B::Int16,
        (IntWidth::W32, true) => B::Int32,
        (IntWidth::W64, true) => B::Int64,
        (IntWidth::W8, false) => B::UInt8,
        (IntWidth::W16, false) => B::UInt16,
        (IntWidth::W32, false) => B::UInt32,
        (IntWidth::W64, false) => B::UInt64,
        (IntWidth::Size, _) => B::Size,
    }
}

fn un_op(op: UnOp) -> tamago::UnaryOp {
    use tamago::UnaryOp as U;
    match op {
        UnOp::Neg => U::Neg,
        UnOp::LogicNot => U::LogicNeg,
        UnOp::BitNot => U::BitNot,
        UnOp::AddrOf => U::AddrOf,
        UnOp::Deref => U::Deref,
    }
}

fn bin_op(op: BinOp) -> tamago::BinOp {
    use tamago::BinOp as B;
    match op {
        BinOp::Add => B::Add,
        BinOp::Sub => B::Sub,
        BinOp::Mul => B::Mul,
        BinOp::Div => B::Div,
        BinOp::Rem => B::Mod,
        BinOp::Eq => B::Eq,
        BinOp::Ne => B::NEq,
        BinOp::Lt => B::LT,
        BinOp::Le => B::LTE,
        BinOp::Gt => B::GT,
        BinOp::Ge => B::GTE,
        BinOp::And => B::And,
        BinOp::Or => B::Or,
        BinOp::BitAnd => B::BitAnd,
        BinOp::BitOr => B::BitOr,
        BinOp::BitXor => B::XOr,
        BinOp::Shl => B::LShift,
        BinOp::Shr => B::RShift,
    }
}

fn assign_op(op: AssignOp) -> tamago::AssignOp {
    use tamago::AssignOp as A;
    match op {
        AssignOp::Assign => A::Assign,
        AssignOp::Add => A::AddAssign,
        AssignOp::Sub => A::SubAssign,
        AssignOp::Mul => A::MulAssign,
        AssignOp::Div => A::DivAssign,
        AssignOp::Rem => A::ModAssign,
        AssignOp::BitAnd => A::BitAndAssign,
        AssignOp::BitOr => A::BitOrAssign,
        AssignOp::BitXor => A::BitXOrAssign,
        AssignOp::Shl => A::LShiftAssign,
        AssignOp::Shr => A::RShiftAssign,
    }
}

/// Build the C globals for every struct, entirely with Tamago's typed builders:
/// a forward declaration, the definition, drop glue (for structs with `@T`
/// fields), and a constructor `dray_new_T`. Emitted ahead of the user's functions.
pub(crate) fn aggregate_globals(ir: &Ir) -> Result<Vec<GlobalStatement>> {
    let mut out = Vec::new();

    for elem in slice_element_types(ir) {
        out.push(GlobalStatement::Struct(slice_struct(&elem)?));
    }

    let pointed_to = pointer_referenced_aggregates(ir);

    for sd in &ir.structs {
        if pointed_to.contains(sd.name.as_str()) {
            out.push(GlobalStatement::Struct(
                StructBuilder::new_with_str(&sd.name)
                    .forward_declaration()
                    .build(),
            ));
        }
    }
    for ed in &ir.enums {
        let mut eb = EnumBuilder::new_with_str(&format!("{}_Tag", ed.name));
        for v in &ed.variants {
            eb = eb.variant(VariantBuilder::new_with_str(&tag_const(&ed.name, &v.name)).build());
        }
        out.push(GlobalStatement::Enum(eb.build()));
        if pointed_to.contains(ed.name.as_str()) {
            out.push(GlobalStatement::Struct(
                StructBuilder::new_with_str(&ed.name)
                    .forward_declaration()
                    .build(),
            ));
        }
    }

    // 2. Definitions in dependency order: a type embedded *by value* must be fully
    //    defined before the type embedding it (a forward declaration only suffices
    //    for a pointer field).
    let structs: HashMap<&str, &dray_ir::StructDef> =
        ir.structs.iter().map(|s| (s.name.as_str(), s)).collect();
    let enums: HashMap<&str, &dray_ir::EnumDef> =
        ir.enums.iter().map(|e| (e.name.as_str(), e)).collect();
    for name in aggregate_definition_order(ir) {
        if let Some(sd) = structs.get(name.as_str()) {
            out.push(GlobalStatement::Struct(struct_definition(sd)?));
        } else if let Some(ed) = enums.get(name.as_str()) {
            out.push(GlobalStatement::Struct(enum_definition(ed)?));
        }
    }

    // 3. Prototypes for every generated function, ahead of all definitions.
    for sd in &ir.structs {
        if has_rc_field(ir, sd) {
            out.push(GlobalStatement::Function(drop_signature(&sd.name).build()));
        }
        out.push(GlobalStatement::Function(
            constructor_signature(sd)?.build(),
        ));
    }
    for ed in &ir.enums {
        if enum_has_rc_payload(ir, ed) {
            out.push(GlobalStatement::Function(drop_signature(&ed.name).build()));
        }
        for v in &ed.variants {
            out.push(GlobalStatement::Function(
                enum_ctor_signature(ed, v)?.build(),
            ));
        }
    }

    // 4. The definitions themselves.
    for sd in &ir.structs {
        if has_rc_field(ir, sd) {
            out.push(GlobalStatement::Function(drop_fn(ir, sd)?));
        }
    }
    for ed in &ir.enums {
        if enum_has_rc_payload(ir, ed) {
            out.push(GlobalStatement::Function(enum_drop_fn(ir, ed)?));
        }
    }
    for sd in &ir.structs {
        out.push(GlobalStatement::Function(constructor_fn(ir, sd)?));
    }
    for ed in &ir.enums {
        for v in &ed.variants {
            out.push(GlobalStatement::Function(enum_ctor(ed, v)?));
        }
    }
    Ok(out)
}

/// `struct DraySlice_T { int32_t len; T *ptr; }` the fat pointer behind `[]T`
fn slice_struct(elem: &Ty) -> Result<tamago::Struct> {
    Ok(StructBuilder::new_with_str(&slice_struct_name(elem))
        .field(FieldBuilder::new_with_str("len", Type::base(BaseType::Int32)).build())
        .field(FieldBuilder::new_with_str("ptr", Type::ptr(lower_ty(elem)?)).build())
        .define()
        .build())
}

fn slice_element_types(ir: &Ir) -> Vec<Ty> {
    let mut found: Vec<Ty> = Vec::new();
    let mut note = |ty: &Ty| {
        if let Ty::Slice(elem) = ty
            && !found.contains(elem)
        {
            found.push((**elem).clone());
        }
    };
    for sd in &ir.structs {
        for f in &sd.fields {
            note(&f.ty);
        }
    }
    for ed in &ir.enums {
        for ty in ed.variants.iter().flat_map(|v| v.payload.iter()) {
            note(ty);
        }
    }
    for item in &ir.items {
        if let Item::Proc(p) = item {
            note(&p.ret);
            for param in &p.params {
                note(&param.ty);
            }
            walk_stmt_types(&p.body, &mut note);
        }
    }
    found
}

fn walk_stmt_types(stmts: &[Stmt], note: &mut impl FnMut(&Ty)) {
    for s in stmts {
        let s = match s {
            Stmt::Located { stmt, .. } => stmt.as_ref(),
            other => other,
        };

        match s {
            Stmt::Block(body) => walk_stmt_types(body, note),
            Stmt::Let { ty, init, .. } => {
                note(ty);
                note(&init.ty);
            }
            Stmt::Assign { target, value, .. } => {
                note(&target.ty);
                note(&value.ty);
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) => note(&e.ty),
            Stmt::If {
                then_branch,
                else_branch,
                ..
            } => {
                walk_stmt_types(then_branch, note);
                if let Some(b) = else_branch {
                    walk_stmt_types(b, note);
                }
            }
            Stmt::While { body, .. } | Stmt::Loop { body } => walk_stmt_types(body, note),
            Stmt::CFor { body, .. } => walk_stmt_types(body, note),
            Stmt::Switch { arms, .. } => {
                for a in arms {
                    walk_stmt_types(&a.body, note);
                }
            }
            _ => {}
        }
    }
}

/// Spell a Dray identifier so it is a legal C identifier
fn c_ident(name: &str) -> String {
    const C_KEYWORDS: &[&str] = &[
        "auto",
        "break",
        "case",
        "char",
        "const",
        "continue",
        "default",
        "do",
        "double",
        "else",
        "enum",
        "extern",
        "float",
        "for",
        "goto",
        "if",
        "inline",
        "int",
        "long",
        "register",
        "restrict",
        "return",
        "short",
        "signed",
        "sizeof",
        "static",
        "struct",
        "switch",
        "typedef",
        "union",
        "unsigned",
        "void",
        "volatile",
        "while",
        "_Alignas",
        "_Alignof",
        "_Atomic",
        "_Bool",
        "_Complex",
        "_Generic",
        "_Imaginary",
        "_Noreturn",
        "_Static_assert",
        "_Thread_local",
        "bool",
        "true",
        "false",
        "NULL",
    ];
    if C_KEYWORDS.contains(&name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

fn slice_struct_name(elem: &Ty) -> String {
    format!("DraySlice_{}", mangle_c_ty(elem))
}

fn mangle_c_ty(ty: &Ty) -> String {
    match ty {
        Ty::Void => "void".to_string(),
        Ty::Bool => "bool".to_string(),
        Ty::Int { bits, signed } => format!(
            "{}{}",
            if *signed { "int" } else { "uint" },
            match bits {
                IntWidth::W8 => "8",
                IntWidth::W16 => "16",
                IntWidth::W32 => "32",
                IntWidth::W64 => "64",
                IntWidth::Size => "size",
            }
        ),
        Ty::Float { bits } => format!("float{bits}"),
        Ty::Named(n) => n.clone(),
        Ty::Ptr(inner) => format!("ptr_{}", mangle_c_ty(inner)),
        Ty::Rc(inner) => format!("rc_{}", mangle_c_ty(inner)),
        Ty::Array(elem, n) => format!("arr{n}_{}", mangle_c_ty(elem)),
        Ty::Slice(elem) => format!("slice_{}", mangle_c_ty(elem)),
        // Generics are gone by codegen and `Infer` never reaches a real type.
        Ty::App(..) | Ty::Infer => "unknown".to_string(),
    }
}

fn pointer_referenced_aggregates(ir: &Ir) -> HashSet<&str> {
    fn note<'a>(ty: &'a Ty, out: &mut HashSet<&'a str>) {
        if let Ty::Ptr(inner) | Ty::Rc(inner) = ty
            && let Ty::Named(name) = &**inner
        {
            out.insert(name.as_str());
        }
    }
    let mut out = HashSet::new();
    for sd in &ir.structs {
        for f in &sd.fields {
            note(&f.ty, &mut out);
        }
    }
    for ed in &ir.enums {
        for ty in ed.variants.iter().flat_map(|v| v.payload.iter()) {
            note(ty, &mut out);
        }
    }
    out
}

/// `struct Name { fields };`
fn struct_definition(sd: &dray_ir::StructDef) -> Result<tamago::Struct> {
    let mut sb = StructBuilder::new_with_str(&sd.name);
    for f in &sd.fields {
        sb = sb.field(FieldBuilder::new_with_str(&c_ident(&f.name), lower_ty(&f.ty)?).build());
    }
    Ok(sb.define().build())
}

fn aggregate_definition_order(ir: &Ir) -> Vec<String> {
    let mut deps: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut names: Vec<&str> = Vec::new();
    for sd in &ir.structs {
        names.push(&sd.name);
        deps.insert(
            &sd.name,
            sd.fields
                .iter()
                .filter_map(|f| by_value_dep(&f.ty))
                .collect(),
        );
    }
    for ed in &ir.enums {
        names.push(&ed.name);
        deps.insert(
            &ed.name,
            ed.variants
                .iter()
                .flat_map(|v| v.payload.iter())
                .filter_map(by_value_dep)
                .collect(),
        );
    }
    let mut order = Vec::new();
    let mut visited = HashSet::new();
    for n in names {
        topo_visit(n, &deps, &mut visited, &mut order);
    }
    order
}

fn by_value_dep(ty: &Ty) -> Option<&str> {
    match ty {
        Ty::Named(n) => Some(n.as_str()),
        _ => None,
    }
}

fn topo_visit<'a>(
    name: &'a str,
    deps: &HashMap<&'a str, Vec<&'a str>>,
    visited: &mut HashSet<String>,
    order: &mut Vec<String>,
) {
    if visited.contains(name) {
        return;
    }
    visited.insert(name.to_string());
    if let Some(ds) = deps.get(name) {
        for d in ds {
            topo_visit(d, deps, visited, order);
        }
    }
    order.push(name.to_string());
}

/// `void dray_drop_T(void *p) { T *self = (T *)p; dray_rc_release(self->f); ... }`
/// `void dray_drop_T(void *p)` — the shape every drop function shares.
fn drop_signature(type_name: &str) -> FunctionBuilder {
    FunctionBuilder::new_with_str(
        &format!("dray_drop_{type_name}"),
        Type::base(BaseType::Void),
    )
    .param(ParameterBuilder::new_with_str("p", Type::ptr(Type::base(BaseType::Void))).build())
}

fn drop_fn(ir: &Ir, sd: &dray_ir::StructDef) -> Result<tamago::Function> {
    let self_ty = Type::ptr(Type::base(BaseType::Struct(sd.name.clone())));
    let mut body = BlockBuilder::new();
    body = body.statement(tamago::Statement::Variable(
        VariableBuilder::new_with_str("self", self_ty.clone())
            .value(tamago::Expr::new_cast(
                self_ty.clone(),
                tamago::Expr::new_ident_with_str("p"),
            ))
            .build(),
    ));
    for f in &sd.fields {
        match &f.ty {
            Ty::Rc(_) => {
                body = body.statement(tamago::Statement::Expr(tamago::Expr::new_fn_call(
                    tamago::Expr::new_ident_with_str("dray_rc_release"),
                    vec![self_field("self", &f.name)],
                )));
            }
            Ty::Named(inner) if ty_holds_rc(ir, &f.ty) => {
                body = body.statement(tamago::Statement::Expr(tamago::Expr::new_fn_call(
                    tamago::Expr::new_ident(format!("dray_drop_{inner}")),
                    vec![tamago::Expr::new_unary(
                        self_field("self", &f.name),
                        tamago::UnaryOp::AddrOf,
                    )],
                )));
            }
            _ => {}
        }
    }
    Ok(drop_signature(&sd.name).body(body.build()).build())
}

/// `T *dray_new_T(f0 t0, ...) { T *self = (T *)dray_rc_alloc(sizeof(T), drop);
///  self->f0 = t0; ...; return self; }`
fn constructor_fn(ir: &Ir, sd: &dray_ir::StructDef) -> Result<tamago::Function> {
    let struct_ty = Type::base(BaseType::Struct(sd.name.clone()));
    let self_ty = Type::ptr(struct_ty.clone());
    let drop_arg = if has_rc_field(ir, sd) {
        tamago::Expr::new_ident(format!("dray_drop_{}", sd.name))
    } else {
        tamago::Expr::new_null()
    };
    let alloc = tamago::Expr::new_cast(
        self_ty.clone(),
        tamago::Expr::new_fn_call(
            tamago::Expr::new_ident_with_str("dray_rc_alloc"),
            vec![tamago::Expr::new_sizeof(struct_ty), drop_arg],
        ),
    );
    let mut body = BlockBuilder::new();
    body = body.statement(tamago::Statement::Variable(
        VariableBuilder::new_with_str("self", self_ty.clone())
            .value(alloc)
            .build(),
    ));
    for f in &sd.fields {
        body = body.statement(tamago::Statement::Expr(tamago::Expr::new_assign(
            self_field("self", &f.name),
            tamago::AssignOp::Assign,
            tamago::Expr::new_ident(f.name.clone()),
        )));
    }
    body = body.statement(tamago::Statement::Return(Some(
        tamago::Expr::new_ident_with_str("self"),
    )));

    Ok(constructor_signature(sd)?.body(body.build()).build())
}

/// `T *dray_new_T(f0 t0, ...)` — shared by the prototype and the definition.
fn constructor_signature(sd: &dray_ir::StructDef) -> Result<FunctionBuilder> {
    let self_ty = Type::ptr(Type::base(BaseType::Struct(sd.name.clone())));
    let mut fb = FunctionBuilder::new_with_str(&format!("dray_new_{}", sd.name), self_ty);
    for f in &sd.fields {
        fb = fb.param(ParameterBuilder::new_with_str(&c_ident(&f.name), lower_ty(&f.ty)?).build());
    }
    Ok(fb)
}

/// `(*base).field` — field access through a struct pointer.
fn self_field(base: &str, field: &str) -> tamago::Expr {
    tamago::Expr::new_ptr_mem_access(tamago::Expr::new_ident_with_str(base), field.to_string())
}

fn has_rc_field(ir: &Ir, sd: &dray_ir::StructDef) -> bool {
    sd.fields.iter().any(|f| ty_holds_rc(ir, &f.ty))
}

fn ty_holds_rc(ir: &Ir, ty: &Ty) -> bool {
    fn walk<'a>(ir: &'a Ir, ty: &'a Ty, seen: &mut Vec<&'a str>) -> bool {
        match ty {
            Ty::Rc(_) => true,
            Ty::Named(name) => {
                if seen.contains(&name.as_str()) {
                    return false;
                }
                seen.push(name);
                if let Some(sd) = ir.structs.iter().find(|s| &s.name == name) {
                    return sd.fields.iter().any(|f| walk(ir, &f.ty, seen));
                }
                if let Some(ed) = ir.enums.iter().find(|e| &e.name == name) {
                    return ed
                        .variants
                        .iter()
                        .flat_map(|v| v.payload.iter())
                        .any(|t| walk(ir, t, seen));
                }
                false
            }
            _ => false,
        }
    }

    walk(ir, ty, &mut Vec::new())
}

/// Build the C globals for every enum with Tamago's typed builders: a tag `enum`,
/// a wrapper `struct` (the tag plus a flat set of payload fields, one per payload
/// slot of each variant), and a constructor `dray_new_Enum_Variant` per variant.
///
/// The layout is deliberately flat (payloads side-by-side, not overlapped in a
/// union) — simple and correct; a union to reclaim the space is a later
/// refinement. Only the active variant's fields are ever read, guarded by `tag`.
/// `struct Name { enum Name_Tag tag; <flat payload fields> };`
fn enum_definition(ed: &dray_ir::EnumDef) -> Result<tamago::Struct> {
    let mut sb = StructBuilder::new_with_str(&ed.name);
    sb = sb.field(
        FieldBuilder::new_with_str(
            "tag",
            Type::base(BaseType::Enum(format!("{}_Tag", ed.name))),
        )
        .build(),
    );
    for v in &ed.variants {
        for (i, ty) in v.payload.iter().enumerate() {
            sb = sb.field(
                FieldBuilder::new_with_str(&payload_field(&v.name, i), lower_ty(ty)?).build(),
            );
        }
    }
    Ok(sb.define().build())
}

/// `Name dray_new_Name_V(t0 f0, ...) { Name self; self.tag = Name_V;
///  self.V_f0 = f0; ...; return self; }`
fn enum_has_rc_payload(ir: &Ir, ed: &dray_ir::EnumDef) -> bool {
    ed.variants
        .iter()
        .any(|v| v.payload.iter().any(|t| ty_holds_rc(ir, t)))
}

/// `void dray_drop_E(void *p) { E *self = ...; switch (self->tag) { ... } }`
fn enum_drop_fn(ir: &Ir, ed: &dray_ir::EnumDef) -> Result<tamago::Function> {
    let self_ty = Type::ptr(Type::base(BaseType::Struct(ed.name.clone())));
    let mut body = BlockBuilder::new();
    body = body.statement(tamago::Statement::Variable(
        VariableBuilder::new_with_str("self", self_ty.clone())
            .value(tamago::Expr::new_cast(
                self_ty,
                tamago::Expr::new_ident_with_str("p"),
            ))
            .build(),
    ));

    let mut sw = SwitchBuilder::new(self_field("self", "tag"));
    for v in &ed.variants {
        let owning: Vec<(usize, &Ty)> = v
            .payload
            .iter()
            .enumerate()
            .filter(|(_, t)| ty_holds_rc(ir, t))
            .collect();
        if owning.is_empty() {
            continue;
        }
        let mut case = BlockBuilder::new();
        for (i, ty) in owning {
            let slot = self_field("self", &payload_field(&v.name, i));
            case = case.statement(tamago::Statement::Expr(match ty {
                Ty::Rc(_) => tamago::Expr::new_fn_call(
                    tamago::Expr::new_ident_with_str("dray_rc_release"),
                    vec![slot],
                ),
                // A by-value payload owns its own nested pointers.
                Ty::Named(inner) => tamago::Expr::new_fn_call(
                    tamago::Expr::new_ident(format!("dray_drop_{inner}")),
                    vec![tamago::Expr::new_unary(slot, tamago::UnaryOp::AddrOf)],
                ),
                other => {
                    return Err(CodegenError::new(format!(
                        "internal: cannot drop payload of type {other:?}"
                    )));
                }
            }));
        }
        case = case.statement(tamago::Statement::Break);
        sw = sw.case(
            tamago::Expr::new_ident(tag_const(&ed.name, &v.name)),
            case.build(),
        );
    }
    // Variants with nothing to release contribute no case, so a `default` keeps
    // the switch exhaustive as far as the C compiler is concerned (-Wswitch).
    sw = sw.default(
        BlockBuilder::new()
            .statement(tamago::Statement::Break)
            .build(),
    );
    body = body.statement(tamago::Statement::Switch(sw.build()));

    Ok(drop_signature(&ed.name).body(body.build()).build())
}

fn enum_ctor(ed: &dray_ir::EnumDef, v: &dray_ir::Variant) -> Result<tamago::Function> {
    use tamago::Expr as T;
    let struct_ty = Type::base(BaseType::Struct(ed.name.clone()));
    let mut body = BlockBuilder::new();
    // `Name self;`
    body = body.statement(tamago::Statement::Variable(
        VariableBuilder::new_with_str("self", struct_ty.clone()).build(),
    ));
    // `self.tag = Name_V;`
    body = body.statement(tamago::Statement::Expr(T::new_assign(
        T::new_mem_access(T::new_ident_with_str("self"), "tag".to_string()),
        tamago::AssignOp::Assign,
        T::new_ident(tag_const(&ed.name, &v.name)),
    )));
    // `self.V_fi = fi;`
    for (i, _) in v.payload.iter().enumerate() {
        body = body.statement(tamago::Statement::Expr(T::new_assign(
            T::new_mem_access(T::new_ident_with_str("self"), payload_field(&v.name, i)),
            tamago::AssignOp::Assign,
            T::new_ident(format!("f{i}")),
        )));
    }
    body = body.statement(tamago::Statement::Return(Some(T::new_ident_with_str(
        "self",
    ))));

    Ok(enum_ctor_signature(ed, v)?.body(body.build()).build())
}

/// `E dray_new_E_V(f0 t0, ...)` — shared by the prototype and the definition.
fn enum_ctor_signature(ed: &dray_ir::EnumDef, v: &dray_ir::Variant) -> Result<FunctionBuilder> {
    let struct_ty = Type::base(BaseType::Struct(ed.name.clone()));
    let mut fb = FunctionBuilder::new_with_str(&enum_ctor_name(&ed.name, &v.name), struct_ty);
    for (i, ty) in v.payload.iter().enumerate() {
        fb = fb.param(ParameterBuilder::new_with_str(&format!("f{i}"), lower_ty(ty)?).build());
    }
    Ok(fb)
}
