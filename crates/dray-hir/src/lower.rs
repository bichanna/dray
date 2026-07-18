// SPDX-License-Identifier: Apache-2.0

//! CST → HIR lowering: name resolution + best-effort type inference in one pass.

use std::collections::HashMap;

use dray_syntax::{Span, SyntaxElement, SyntaxKind, SyntaxNode};

use crate::hir::*;
use crate::types::{is_type, lower_type, name_to_ty};

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
    enum_type_params: HashMap<String, Vec<String>>,
    /// Every declared type's comptime-parameter count, so type references can be
    /// checked for existence and correct arity (0 = a non-generic type).
    type_arity: HashMap<String, usize>,
    /// Each proc's runtime parameter count, to check call arity
    proc_arity: HashMap<String, usize>,
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
            enum_type_params: HashMap::new(),
            type_arity: HashMap::new(),
            proc_arity: HashMap::new(),
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
                        self.proc_arity
                            .insert(name.clone(), runtime_param_count(decl));
                        let id = self.add_def(name.clone(), DefKind::Proc, ret);
                        self.bind_module(name, id, decl.span());
                    }
                }
                SyntaxKind::ExternProcDecl => {
                    if let Some(name) = first_ident(decl) {
                        let symbol = self.extern_symbol(decl).unwrap_or_else(|| name.clone());
                        let ret = self.return_type(decl);
                        self.proc_arity
                            .insert(name.clone(), runtime_param_count(decl));
                        let id = self.add_def(name.clone(), DefKind::ExternProc { symbol }, ret);
                        self.bind_module(name, id, decl.span());
                    }
                }
                SyntaxKind::StructDef => {
                    if let Some(name) = first_ident(decl) {
                        let fields = self.struct_fields(decl);
                        let id =
                            self.add_def(name.clone(), DefKind::Struct, Ty::Named(name.clone()));
                        self.bind_module(name.clone(), id, decl.span());
                        self.type_arity
                            .insert(name.clone(), comptime_type_params(decl).len());
                        self.structs.insert(name, fields);
                    }
                }
                SyntaxKind::EnumDef => {
                    if let Some(name) = first_ident(decl) {
                        let variants = self.enum_variants(decl);
                        let id = self.add_def(name.clone(), DefKind::Enum, Ty::Named(name.clone()));
                        self.bind_module(name.clone(), id, decl.span());
                        let tps = comptime_type_params(decl);
                        self.type_arity.insert(name.clone(), tps.len());
                        self.enum_type_params.insert(name.clone(), tps);
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

    fn bind_module(&mut self, name: String, id: DefId, span: Span) {
        if self.scopes[0].names.contains_key(&name) {
            self.err(span, format!("`{name}` is declared more than once"));
        }
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

        self.validate_decl_types(node, &[]);

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
        let names: Vec<String> = out.iter().map(|p| p.name.clone()).collect();
        self.check_duplicates(names.iter().map(String::as_str), "parameter", list.span());
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
                // `static_assert(cond, "msg")` is a compile-time builtin statement,
                // not an ordinary call expression.
                if let Some(st) = self.lower_static_assert(&e) {
                    return Some(st);
                }
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
        if let Some(cn) = &callee_node
            && let Some((enum_name, variant, type_args)) = self.enum_variant_ref(cn)
        {
            let args = self.call_args(node);
            self.check_variant(&enum_name, &variant, args.len(), node.span());
            let ty = enum_instance_ty(&enum_name, type_args);
            return (
                ExprKind::EnumInit {
                    enum_name,
                    variant,
                    args,
                },
                ty,
            );
        }

        if let Some(cn) = &callee_node
            && cn.kind() == SyntaxKind::NameExpr
            && ident_text(cn) == "sizeof"
        {
            return self.lower_sizeof(node);
        }

        let callee = match callee_node {
            Some(c) => self.lower_expr(&c),
            None => return (ExprKind::Unresolved("call".into()), Ty::Infer),
        };
        let args = self.call_args(node);
        // If the callee names a known proc, its argument count must match.
        if let ExprKind::Name { name, .. } = &callee.kind
            && let Some(&arity) = self.proc_arity.get(name)
            && args.len() != arity
        {
            self.err(
                node.span(),
                format!(
                    "proc `{name}` takes {arity} argument(s), but {} were given",
                    args.len()
                ),
            );
        }
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
        if let Some((enum_name, variant, type_args)) = self.enum_variant_ref(node) {
            self.check_variant(&enum_name, &variant, 0, node.span());
            let ty = enum_instance_ty(&enum_name, type_args);
            return (
                ExprKind::EnumInit {
                    enum_name,
                    variant,
                    args: Vec::new(),
                },
                ty,
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
            Some(sname) => match self.structs.get(&sname) {
                Some(fields) => match fields.iter().find(|f| f.name == member) {
                    Some(f) => f.ty.clone(),
                    None => {
                        self.err(
                            node.span(),
                            format!("struct `{sname}` has no field `{member}`"),
                        );
                        Ty::Infer
                    }
                },
                None => Ty::Infer,
            },
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

    fn enum_variant_ref(&self, node: &SyntaxNode) -> Option<(String, String, Vec<Ty>)> {
        if node.kind() != SyntaxKind::FieldExpr {
            return None;
        }
        let recv = self.first_expr(node)?;
        let variant = node.token_of_kind(SyntaxKind::Ident)?.text().to_string();

        let (enum_name, type_args) = match recv.kind() {
            // `Shape.Circle` — a plain enum name.
            SyntaxKind::NameExpr => (ident_text(&recv), Vec::new()),
            // `Maybe(int32).Some` — a generic instantiation, which parses in
            // expression position as a call of the enum name on its type arguments.
            SyntaxKind::CallExpr => {
                let inner = self.first_expr(&recv)?;
                if inner.kind() != SyntaxKind::NameExpr {
                    return None;
                }
                let args = self.type_args_from_call(&recv)?;
                (ident_text(&inner), args)
            }
            _ => return None,
        };

        self.enums.get(&enum_name)?;
        Some((enum_name, variant, type_args))
    }

    fn lower_static_assert(&mut self, e: &SyntaxNode) -> Option<Stmt> {
        if e.kind() != SyntaxKind::CallExpr {
            return None;
        }
        let callee = self.first_expr(e)?;
        if callee.kind() != SyntaxKind::NameExpr || ident_text(&callee) != "static_assert" {
            return None;
        }
        let args = self.call_args(e);
        let [cond, message] = args.as_slice() else {
            self.err(
                e.span(),
                format!(
                    "`static_assert` takes a condition and a message, but {} argument(s) were given",
                    args.len()
                ),
            );
            return Some(Stmt::Expr(self.lower_expr(e)));
        };
        let ExprKind::Str(text) = &message.kind else {
            self.err(
                e.span(),
                "`static_assert`'s second argument must be a string literal",
            );
            return Some(Stmt::Expr(self.lower_expr(e)));
        };
        Some(Stmt::StaticAssert {
            cond: cond.clone(),
            message: text.clone(),
        })
    }

    fn lower_sizeof(&mut self, node: &SyntaxNode) -> (ExprKind, Ty) {
        let size_ty = Ty::Int {
            bits: IntWidth::Size,
            signed: false,
        };
        let args: Vec<SyntaxNode> = node
            .child_of_kind(SyntaxKind::ArgList)
            .map(|al| al.children())
            .unwrap_or_default();
        let [arg] = args.as_slice() else {
            self.err(
                node.span(),
                format!(
                    "`sizeof` takes exactly 1 type argument, but {} were given",
                    args.len()
                ),
            );
            return (ExprKind::Unresolved("sizeof".into()), size_ty);
        };
        match expr_as_type(arg) {
            Some(ty) => {
                self.check_type(&ty, &[], arg.span());
                (ExprKind::SizeOf(ty), size_ty)
            }
            None => {
                self.err(arg.span(), "`sizeof` expects a type argument");
                (ExprKind::Unresolved("sizeof".into()), size_ty)
            }
        }
    }

    fn check_variant(&mut self, enum_name: &str, variant: &str, count: usize, span: Span) {
        let Some(variants) = self.enums.get(enum_name) else {
            return; // receiver isn't an enum — reported elsewhere
        };
        match variants.iter().find(|v| v.name == variant) {
            None => self.err(
                span,
                format!("enum `{enum_name}` has no variant `{variant}`"),
            ),
            Some(v) if v.payload.len() != count => self.err(
                span,
                format!(
                    "variant `{enum_name}.{variant}` takes {} value(s), but {count} were given",
                    v.payload.len()
                ),
            ),
            Some(_) => {}
        }
    }

    /// Read the type arguments of a generic instantiation written in expression
    /// position (`Maybe(int32)` → `[int32]`). Each argument must be a type
    /// expressible as an expression (a name or a nested instantiation). anything
    /// else yields `None` so the caller falls back to treating it as a call.
    fn type_args_from_call(&self, call: &SyntaxNode) -> Option<Vec<Ty>> {
        let arg_list = call.child_of_kind(SyntaxKind::ArgList)?;
        arg_list.children().iter().map(expr_as_type).collect()
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

    /// Validate every type reference in a declaration's signature (and, for a
    /// proc, its body) against the declared types and the type parameters in
    /// scope.
    fn validate_decl_types(&mut self, node: &SyntaxNode, type_params: &[String]) {
        for (i, p) in type_params.iter().enumerate() {
            if type_params[..i].contains(p) {
                self.err(node.span(), format!("duplicate type parameter `{p}`"));
            }
        }
        let mut type_nodes = Vec::new();
        collect_type_nodes(node, &mut type_nodes);
        for tn in type_nodes {
            if let Some(ty) = lower_type(&tn) {
                self.check_type(&ty, type_params, tn.span());
            }
        }
    }

    /// Report an error for any type reference that names an undeclared type uses
    /// a generic type with the wrong number of arguments, or applies type
    /// arguments to something that isn't generic.
    fn check_type(&mut self, ty: &Ty, type_params: &[String], span: Span) {
        match ty {
            Ty::Named(n) => {
                if type_params.iter().any(|p| p == n) {
                    return;
                }
                match self.type_arity.get(n) {
                    Some(0) => {}
                    Some(arity) => self.err(
                        span,
                        format!("`{n}` is generic; write `{n}(...)` with {arity} type argument(s)"),
                    ),
                    None => self.err(span, format!("unknown type `{n}`")),
                }
            }
            Ty::App(name, args) => {
                if type_params.iter().any(|p| p == name) {
                    self.err(
                        span,
                        format!("type parameter `{name}` cannot take type arguments"),
                    );
                } else {
                    match self.type_arity.get(name) {
                        Some(0) => self.err(
                            span,
                            format!("`{name}` is not generic and takes no type arguments"),
                        ),
                        Some(arity) if *arity != args.len() => self.err(
                            span,
                            format!(
                                "`{name}` expects {arity} type argument(s), but {} were given",
                                args.len()
                            ),
                        ),
                        Some(_) => {}
                        None => self.err(span, format!("unknown type `{name}`")),
                    }
                }
                for a in args {
                    self.check_type(a, type_params, span);
                }
            }
            Ty::Ptr(inner) | Ty::Rc(inner) => self.check_type(inner, type_params, span),
            Ty::Void | Ty::Bool | Ty::Int { .. } | Ty::Float { .. } | Ty::Infer => {}
        }
    }

    fn check_duplicates<'n, I>(&mut self, names: I, what: &str, span: Span)
    where
        I: IntoIterator<Item = &'n str>,
    {
        let mut seen: Vec<&str> = Vec::new();
        let mut reported: Vec<String> = Vec::new();
        for n in names {
            if seen.contains(&n) && !reported.iter().any(|r| r == n) {
                self.err(span, format!("duplicate {what} `{n}`"));
                reported.push(n.to_string());
            } else {
                seen.push(n);
            }
        }
    }

    fn lower_struct(&mut self, node: &SyntaxNode) {
        let name = match first_ident(node) {
            Some(n) => n,
            None => return,
        };
        let def = self.resolve(&name).unwrap_or(DefId(0));
        let fields = self.structs.get(&name).cloned().unwrap_or_default();
        let type_params = comptime_type_params(node);
        self.validate_decl_types(node, &type_params);
        self.check_duplicates(fields.iter().map(|f| f.name.as_str()), "field", node.span());
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
        let type_params = comptime_type_params(node);
        self.validate_decl_types(node, &type_params);
        self.check_duplicates(
            variants.iter().map(|v| v.name.as_str()),
            "variant",
            node.span(),
        );

        // A variant must not share a name with a comptime type parameter.
        for v in &variants {
            if type_params.contains(&v.name) {
                self.err(
                    node.span(),
                    format!(
                        "variant `{}` collides with the comptime type parameter of the same name",
                        v.name
                    ),
                );
            }
        }
        self.items.push(Item::Enum(EnumDef {
            def,
            name,
            type_params,
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

        let cases: Vec<String> = arms
            .iter()
            .filter_map(|a| match &a.pattern {
                Pattern::Enum {
                    enum_name, variant, ..
                } => Some(format!("{enum_name}.{variant}")),
                _ => None,
            })
            .collect();
        self.check_duplicates(cases.iter().map(String::as_str), "case", node.span());
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
            self.check_variant(enum_name, variant, bindings.len(), clause.span());
            let payload = self.concrete_variant_payload(enum_name, variant, scrut_ty);
            for (i, b) in bindings.iter().enumerate() {
                let ty = payload.get(i).cloned().unwrap_or(Ty::Infer);
                let id = self.add_def(b.clone(), DefKind::Local, ty);
                self.bind_local(b.clone(), id);
            }
        }
        let body = self.lower_block(clause);
        self.pop_scope();
        Arm { pattern, body }
    }

    /// The payload types of `enum_name::variant`, with the enum's type parameters
    /// substituted by the scrutinee's actual type arguments when it is a generic
    /// instantiation (`Maybe(@Node)` → `Some`'s `T` becomes `@Node`). This is what
    /// gives a matched binding its concrete type, so field access through it
    /// auto-dereferences correctly :)
    fn concrete_variant_payload(&self, enum_name: &str, variant: &str, scrut_ty: &Ty) -> Vec<Ty> {
        let payload = self
            .enums
            .get(enum_name)
            .and_then(|vs| vs.iter().find(|v| v.name == variant))
            .map(|v| v.payload.clone())
            .unwrap_or_default();

        let type_args = match scrut_ty {
            Ty::App(_, args) => args.as_slice(),
            _ => return payload,
        };
        let params = match self.enum_type_params.get(enum_name) {
            Some(p) => p,
            None => return payload,
        };
        payload
            .iter()
            .map(|ty| subst_type_params(ty, params, type_args))
            .collect()
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

fn subst_type_params(ty: &Ty, params: &[String], args: &[Ty]) -> Ty {
    match ty {
        Ty::Named(n) => match params.iter().position(|p| p == n) {
            Some(i) => args.get(i).cloned().unwrap_or_else(|| ty.clone()),
            None => ty.clone(),
        },
        Ty::Ptr(inner) => Ty::Ptr(Box::new(subst_type_params(inner, params, args))),
        Ty::Rc(inner) => Ty::Rc(Box::new(subst_type_params(inner, params, args))),
        Ty::App(n, a) => Ty::App(
            n.clone(),
            a.iter()
                .map(|t| subst_type_params(t, params, args))
                .collect(),
        ),
        _ => ty.clone(),
    }
}

fn enum_instance_ty(enum_name: &str, type_args: Vec<Ty>) -> Ty {
    if type_args.is_empty() {
        Ty::Named(enum_name.to_string())
    } else {
        Ty::App(enum_name.to_string(), type_args)
    }
}

fn expr_as_type(node: &SyntaxNode) -> Option<Ty> {
    // An argument that was parsed directly as a type (`@Node`, `[]T`) — the arg
    // list parses type-only-prefixed arguments as types (see `arg_list`).
    if is_type(node.kind()) {
        return lower_type(node);
    }
    match node.kind() {
        SyntaxKind::NameExpr => {
            let name = node.token_of_kind(SyntaxKind::Ident)?.text().to_string();
            Some(name_to_ty(&name))
        }
        SyntaxKind::CallExpr => {
            let callee = node
                .children()
                .into_iter()
                .find(|c| c.kind() == SyntaxKind::NameExpr)?;
            let name = callee.token_of_kind(SyntaxKind::Ident)?.text().to_string();
            let arg_list = node.child_of_kind(SyntaxKind::ArgList)?;
            let args: Option<Vec<Ty>> = arg_list.children().iter().map(expr_as_type).collect();
            Some(Ty::App(name, args?))
        }
        SyntaxKind::ParenExpr => expr_as_type(&node.children().into_iter().next()?),
        _ => None,
    }
}

fn runtime_param_count(node: &SyntaxNode) -> usize {
    node.child_of_kind(SyntaxKind::ParamList)
        .map(|pl| {
            pl.children()
                .iter()
                .filter(|p| {
                    p.kind() == SyntaxKind::Param
                        && p.token_of_kind(SyntaxKind::KwComptime).is_none()
                })
                .count()
        })
        .unwrap_or(0)
}

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

fn collect_type_nodes(node: &SyntaxNode, out: &mut Vec<SyntaxNode>) {
    for child in node.children() {
        if child.kind() == SyntaxKind::Param
            && child.token_of_kind(SyntaxKind::KwComptime).is_some()
        {
            continue;
        }
        if is_type(child.kind()) {
            out.push(child);
        } else {
            collect_type_nodes(&child, out);
        }
    }
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
                if let (Some(h), Some(l)) = (hi, lo)
                    && let Ok(byte) = u8::from_str_radix(&format!("{h}{l}"), 16)
                {
                    out.push(byte as char);
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
