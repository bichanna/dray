// SPDX-License-Identifier: Apache-2.0

//! The HIR → Tamago lowering.

use dray_hir::{
    AssignOp, BinOp, DefId, DefKind, Expr, ExprKind, ExternProc, Hir, IntWidth, Item, Proc, Stmt,
    Ty, UnOp,
};
use tamago::{
    self, Block, BlockBuilder, ForBuilder, ForInit, FunctionBuilder, GlobalStatement, IfBuilder,
    IncludeBuilder, ParameterBuilder, Scope, ScopeBuilder, Type, VariableBuilder, WhileBuilder,
};

use crate::{CodegenError, Result};

/// Lower a HIR module to a Tamago `Scope` (a C translation unit).
pub fn lower_hir(hir: &Hir) -> Result<Scope> {
    let mut scope = ScopeBuilder::new();

    scope = scope.global_statement(GlobalStatement::Include(
        IncludeBuilder::new_system_with_str("stdint.h").build(),
    ));

    for item in &hir.items {
        scope = scope.new_line();
        match item {
            Item::Include(h) => {
                scope = scope.global_statement(GlobalStatement::Include(
                    IncludeBuilder::new_system_with_str(h).build(),
                ));
            }
            Item::ExternProc(e) => {
                scope = scope.global_statement(GlobalStatement::Function(lower_extern(hir, e)?));
            }
            Item::Proc(p) => {
                scope = scope.global_statement(GlobalStatement::Function(lower_proc(hir, p)?));
            }
        }
    }
    Ok(scope.build())
}

fn lower_extern(hir: &Hir, e: &ExternProc) -> Result<tamago::Function> {
    let mut fb = FunctionBuilder::new_with_str(&e.symbol, lower_ty(&e.ret)?).make_extern();
    for p in &e.params {
        fb = fb.param(ParameterBuilder::new_with_str(&p.name, lower_ty(&p.ty)?).build());
    }
    let _ = hir;
    Ok(fb.build())
}

fn lower_proc(hir: &Hir, p: &Proc) -> Result<tamago::Function> {
    let mut fb = FunctionBuilder::new_with_str(&p.name, lower_ty(&p.ret)?);
    for param in &p.params {
        fb = fb.param(ParameterBuilder::new_with_str(&param.name, lower_ty(&param.ty)?).build());
    }
    fb = fb.body(lower_body(hir, &p.body)?);
    Ok(fb.build())
}

fn lower_body(hir: &Hir, stmts: &[Stmt]) -> Result<Block> {
    let mut b = BlockBuilder::new();
    for s in stmts {
        b = b.statement(lower_stmt(hir, s)?);
    }
    Ok(b.build())
}

fn lower_stmt(hir: &Hir, s: &Stmt) -> Result<tamago::Statement> {
    use tamago::Statement;
    Ok(match s {
        Stmt::Let { name, ty, init, .. } => Statement::Variable(
            VariableBuilder::new_with_str(name, lower_ty(ty)?)
                .value(lower_expr(hir, init)?)
                .build(),
        ),
        Stmt::Assign { target, op, value } => Statement::Expr(tamago::Expr::new_assign(
            lower_expr(hir, target)?,
            assign_op(*op),
            lower_expr(hir, value)?,
        )),
        Stmt::Return(Some(e)) => Statement::Return(Some(lower_expr(hir, e)?)),
        Stmt::Return(None) => Statement::Return(None),
        Stmt::Break => Statement::Break,
        Stmt::Continue => Statement::Continue,
        Stmt::Expr(e) => Statement::Expr(lower_expr(hir, e)?),
        Stmt::If {
            cond,
            then_branch,
            else_branch,
        } => Statement::If(lower_if(hir, cond, then_branch, else_branch.as_deref())?),
        Stmt::While { cond, body } => Statement::While(
            WhileBuilder::new(lower_expr(hir, cond)?)
                .body(lower_body(hir, body)?)
                .build(),
        ),
        Stmt::Loop { body } => {
            Statement::For(ForBuilder::new().body(lower_body(hir, body)?).build())
        }
        Stmt::CFor {
            init,
            cond,
            post,
            body,
        } => {
            let mut fb = ForBuilder::new();
            if let Some(init) = init {
                fb = fb.init(lower_for_init(hir, init)?);
            }
            if let Some(cond) = cond {
                fb = fb.cond(lower_expr(hir, cond)?);
            }
            if let Some(post) = post {
                fb = fb.step(lower_for_step(hir, post)?);
            }
            Statement::For(fb.body(lower_body(hir, body)?).build())
        }
    })
}

fn lower_if(
    hir: &Hir,
    cond: &Expr,
    then_branch: &[Stmt],
    else_branch: Option<&[Stmt]>,
) -> Result<tamago::If> {
    let mut b = IfBuilder::new(lower_expr(hir, cond)?).then(lower_body(hir, then_branch)?);
    if let Some(eb) = else_branch {
        b = b.other(lower_body(hir, eb)?);
    }
    Ok(b.build())
}

fn lower_for_init(hir: &Hir, s: &Stmt) -> Result<ForInit> {
    Ok(match s {
        Stmt::Let { name, ty, init, .. } => ForInit::Decl(
            VariableBuilder::new_with_str(name, lower_ty(ty)?)
                .value(lower_expr(hir, init)?)
                .build(),
        ),
        Stmt::Assign { target, op, value } => ForInit::Expr(tamago::Expr::new_assign(
            lower_expr(hir, target)?,
            assign_op(*op),
            lower_expr(hir, value)?,
        )),
        _ => return Err(CodegenError::new("unsupported for-init statement")),
    })
}

fn lower_for_step(hir: &Hir, s: &Stmt) -> Result<tamago::Expr> {
    Ok(match s {
        Stmt::Assign { target, op, value } => tamago::Expr::new_assign(
            lower_expr(hir, target)?,
            assign_op(*op),
            lower_expr(hir, value)?,
        ),
        Stmt::Expr(e) => lower_expr(hir, e)?,
        _ => return Err(CodegenError::new("unsupported for-post statement")),
    })
}

fn lower_expr(hir: &Hir, e: &Expr) -> Result<tamago::Expr> {
    use tamago::Expr as T;
    Ok(match &e.kind {
        ExprKind::Int(v) => T::Int(*v),
        ExprKind::Float(v) => T::Double(*v),
        ExprKind::Str(s) => T::Str(s.clone()),
        ExprKind::Char(c) => T::Char(*c),
        ExprKind::Bool(b) => T::Bool(*b),
        ExprKind::Name { def, name } => T::new_ident(c_name(hir, *def, name)),
        ExprKind::Unresolved(n) => {
            return Err(CodegenError::new(format!(
                "unresolved name `{n}` reached codegen"
            )))
        }
        ExprKind::Unary { op, operand } => T::new_unary(lower_expr(hir, operand)?, un_op(*op)),
        ExprKind::Binary { op, lhs, rhs } => {
            T::new_binary(lower_expr(hir, lhs)?, bin_op(*op), lower_expr(hir, rhs)?)
        }
        ExprKind::Call { callee, args } => {
            let a = args
                .iter()
                .map(|a| lower_expr(hir, a))
                .collect::<Result<Vec<_>>>()?;
            T::new_fn_call(lower_expr(hir, callee)?, a)
        }
        ExprKind::Field { recv, member } => {
            T::new_mem_access(lower_expr(hir, recv)?, member.clone())
        }
        ExprKind::Index { base, index } => {
            T::new_arr_index(lower_expr(hir, base)?, lower_expr(hir, index)?)
        }
        ExprKind::Cast { ty, operand } => T::new_cast(lower_ty(ty)?, lower_expr(hir, operand)?),
        ExprKind::Paren(inner) => T::new_parenthesized(lower_expr(hir, inner)?),
    })
}

/// The C name for a resolved reference: an extern's linked symbol, else the
/// Dray name (which is the C name for procs, params, and locals).
fn c_name(hir: &Hir, def: DefId, fallback: &str) -> String {
    match &hir.def(def).kind {
        DefKind::ExternProc { symbol } => symbol.clone(),
        _ => fallback.to_string(),
    }
}

fn lower_ty(t: &Ty) -> Result<Type> {
    use tamago::BaseType as B;
    Ok(match t {
        Ty::Void => Type::base(B::Void),
        Ty::Bool => Type::base(B::Bool),
        Ty::Int { bits, signed } => Type::base(int_base(*bits, *signed)),
        Ty::Float { bits } => Type::base(if *bits == 32 { B::Float } else { B::Double }),
        Ty::Ptr(inner) => Type::ptr(lower_ty(inner)?),
        Ty::Named(n) => Type::base(B::TypeDef(n.clone())),
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
