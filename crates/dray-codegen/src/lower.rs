// SPDX-License-Identifier: Apache-2.0

//! The IR → Tamago lowering. Consumes RC-annotated IR (`dray-ir`); expressions
//! and types are still HIR's (reused unchanged), so only statements gain the RC
//! operations and `alloc`.

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

    if ir.uses_rc {
        scope = scope.new_line();
        scope = scope.global_statement(GlobalStatement::Raw(crate::RC_RUNTIME.to_string()));
    }

    for gs in struct_globals(ir)? {
        scope = scope.new_line();
        scope = scope.global_statement(gs);
    }

    for gs in enum_globals(ir)? {
        scope = scope.new_line();
        scope = scope.global_statement(gs);
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
        fb = fb.param(ParameterBuilder::new_with_str(&p.name, lower_ty(&p.ty)?).build());
    }
    Ok(fb.build())
}

fn lower_proc(ir: &Ir, p: &Proc) -> Result<tamago::Function> {
    let mut fb = FunctionBuilder::new_with_str(&p.name, lower_ty(&p.ret)?);
    for param in &p.params {
        fb = fb.param(ParameterBuilder::new_with_str(&param.name, lower_ty(&param.ty)?).build());
    }
    fb = fb.body(lower_body(ir, &p.body)?);
    Ok(fb.build())
}

fn lower_body(ir: &Ir, stmts: &[Stmt]) -> Result<Block> {
    let mut b = BlockBuilder::new();
    for s in stmts {
        b = b.statement(lower_stmt(ir, s)?);
    }
    Ok(b.build())
}

fn lower_stmt(ir: &Ir, s: &Stmt) -> Result<tamago::Statement> {
    use tamago::Statement;
    Ok(match s {
        Stmt::Let { name, ty, init } => Statement::Variable(
            VariableBuilder::new_with_str(name, lower_ty(ty)?)
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
        // RC operations: plain calls into the emitted runtime.
        Stmt::Retain(name) => Statement::Expr(rc_call("dray_rc_retain", name)),
        Stmt::Release(name) | Stmt::Free(name) => Statement::Expr(rc_call("dray_rc_release", name)),
        Stmt::Switch { scrutinee, arms } => lower_switch(ir, scrutinee, arms)?,
    })
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
                    let ty = payload.get(i).cloned().unwrap_or(Ty::Infer);
                    let field =
                        T::new_mem_access(lower_expr(ir, scrutinee)?, payload_field(variant, i));
                    b = b.statement(tamago::Statement::Variable(
                        VariableBuilder::new_with_str(bind, lower_ty(&ty)?)
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
        vec![T::new_ident(arg.to_string())],
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
            VariableBuilder::new_with_str(name, lower_ty(ty)?)
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
            let base = if matches!(recv.ty, Ty::Rc(_) | Ty::Ptr(_)) {
                T::new_parenthesized(T::new_unary(base, tamago::UnaryOp::Deref))
            } else {
                base
            };
            T::new_mem_access(base, member.clone())
        }
        ExprKind::Index { base, index } => {
            T::new_arr_index(lower_expr(ir, base)?, lower_expr(ir, index)?)
        }
        ExprKind::Cast { ty, operand } => T::new_cast(lower_ty(ty)?, lower_expr(ir, operand)?),
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
        // Both raw and RC pointers are a C `T*`; the RC bookkeeping is separate.
        Ty::Ptr(inner) | Ty::Rc(inner) => Type::ptr(lower_ty(inner)?),
        Ty::Named(n) => Type::base(B::Struct(n.clone())),
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
pub(crate) fn struct_globals(ir: &Ir) -> Result<Vec<GlobalStatement>> {
    let mut out = Vec::new();

    // Forward declarations first, so self- and mutual references resolve.
    for sd in &ir.structs {
        out.push(GlobalStatement::Struct(
            StructBuilder::new_with_str(&sd.name)
                .forward_declaration()
                .build(),
        ));
    }
    // Definitions.
    for sd in &ir.structs {
        let mut sb = StructBuilder::new_with_str(&sd.name);
        for f in &sd.fields {
            sb = sb.field(FieldBuilder::new_with_str(&f.name, lower_ty(&f.ty)?).build());
        }
        out.push(GlobalStatement::Struct(sb.define().build()));
    }
    // Drop glue: release each owned `@T` field (what makes freeing recursive).
    for sd in &ir.structs {
        if has_rc_field(sd) {
            out.push(GlobalStatement::Function(drop_fn(sd)?));
        }
    }
    // Constructors.
    for sd in &ir.structs {
        out.push(GlobalStatement::Function(constructor_fn(sd)?));
    }
    Ok(out)
}

/// `void dray_drop_T(void *p) { T *self = (T *)p; dray_rc_release(self->f); ... }`
fn drop_fn(sd: &dray_ir::StructDef) -> Result<tamago::Function> {
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
        if matches!(f.ty, Ty::Rc(_)) {
            body = body.statement(tamago::Statement::Expr(tamago::Expr::new_fn_call(
                tamago::Expr::new_ident_with_str("dray_rc_release"),
                vec![self_field("self", &f.name)],
            )));
        }
    }
    Ok(FunctionBuilder::new_with_str(
        &format!("dray_drop_{}", sd.name),
        Type::base(BaseType::Void),
    )
    .param(ParameterBuilder::new_with_str("p", Type::ptr(Type::base(BaseType::Void))).build())
    .body(body.build())
    .build())
}

/// `T *dray_new_T(f0 t0, ...) { T *self = (T *)dray_rc_alloc(sizeof(T), drop);
///  self->f0 = t0; ...; return self; }`
fn constructor_fn(sd: &dray_ir::StructDef) -> Result<tamago::Function> {
    let struct_ty = Type::base(BaseType::Struct(sd.name.clone()));
    let self_ty = Type::ptr(struct_ty.clone());
    let drop_arg = if has_rc_field(sd) {
        tamago::Expr::new_ident(format!("dray_drop_{}", sd.name))
    } else {
        tamago::Expr::Int(0)
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

    let mut fb = FunctionBuilder::new_with_str(&format!("dray_new_{}", sd.name), self_ty);
    for f in &sd.fields {
        fb = fb.param(ParameterBuilder::new_with_str(&f.name, lower_ty(&f.ty)?).build());
    }
    Ok(fb.body(body.build()).build())
}

/// `(*base).field` — field access through a struct pointer.
fn self_field(base: &str, field: &str) -> tamago::Expr {
    tamago::Expr::new_mem_access(
        tamago::Expr::new_parenthesized(tamago::Expr::new_unary(
            tamago::Expr::new_ident_with_str(base),
            tamago::UnaryOp::Deref,
        )),
        field.to_string(),
    )
}

fn has_rc_field(sd: &dray_ir::StructDef) -> bool {
    sd.fields.iter().any(|f| matches!(f.ty, Ty::Rc(_)))
}

/// Build the C globals for every enum with Tamago's typed builders: a tag `enum`,
/// a wrapper `struct` (the tag plus a flat set of payload fields, one per payload
/// slot of each variant), and a constructor `dray_new_Enum_Variant` per variant.
///
/// The layout is deliberately flat (payloads side-by-side, not overlapped in a
/// union) — simple and correct; a union to reclaim the space is a later
/// refinement. Only the active variant's fields are ever read, guarded by `tag`.
pub(crate) fn enum_globals(ir: &Ir) -> Result<Vec<GlobalStatement>> {
    let mut out = Vec::new();
    for ed in &ir.enums {
        // Tag enum: `enum Name_Tag { Name_V0, Name_V1, ... };`
        let mut eb = EnumBuilder::new_with_str(&format!("{}_Tag", ed.name));
        for v in &ed.variants {
            eb = eb.variant(VariantBuilder::new_with_str(&tag_const(&ed.name, &v.name)).build());
        }
        out.push(GlobalStatement::Enum(eb.build()));

        // Wrapper struct: `struct Name { enum Name_Tag tag; <payload fields> };`
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
        out.push(GlobalStatement::Struct(sb.define().build()));

        // Constructor per variant.
        for v in &ed.variants {
            out.push(GlobalStatement::Function(enum_ctor(ed, v)?));
        }
    }
    Ok(out)
}

/// `Name dray_new_Name_V(t0 f0, ...) { Name self; self.tag = Name_V;
///  self.V_f0 = f0; ...; return self; }`
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

    let mut fb = FunctionBuilder::new_with_str(&enum_ctor_name(&ed.name, &v.name), struct_ty);
    for (i, ty) in v.payload.iter().enumerate() {
        fb = fb.param(ParameterBuilder::new_with_str(&format!("f{i}"), lower_ty(ty)?).build());
    }
    Ok(fb.body(body.build()).build())
}
