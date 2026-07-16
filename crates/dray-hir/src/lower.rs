// SPDX-License-Identifier: Apache-2.0

//! CST → HIR lowering: name resolution + best-effort type inference in one pass.

use std::collections::HashMap;

use dray_syntax::{Span, SyntaxElement, SyntaxKind, SyntaxNode};

use crate::hir::*;
use crate::types::{is_type, lower_type};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError {
    pub message: String,
    pub span: Span,
}

pub fn lower(root: &SyntaxNode) -> (Hir, Vec<ResolveError>) {
    let mut lw = Lowerer::new();
    lw.run(root);
    (
        Hir {
            items: lw.items,
            defs: lw.defs,
        },
        lw.errors,
    )
}

struct Scope {
    names: HashMap<String, DefId>,
}

impl Scope {
    fn new() -> Scope {
        Scope {
            names: HashMap::new(),
        }
    }
}

struct Lowerer {
    defs: Vec<DefInfo>,
    scopes: Vec<Scope>,
    items: Vec<Item>,
    errors: Vec<ResolveError>,
    structs: HashMap<String, Vec<Field>>,
    enums: HashMap<String, Vec<Variant>>,
}

impl Lowerer {
    fn new() -> Lowerer {
        Lowerer {
            defs: Vec::new(),
            scopes: vec![Scope::new()], // scopes[0] = module scope
            items: Vec::new(),
            errors: Vec::new(),
            structs: HashMap::new(),
            enums: HashMap::new(),
        }
    }

    fn run(&mut self, root: &SyntaxNode) {
        if root.kind() != SyntaxKind::SourceFile {
            self.err(root.span(), "expected a SourceFile root");
            return;
        }
        let decls = root.children();

        // Pass 1: register top-level proc/extern names for forward references
        for decl in &decls {
            match decl.kind() {
                SyntaxKind::ProcDef => {
                    if let Some(name) = first_ident(decl) {
                        let ret = self.return_type(decl);
                        let id = self.add_def(name.clone(), DefKind::Proc, ret);
                        self.bind_module(name, id);
                    }
                }
                SyntaxKind::ExternProcDecl => {
                    if let Some(name) = first_ident(decl) {
                        let symbol = self.extern_symbol(decl).unwrap_or_else(|| name.clone());
                        let ret = self.return_type(decl);
                        let id = self.add_def(name.clone(), DefKind::ExternProc { symbol }, ret);
                        self.bind_module(name, id);
                    }
                }
                SyntaxKind::StructDef => {
                    if let Some(name) = first_ident(decl) {
                        let fields = self.struct_fields(decl);
                        let id =
                            self.add_def(name.clone(), DefKind::Struct, Ty::Named(name.clone()));
                        self.bind_module(name.clone(), id);
                        self.structs.insert(name, fields);
                    }
                }
                SyntaxKind::EnumDef => {
                    if let Some(name) = first_ident(decl) {
                        let variants = self.enum_variants(decl);
                        let id = self.add_def(name.clone(), DefKind::Enum, Ty::Named(name.clone()));
                        self.bind_module(name.clone(), id);
                        self.enums.insert(name, variants);
                    }
                }
                _ => {}
            }
        }

        // Pass 2: lower each declaration
        for decl in &decls {
            match decl.kind() {
                SyntaxKind::CHeaderDecl => {
                    if let Some(h) = self.c_header(decl) {
                        self.items.push(Item::Include(h));
                    }
                }
                SyntaxKind::ProcDef => self.lower_proc(decl),
                SyntaxKind::ExternProcDecl => self.lower_extern(decl),
                SyntaxKind::StructDef => self.lower_struct(decl),
                SyntaxKind::EnumDef => self.lower_enum(decl),
                SyntaxKind::Error => {
                    self.err(
                        decl.span(),
                        "source has an Error node; fix parse errors first",
                    );
                }
                other => {
                    self.err(
                        decl.span(),
                        format!("top-level {other:?} is not lowered to HIR yet"),
                    );
                }
            }
        }
    }

    fn add_def(&mut self, name: String, kind: DefKind, ty: Ty) -> DefId {
        let id = DefId(self.defs.len() as u32);
        self.defs.push(DefInfo { name, kind, ty });
        id
    }

    fn bind_module(&mut self, name: String, id: DefId) {
        self.scopes[0].names.insert(name, id);
    }

    fn bind_local(&mut self, name: String, id: DefId) {
        self.scopes.last_mut().unwrap().names.insert(name, id);
    }

    fn push_scope(&mut self) {
        self.scopes.push(Scope::new());
    }
    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Resolve a name against the scope stack (innermost first, module last).
    fn resolve(&self, name: &str) -> Option<DefId> {
        for scope in self.scopes.iter().rev() {
            if let Some(&id) = scope.names.get(name) {
                return Some(id);
            }
        }
        None
    }

    fn lower_proc(&mut self, node: &SyntaxNode) {
        let name = match first_ident(node) {
            Some(n) => n,
            None => return,
        };
        let def = self.resolve(&name).unwrap(); // registered in pass 1
        let ret = self.return_type(node);

        self.push_scope();
        let params = self.lower_params(node);
        let body = match node.child_of_kind(SyntaxKind::Block) {
            Some(b) => self.lower_block(&b),
            None => {
                self.err(node.span(), "proc without a body");
                Vec::new()
            }
        };
        self.pop_scope();

        self.items.push(Item::Proc(Proc {
            def,
            name,
            params,
            ret,
            body,
        }));
    }

    fn lower_extern(&mut self, node: &SyntaxNode) {
        let name = match first_ident(node) {
            Some(n) => n,
            None => return,
        };
        let def = self.resolve(&name).unwrap();
        let symbol = self.extern_symbol(node).unwrap_or_else(|| name.clone());
        let ret = self.return_type(node);
        self.push_scope();
        let params = self.lower_params(node);
        self.pop_scope();
        self.items.push(Item::ExternProc(ExternProc {
            def,
            name,
            symbol,
            params,
            ret,
        }));
    }

    fn lower_params(&mut self, node: &SyntaxNode) -> Vec<Param> {
        let list = match node.child_of_kind(SyntaxKind::ParamList) {
            Some(l) => l,
            None => return Vec::new(),
        };
        let mut out = Vec::new();
        for p in list.children() {
            if p.kind() != SyntaxKind::Param {
                continue;
            }
            if p.token_of_kind(SyntaxKind::KwComptime).is_some() {
                self.err(
                    p.span(),
                    "comptime parameters need monomorphization (not in HIR yet)",
                );
                continue;
            }
            let pname = match first_ident(&p) {
                Some(n) => n,
                None => continue,
            };
            let ty = p
                .children()
                .into_iter()
                .find(|c| is_type(c.kind()))
                .and_then(|t| self.checked_type(&t))
                .unwrap_or(Ty::Infer);
            let id = self.add_def(pname.clone(), DefKind::Param, ty.clone());
            self.bind_local(pname.clone(), id);
            out.push(Param {
                def: id,
                name: pname,
                ty,
            });
        }
        out
    }

    // ── statements ───────────────────────────────────────────────────────────

    fn lower_block(&mut self, node: &SyntaxNode) -> Vec<Stmt> {
        self.push_scope();
        let mut out = Vec::new();
        for child in node.children() {
            if let Some(s) = self.lower_stmt(&child) {
                out.push(s);
            }
        }
        self.pop_scope();
        out
    }

    fn lower_stmt(&mut self, node: &SyntaxNode) -> Option<Stmt> {
        match node.kind() {
            SyntaxKind::VarDecl => self.lower_var_decl(node),
            SyntaxKind::AssignStmt => self.lower_assign(node),
            SyntaxKind::ReturnStmt => Some(Stmt::Return(
                self.first_expr(node).map(|e| self.lower_expr(&e)),
            )),
            SyntaxKind::BreakStmt => Some(Stmt::Break),
            SyntaxKind::ContinueStmt => Some(Stmt::Continue),
            SyntaxKind::ExprStmt => {
                let e = self.first_expr(node)?;
                Some(Stmt::Expr(self.lower_expr(&e)))
            }
            SyntaxKind::IfStmt => self.lower_if(node),
            SyntaxKind::ForStmt => self.lower_for(node),
            SyntaxKind::SwitchStmt => self.lower_switch(node),
            SyntaxKind::Block => {
                self.err(node.span(), "nested bare blocks are not lowered yet");
                None
            }
            _ => None,
        }
    }

    fn lower_var_decl(&mut self, node: &SyntaxNode) -> Option<Stmt> {
        let name = first_ident(node)?;
        let declared = node
            .children()
            .into_iter()
            .find(|c| is_type(c.kind()))
            .and_then(|t| self.checked_type(&t));
        let init_node = self.first_expr(node)?;
        let init = self.lower_expr(&init_node);
        let ty = declared.unwrap_or_else(|| default_ty(&init.ty));
        let id = self.add_def(name.clone(), DefKind::Local, ty.clone());
        self.bind_local(name.clone(), id);
        Some(Stmt::Let {
            def: id,
            name,
            ty,
            init,
        })
    }

    fn lower_assign(&mut self, node: &SyntaxNode) -> Option<Stmt> {
        let parts = self.expr_children(node);
        if parts.len() != 2 {
            self.err(node.span(), "assignment needs a target and a value");
            return None;
        }
        let op = assign_op(node)?;
        Some(Stmt::Assign {
            target: self.lower_expr(&parts[0]),
            op,
            value: self.lower_expr(&parts[1]),
        })
    }

    fn lower_if(&mut self, node: &SyntaxNode) -> Option<Stmt> {
        if node.child_of_kind(SyntaxKind::VarDecl).is_some() {
            self.err(node.span(), "if-init clauses are not lowered yet");
            return None;
        }
        let cond = self.condition(node)?;
        let then_branch = node
            .child_of_kind(SyntaxKind::Block)
            .map(|b| self.lower_block(&b))
            .unwrap_or_default();
        let else_branch = node.child_of_kind(SyntaxKind::ElseClause).and_then(|ec| {
            if let Some(inner_if) = ec.child_of_kind(SyntaxKind::IfStmt) {
                self.lower_if(&inner_if).map(|s| vec![s]) // else-if
            } else {
                ec.child_of_kind(SyntaxKind::Block)
                    .map(|b| self.lower_block(&b))
            }
        });
        Some(Stmt::If {
            cond,
            then_branch,
            else_branch,
        })
    }

    fn lower_for(&mut self, node: &SyntaxNode) -> Option<Stmt> {
        let is_range = node.token_of_kind(SyntaxKind::KwIn).is_some();
        if is_range {
            self.err(node.span(), "for-in range loops are not lowered yet");
            return None;
        }
        let has_semi = node
            .children_with_tokens()
            .iter()
            .any(|e| matches!(e, SyntaxElement::Token(t) if t.kind() == SyntaxKind::Semi));
        let has_cond = node.child_of_kind(SyntaxKind::Condition).is_some();

        if has_semi {
            // C-style. Open one scope for the whole loop so the init binding is
            // visible to the condition, the post, and the body. `lower_block`
            // opens a further nested scope for the body itself.
            self.push_scope();
            let init = self.for_init(node).map(Box::new);
            let cond = node
                .child_of_kind(SyntaxKind::Condition)
                .and_then(|c| self.first_expr(&c).map(|e| self.lower_expr(&e)));
            let post = self.for_post(node).map(Box::new);
            let body = node
                .child_of_kind(SyntaxKind::Block)
                .map(|b| self.lower_block(&b))
                .unwrap_or_default();
            self.pop_scope();
            Some(Stmt::CFor {
                init,
                cond,
                post,
                body,
            })
        } else {
            let body = node
                .child_of_kind(SyntaxKind::Block)
                .map(|b| self.lower_block(&b))
                .unwrap_or_default();
            if has_cond {
                let cond = self.condition(node)?;
                Some(Stmt::While { cond, body })
            } else {
                Some(Stmt::Loop { body })
            }
        }
    }

    /// The init statement of a C-style for (a VarDecl or AssignStmt), lowered
    /// without a trailing form. Binds into the current (loop) scope.
    fn for_init(&mut self, node: &SyntaxNode) -> Option<Stmt> {
        if let Some(vd) = node.child_of_kind(SyntaxKind::VarDecl) {
            self.lower_var_decl(&vd)
        } else {
            // an AssignStmt init: the first assignment child, if any
            node.children()
                .into_iter()
                .find(|c| c.kind() == SyntaxKind::AssignStmt)
                .and_then(|a| self.lower_assign(&a))
        }
    }

    /// The post statement of a C-style for: the assignment/expr after the cond.
    fn for_post(&mut self, node: &SyntaxNode) -> Option<Stmt> {
        let has_decl_init = node.child_of_kind(SyntaxKind::VarDecl).is_some();
        let stmts: Vec<SyntaxNode> = node
            .children()
            .into_iter()
            .filter(|c| matches!(c.kind(), SyntaxKind::AssignStmt | SyntaxKind::ExprStmt))
            .collect();
        let post = if has_decl_init || stmts.len() >= 2 {
            stmts.into_iter().next_back()
        } else {
            None
        };
        post.and_then(|p| self.lower_stmt(&p))
    }

    // ── expressions ──────────────────────────────────────────────────────────

    fn lower_expr(&mut self, node: &SyntaxNode) -> Expr {
        let span = node.span();
        let (kind, ty) = match node.kind() {
            SyntaxKind::LiteralExpr => self.lower_literal(node),
            SyntaxKind::NameExpr => self.lower_name(node),
            SyntaxKind::ParenExpr => {
                let inner = self.first_expr(node);
                match inner {
                    Some(e) => {
                        let le = self.lower_expr(&e);
                        let ty = le.ty.clone();
                        (ExprKind::Paren(Box::new(le)), ty)
                    }
                    None => (ExprKind::Unresolved("()".into()), Ty::Infer),
                }
            }
            SyntaxKind::PrefixExpr => self.lower_prefix(node),
            SyntaxKind::BinaryExpr => self.lower_binary(node),
            SyntaxKind::CallExpr => self.lower_call(node),
            SyntaxKind::FieldExpr => self.lower_field(node),
            SyntaxKind::IndexExpr => self.lower_index(node),
            SyntaxKind::CastExpr => self.lower_cast(node),
            SyntaxKind::AllocExpr => self.lower_alloc(node),
            other => {
                self.err(span, format!("unsupported expression {other:?}"));
                (ExprKind::Unresolved(format!("{other:?}")), Ty::Infer)
            }
        };
        Expr { kind, ty, span }
    }

    fn lower_literal(&mut self, node: &SyntaxNode) -> (ExprKind, Ty) {
        let tok = node
            .children_with_tokens()
            .into_iter()
            .find_map(|e| match e {
                SyntaxElement::Token(t) if !t.kind().is_trivia() => Some(t),
                _ => None,
            });
        let Some(tok) = tok else {
            return (ExprKind::Unresolved("lit".into()), Ty::Infer);
        };
        let text = tok.text();
        match tok.kind() {
            SyntaxKind::IntLit => match text.parse::<i64>() {
                Ok(v) => (ExprKind::Int(v), Ty::i32()),
                Err(_) => {
                    self.err(node.span(), format!("invalid integer `{text}`"));
                    (ExprKind::Unresolved(text.into()), Ty::i32())
                }
            },
            SyntaxKind::FloatLit => match text.parse::<f64>() {
                Ok(v) => (ExprKind::Float(v), Ty::f64()),
                Err(_) => {
                    self.err(node.span(), format!("invalid float `{text}`"));
                    (ExprKind::Unresolved(text.into()), Ty::f64())
                }
            },
            SyntaxKind::StringLit => (ExprKind::Str(unquote(text)), Ty::Ptr(Box::new(Ty::i8()))),
            SyntaxKind::RuneLit => match decode_rune(text) {
                Ok(c) => (ExprKind::Char(c), Ty::i8()),
                Err(m) => {
                    self.err(node.span(), m);
                    (ExprKind::Unresolved(text.into()), Ty::i8())
                }
            },
            SyntaxKind::KwTrue => (ExprKind::Bool(true), Ty::Bool),
            SyntaxKind::KwFalse => (ExprKind::Bool(false), Ty::Bool),
            _ => (ExprKind::Unresolved(text.into()), Ty::Infer),
        }
    }

    fn lower_name(&mut self, node: &SyntaxNode) -> (ExprKind, Ty) {
        let name = ident_text(node);
        match self.resolve(&name) {
            Some(def) => {
                let ty = self.defs[def.0 as usize].ty.clone();
                (ExprKind::Name { def, name }, ty)
            }
            None => {
                self.err(node.span(), format!("cannot find `{name}` in this scope"));
                (ExprKind::Unresolved(name), Ty::Infer)
            }
        }
    }

    fn lower_prefix(&mut self, node: &SyntaxNode) -> (ExprKind, Ty) {
        let op = leading_op(node).and_then(un_op);
        let operand = self.first_expr(node).map(|e| self.lower_expr(&e));
        match (op, operand) {
            (Some(op), Some(operand)) => {
                let ty = match op {
                    UnOp::LogicNot => Ty::Bool,
                    UnOp::AddrOf => Ty::Ptr(Box::new(operand.ty.clone())),
                    UnOp::Deref => match &operand.ty {
                        Ty::Ptr(inner) | Ty::Rc(inner) => (**inner).clone(),
                        _ => Ty::Infer,
                    },
                    UnOp::Neg | UnOp::BitNot => operand.ty.clone(),
                };
                (
                    ExprKind::Unary {
                        op,
                        operand: Box::new(operand),
                    },
                    ty,
                )
            }
            _ => (ExprKind::Unresolved("prefix".into()), Ty::Infer),
        }
    }

    fn lower_binary(&mut self, node: &SyntaxNode) -> (ExprKind, Ty) {
        let parts = self.expr_children(node);
        if parts.len() != 2 {
            return (ExprKind::Unresolved("binary".into()), Ty::Infer);
        }
        let op = match middle_op(node).and_then(bin_op) {
            Some(op) => op,
            None => return (ExprKind::Unresolved("binop".into()), Ty::Infer),
        };
        let lhs = self.lower_expr(&parts[0]);
        let rhs = self.lower_expr(&parts[1]);
        let ty = if op.is_boolean() {
            Ty::Bool
        } else {
            lhs.ty.clone()
        };
        (
            ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            ty,
        )
    }

    fn lower_call(&mut self, node: &SyntaxNode) -> (ExprKind, Ty) {
        let callee_node = self.first_expr(node);

        // `Enum.Variant(args)` enum construction, not an ordinary call.
        if let Some(cn) = &callee_node {
            if let Some((enum_name, variant)) = self.enum_variant_ref(cn) {
                let args = self.call_args(node);
                return (
                    ExprKind::EnumInit {
                        enum_name: enum_name.clone(),
                        variant,
                        args,
                    },
                    Ty::Named(enum_name),
                );
            }
        }

        let callee = match callee_node {
            Some(c) => self.lower_expr(&c),
            None => return (ExprKind::Unresolved("call".into()), Ty::Infer),
        };
        let args = self.call_args(node);
        let ty = callee.ty.clone();
        (
            ExprKind::Call {
                callee: Box::new(callee),
                args,
            },
            ty,
        )
    }

    fn call_args(&mut self, node: &SyntaxNode) -> Vec<Expr> {
        node.child_of_kind(SyntaxKind::ArgList)
            .map(|al| {
                self.expr_children(&al)
                    .iter()
                    .map(|a| self.lower_expr(a))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    fn lower_field(&mut self, node: &SyntaxNode) -> (ExprKind, Ty) {
        // `Enum.Variant` with no call is a unit-variant construction.
        if let Some((enum_name, variant)) = self.enum_variant_ref(node) {
            return (
                ExprKind::EnumInit {
                    enum_name: enum_name.clone(),
                    variant,
                    args: Vec::new(),
                },
                Ty::Named(enum_name),
            );
        }
        let recv = match self.first_expr(node) {
            Some(r) => self.lower_expr(&r),
            None => return (ExprKind::Unresolved("field".into()), Ty::Infer),
        };
        let member = node
            .token_of_kind(SyntaxKind::Ident)
            .map(|t| t.text().to_string())
            .unwrap_or_default();
        let ty = match struct_name_of(&recv.ty) {
            Some(sname) => self
                .structs
                .get(&sname)
                .and_then(|fs| fs.iter().find(|f| f.name == member))
                .map(|f| f.ty.clone())
                .unwrap_or(Ty::Infer),
            None => Ty::Infer,
        };
        (
            ExprKind::Field {
                recv: Box::new(recv),
                member,
            },
            ty,
        )
    }

    fn enum_variant_ref(&self, node: &SyntaxNode) -> Option<(String, String)> {
        if node.kind() != SyntaxKind::FieldExpr {
            return None;
        }
        let recv = self.first_expr(node)?;
        if recv.kind() != SyntaxKind::NameExpr {
            return None;
        }
        let enum_name = ident_text(&recv);
        let variant = node.token_of_kind(SyntaxKind::Ident)?.text().to_string();
        let variants = self.enums.get(&enum_name)?;
        if variants.iter().any(|v| v.name == variant) {
            Some((enum_name, variant))
        } else {
            None
        }
    }

    fn lower_index(&mut self, node: &SyntaxNode) -> (ExprKind, Ty) {
        let parts = self.expr_children(node);
        if parts.len() != 2 {
            return (ExprKind::Unresolved("index".into()), Ty::Infer);
        }
        let base = self.lower_expr(&parts[0]);
        let index = self.lower_expr(&parts[1]);
        let ty = match &base.ty {
            Ty::Ptr(inner) => (**inner).clone(),
            _ => Ty::Infer,
        };
        (
            ExprKind::Index {
                base: Box::new(base),
                index: Box::new(index),
            },
            ty,
        )
    }

    fn lower_cast(&mut self, node: &SyntaxNode) -> (ExprKind, Ty) {
        let ty_node = node.children().into_iter().find(|c| is_type(c.kind()));
        let operand = node
            .children()
            .into_iter()
            .find(|c| !is_type(c.kind()) && is_expr(c.kind()));
        match (ty_node.and_then(|t| self.checked_type(&t)), operand) {
            (Some(ty), Some(op)) => {
                let operand = self.lower_expr(&op);
                (
                    ExprKind::Cast {
                        ty: ty.clone(),
                        operand: Box::new(operand),
                    },
                    ty,
                )
            }
            _ => {
                self.err(node.span(), "malformed or unsupported cast");
                (ExprKind::Unresolved("cast".into()), Ty::Infer)
            }
        }
    }

    // ── small helpers ────────────────────────────────────────────────────────

    /// Resolve the fields of a `StructDef` CST node to `(name, Ty)` pairs.
    fn struct_fields(&mut self, node: &SyntaxNode) -> Vec<Field> {
        let mut fields = Vec::new();
        for fd in node.children() {
            if fd.kind() != SyntaxKind::FieldDecl {
                continue;
            }
            let name = ident_text(&fd);
            let ty = fd
                .children()
                .into_iter()
                .find(|c| is_type(c.kind()))
                .and_then(|t| lower_type(&t))
                .unwrap_or(Ty::Infer);
            fields.push(Field { name, ty });
        }
        fields
    }

    fn lower_struct(&mut self, node: &SyntaxNode) {
        let name = match first_ident(node) {
            Some(n) => n,
            None => return,
        };
        let def = self.resolve(&name).unwrap_or(DefId(0));
        let fields = self.structs.get(&name).cloned().unwrap_or_default();
        let type_params = comptime_type_params(node);
        for f in &fields {
            if type_params.contains(&f.name) {
                self.err(
                    node.span(),
                    format!(
                        "field `{}` collides with the comptime type parameter of the same name",
                        f.name
                    ),
                );
            }
        }
        self.items.push(Item::Struct(StructDef {
            def,
            name,
            type_params,
            fields,
        }));
    }

    /// Resolve an `EnumDef` CST node's variants to `(name, payload types)`
    fn enum_variants(&mut self, node: &SyntaxNode) -> Vec<Variant> {
        let mut variants = Vec::new();
        for v in node.children() {
            if v.kind() != SyntaxKind::EnumVariant {
                continue;
            }
            let name = ident_text(&v);
            let payload = v
                .child_of_kind(SyntaxKind::TypeList)
                .map(|tl| {
                    tl.children()
                        .into_iter()
                        .filter(|c| is_type(c.kind()))
                        .filter_map(|t| lower_type(&t))
                        .collect()
                })
                .unwrap_or_default();
            variants.push(Variant { name, payload });
        }
        variants
    }

    fn lower_enum(&mut self, node: &SyntaxNode) {
        let name = match first_ident(node) {
            Some(n) => n,
            None => return,
        };
        let def = self.resolve(&name).unwrap_or(DefId(0));
        let variants = self.enums.get(&name).cloned().unwrap_or_default();
        self.items.push(Item::Enum(EnumDef {
            def,
            name,
            variants,
        }));
    }

    /// `switch scrutinee { case Pat: … }` → `Stmt::Switch`.
    fn lower_switch(&mut self, node: &SyntaxNode) -> Option<Stmt> {
        let scrutinee = self.first_expr(node).map(|e| self.lower_expr(&e))?;
        let mut arms = Vec::new();
        for clause in node.children() {
            if clause.kind() != SyntaxKind::CaseClause {
                continue;
            }
            arms.push(self.lower_case(&clause, &scrutinee.ty));
        }
        Some(Stmt::Switch { scrutinee, arms })
    }

    fn lower_case(&mut self, clause: &SyntaxNode, scrut_ty: &Ty) -> Arm {
        // A case has one pattern (multi-pattern lists are a later refinement).
        let pattern = if let Some(pat) = clause.child_of_kind(SyntaxKind::EnumPattern) {
            self.lower_enum_pattern(&pat)
        } else if let Some(e) = self.first_expr(clause) {
            Pattern::Value(self.lower_expr(&e))
        } else {
            Pattern::Value(Expr {
                kind: ExprKind::Unresolved("pattern".into()),
                ty: Ty::Infer,
                span: clause.span(),
            })
        };

        // Bind the enum pattern's payload identifiers as locals typed by the
        // matched variant's payload, so the arm body can use them
        self.push_scope();
        if let Pattern::Enum {
            enum_name,
            variant,
            bindings,
        } = &pattern
        {
            let payload = self
                .enums
                .get(enum_name)
                .and_then(|vs| vs.iter().find(|v| &v.name == variant))
                .map(|v| v.payload.clone())
                .unwrap_or_default();
            for (i, b) in bindings.iter().enumerate() {
                let ty = payload.get(i).cloned().unwrap_or(Ty::Infer);
                let id = self.add_def(b.clone(), DefKind::Local, ty);
                self.bind_local(b.clone(), id);
            }
        }
        let _ = scrut_ty;
        let body = self.lower_block(clause);
        self.pop_scope();
        Arm { pattern, body }
    }

    fn lower_enum_pattern(&mut self, pat: &SyntaxNode) -> Pattern {
        let idents: Vec<String> = pat
            .children_with_tokens()
            .into_iter()
            .filter_map(|e| match e {
                SyntaxElement::Token(t) if t.kind() == SyntaxKind::Ident => {
                    Some(t.text().to_string())
                }
                _ => None,
            })
            .collect();
        // `Enum . Variant ( b0, b1, ... )` — first two idents are the type + variant.
        let enum_name = idents.first().cloned().unwrap_or_default();
        let variant = idents.get(1).cloned().unwrap_or_default();
        let bindings = idents.iter().skip(2).cloned().collect();
        Pattern::Enum {
            enum_name,
            variant,
            bindings,
        }
    }

    /// Lower `alloc T` or `alloc T{ field: value, ... }`. The composite form
    /// carries per-field initializers.
    fn lower_alloc(&mut self, node: &SyntaxNode) -> (ExprKind, Ty) {
        if node.token_of_kind(SyntaxKind::KwTryAlloc).is_some() {
            self.err(node.span(), "try_alloc is not lowered yet; use `alloc`");
            return (ExprKind::Unresolved("try_alloc".into()), Ty::Infer);
        }

        // Composite form: the child is a CompositeLit wrapping the type + fields.
        if let Some(lit) = node.child_of_kind(SyntaxKind::CompositeLit) {
            return self.lower_composite_alloc(&lit);
        }

        // Bare form: `alloc T`, zero-initialized.
        let ty_node = node.children().into_iter().find(|c| is_type(c.kind()));
        match ty_node.and_then(|t| self.checked_type(&t)) {
            Some(inner) => (
                ExprKind::Alloc {
                    ty: inner.clone(),
                    fields: Vec::new(),
                },
                Ty::Rc(Box::new(inner)),
            ),
            None => {
                self.err(node.span(), "alloc needs a type");
                (ExprKind::Unresolved("alloc".into()), Ty::Infer)
            }
        }
    }

    /// Lower a `CompositeLit` sitting under an `alloc`: resolve the struct type,
    /// then each `field: value` element (checking the field exists).
    fn lower_composite_alloc(&mut self, lit: &SyntaxNode) -> (ExprKind, Ty) {
        let ty_node = lit.children().into_iter().find(|c| is_type(c.kind()));
        let ty = match ty_node.and_then(|t| self.checked_type(&t)) {
            Some(t) => t,
            None => {
                self.err(lit.span(), "composite literal needs a type");
                return (ExprKind::Unresolved("composite".into()), Ty::Infer);
            }
        };
        let struct_name = match &ty {
            Ty::Named(n) => n.clone(),
            Ty::App(n, _) => n.clone(),
            _ => {
                self.err(lit.span(), "only structs can be built with `{ ... }`");
                return (ExprKind::Unresolved("composite".into()), Ty::Infer);
            }
        };
        let known: Vec<String> = self
            .structs
            .get(&struct_name)
            .map(|fs| fs.iter().map(|f| f.name.clone()).collect())
            .unwrap_or_default();

        let mut fields = Vec::new();
        for el in lit.children() {
            if el.kind() != SyntaxKind::Element {
                continue;
            }
            let fname = ident_text(&el);
            if !known.is_empty() && !known.contains(&fname) {
                self.err(el.span(), format!("`{struct_name}` has no field `{fname}`"));
            }
            let value = match self.first_expr(&el) {
                Some(e) => self.lower_expr(&e),
                None => continue,
            };
            fields.push((fname, value));
        }
        (
            ExprKind::Alloc {
                ty: ty.clone(),
                fields,
            },
            Ty::Rc(Box::new(ty)),
        )
    }

    fn checked_type(&mut self, node: &SyntaxNode) -> Option<Ty> {
        match lower_type(node) {
            Some(t) => Some(t),
            None => {
                self.err(
                    node.span(),
                    format!("{:?} types are not lowered to HIR yet", node.kind()),
                );
                None
            }
        }
    }

    fn condition(&mut self, node: &SyntaxNode) -> Option<Expr> {
        let cond = node.child_of_kind(SyntaxKind::Condition)?;
        let e = self.first_expr(&cond)?;
        Some(self.lower_expr(&e))
    }

    fn return_type(&mut self, node: &SyntaxNode) -> Ty {
        match node.child_of_kind(SyntaxKind::RetType) {
            Some(rt) => rt
                .children()
                .into_iter()
                .find(|c| is_type(c.kind()))
                .and_then(|t| lower_type(&t))
                .unwrap_or(Ty::Infer),
            None => Ty::Void,
        }
    }

    fn extern_symbol(&self, node: &SyntaxNode) -> Option<String> {
        node.token_of_kind(SyntaxKind::StringLit)
            .map(|t| unquote(t.text()))
    }

    fn c_header(&mut self, node: &SyntaxNode) -> Option<String> {
        match node.token_of_kind(SyntaxKind::StringLit) {
            Some(t) => Some(unquote(t.text())),
            None => {
                self.err(node.span(), "c_header without a header string");
                None
            }
        }
    }

    fn first_expr(&self, node: &SyntaxNode) -> Option<SyntaxNode> {
        node.children().into_iter().find(|c| is_expr(c.kind()))
    }

    fn expr_children(&self, node: &SyntaxNode) -> Vec<SyntaxNode> {
        node.children()
            .into_iter()
            .filter(|c| is_expr(c.kind()))
            .collect()
    }

    fn err(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(ResolveError {
            message: message.into(),
            span,
        });
    }
}

// ── free helpers ─────────────────────────────────────────────────────────────

/// The int32 default for an inferred binding whose init type is unusable.
fn default_ty(init_ty: &Ty) -> Ty {
    match init_ty {
        Ty::Infer | Ty::Void => Ty::i32(),
        other => other.clone(),
    }
}

fn is_expr(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::LiteralExpr
            | SyntaxKind::NameExpr
            | SyntaxKind::ParenExpr
            | SyntaxKind::PrefixExpr
            | SyntaxKind::BinaryExpr
            | SyntaxKind::CallExpr
            | SyntaxKind::FieldExpr
            | SyntaxKind::IndexExpr
            | SyntaxKind::CastExpr
            | SyntaxKind::AllocExpr
    )
}

fn first_ident(node: &SyntaxNode) -> Option<String> {
    node.token_of_kind(SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

/// (`Box(comptime T: type)` → `["T"]`). Empty when there is no clause.
fn comptime_type_params(node: &SyntaxNode) -> Vec<String> {
    node.child_of_kind(SyntaxKind::ParamList)
        .map(|pl| {
            pl.children()
                .into_iter()
                .filter(|p| {
                    p.kind() == SyntaxKind::Param
                        && p.token_of_kind(SyntaxKind::KwComptime).is_some()
                })
                .filter_map(|p| {
                    p.token_of_kind(SyntaxKind::Ident)
                        .map(|t| t.text().to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn struct_name_of(ty: &Ty) -> Option<String> {
    match ty {
        Ty::Named(n) => Some(n.clone()),
        Ty::Rc(inner) | Ty::Ptr(inner) => match &**inner {
            Ty::Named(n) => Some(n.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn ident_text(node: &SyntaxNode) -> String {
    node.token_of_kind(SyntaxKind::Ident)
        .map(|t| t.text().to_string())
        .unwrap_or_else(|| node.text().trim().to_string())
}

fn leading_op(node: &SyntaxNode) -> Option<String> {
    node.children_with_tokens()
        .into_iter()
        .find_map(|e| match e {
            SyntaxElement::Token(t) if !t.kind().is_trivia() => Some(t.text().to_string()),
            _ => None,
        })
}

fn middle_op(node: &SyntaxNode) -> Option<String> {
    for el in node.children_with_tokens() {
        if let SyntaxElement::Token(t) = el
            && !t.kind().is_trivia()
        {
            return Some(t.text().to_string());
        }
    }
    None
}

fn un_op(glyph: String) -> Option<UnOp> {
    Some(match glyph.as_str() {
        "-" => UnOp::Neg,
        "!" => UnOp::LogicNot,
        "~" => UnOp::BitNot,
        "&" => UnOp::AddrOf,
        "*" => UnOp::Deref,
        _ => return None,
    })
}

fn bin_op(glyph: String) -> Option<BinOp> {
    Some(match glyph.as_str() {
        "+" => BinOp::Add,
        "-" => BinOp::Sub,
        "*" => BinOp::Mul,
        "/" => BinOp::Div,
        "%" => BinOp::Rem,
        "==" => BinOp::Eq,
        "!=" => BinOp::Ne,
        "<" => BinOp::Lt,
        "<=" => BinOp::Le,
        ">" => BinOp::Gt,
        ">=" => BinOp::Ge,
        "&&" => BinOp::And,
        "||" => BinOp::Or,
        "&" => BinOp::BitAnd,
        "|" => BinOp::BitOr,
        "^" => BinOp::BitXor,
        "<<" => BinOp::Shl,
        ">>" => BinOp::Shr,
        _ => return None,
    })
}

fn assign_op(node: &SyntaxNode) -> Option<AssignOp> {
    for el in node.children_with_tokens() {
        if let SyntaxElement::Token(t) = el {
            let op = match t.kind() {
                SyntaxKind::Eq => AssignOp::Assign,
                SyntaxKind::PlusEq => AssignOp::Add,
                SyntaxKind::MinusEq => AssignOp::Sub,
                SyntaxKind::StarEq => AssignOp::Mul,
                SyntaxKind::SlashEq => AssignOp::Div,
                SyntaxKind::PercentEq => AssignOp::Rem,
                SyntaxKind::AmpEq => AssignOp::BitAnd,
                SyntaxKind::PipeEq => AssignOp::BitOr,
                SyntaxKind::CaretEq => AssignOp::BitXor,
                SyntaxKind::ShlEq => AssignOp::Shl,
                SyntaxKind::ShrEq => AssignOp::Shr,
                _ => continue,
            };
            return Some(op);
        }
    }
    None
}

fn unquote(text: &str) -> String {
    let inner = text
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(text);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('x') => {
                let hi = chars.next();
                let lo = chars.next();
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        if let Ok(byte) = u8::from_str_radix(&format!("{h}{l}"), 16) {
                            out.push(byte as char);
                        }
                    }
                    _ => {}
                }
            }
            // Unknown escape: keep it verbatim rather than dropping the backslash.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn decode_rune(text: &str) -> std::result::Result<char, String> {
    let inner = text
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .ok_or_else(|| format!("malformed rune `{text}`"))?;
    let mut chars = inner.chars();
    let first = chars.next().ok_or_else(|| "empty rune".to_string())?;
    if first != '\\' {
        return Ok(first);
    }
    Ok(
        match chars.next().ok_or_else(|| "dangling escape".to_string())? {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '0' => '\0',
            '\\' => '\\',
            '\'' => '\'',
            '"' => '"',
            other => return Err(format!("unsupported rune escape \\{other}")),
        },
    )
}
