// SPDX-License-Identifier: Apache-2.0

//! CST → HIR lowering: name resolution + best-effort type inference in one pass.

use std::collections::{HashMap, HashSet};

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
    /// Each struct's comptime type-parameter names, for substituting a generic
    /// instantiation's arguments into its field types
    struct_type_params: HashMap<String, Vec<String>>,
    /// Every declared type's comptime-parameter count, so type references can be
    /// checked for existence and correct arity (0 = a non-generic type).
    type_arity: HashMap<String, usize>,
    /// Each proc's runtime parameter count, to check call arity
    proc_arity: HashMap<String, usize>,
    /// Each generic proc's comptime type-parameter names, for call-site inference.
    proc_type_params: HashMap<String, Vec<String>>,
    /// Each generic proc's declared runtime parameter types, still mentioning its
    /// type parameters — the patterns that argument types are matched against
    proc_param_types: HashMap<String, Vec<Ty>>,
    /// Every proc's declared parameter types, for checking call arguments
    proc_signatures: HashMap<String, Vec<Ty>>,
    /// C functions declared with `...`, which accept arguments beyond those
    variadic_procs: HashSet<String>,
    /// The declared return type of the proc being lowered, to check `return`.
    current_ret: Ty,
    /// Counter for compiler-generated local names.
    temp: u32,
    /// True while lowering an `extern` declaration, where `cchar` is allowed
    in_extern: bool,
    /// Struct names currently being zero-initialized, so a by-value cycle cannot
    /// drive `zero_aggregate` into unbounded recursion.
    zeroing: Vec<String>,
    /// The comptime type parameters of the declaration currently being lowered, so
    /// type references inside its body (`sizeof(T)`) resolve to them.
    type_params_in_scope: Vec<String>,
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
            struct_type_params: HashMap::new(),
            type_arity: HashMap::new(),
            proc_arity: HashMap::new(),
            proc_type_params: HashMap::new(),
            proc_signatures: HashMap::new(),
            variadic_procs: HashSet::new(),
            proc_param_types: HashMap::new(),
            current_ret: Ty::Void,
            temp: 0,
            in_extern: false,
            zeroing: Vec::new(),
            type_params_in_scope: Vec::new(),
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
                        self.proc_signatures
                            .insert(name.clone(), declared_param_types(decl));
                        let type_params = comptime_type_params(decl);
                        if !type_params.is_empty() {
                            self.proc_param_types
                                .insert(name.clone(), declared_param_types(decl));
                            self.proc_type_params.insert(name.clone(), type_params);
                        }
                        let id = self.add_def(name.clone(), DefKind::Proc, ret);
                        self.bind_module(name.clone(), id, decl.span());
                    }
                }
                SyntaxKind::ExternProcDecl => {
                    if let Some(name) = first_ident(decl) {
                        let symbol = self.extern_symbol(decl).unwrap_or_else(|| name.clone());
                        let ret = self.return_type(decl);
                        self.proc_arity
                            .insert(name.clone(), runtime_param_count(decl));
                        self.proc_signatures
                            .insert(name.clone(), declared_param_types(decl));
                        if declares_variadic(decl) {
                            self.variadic_procs.insert(name.clone());
                        }
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
                        let tps = comptime_type_params(decl);
                        self.type_arity.insert(name.clone(), tps.len());
                        self.struct_type_params.insert(name.clone(), tps);
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

    fn declared_in_current_scope(&self, name: &str) -> bool {
        let body_and_params = self.scopes.len().saturating_sub(2)..self.scopes.len();
        self.scopes[body_and_params]
            .iter()
            .enumerate()
            .any(|(depth, scope)| {
                // Only the parameter scope is shared. an outer *block* may be
                // shadowed legally
                let is_param_scope = depth == 0;
                scope.names.get(name).is_some_and(|id| {
                    !is_param_scope
                        || matches!(
                            self.defs.get(id.0 as usize).map(|d| &d.kind),
                            Some(DefKind::Param)
                        )
                })
            })
    }

    fn bind_local(&mut self, name: String, id: DefId) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.names.insert(name, id);
        }
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
        let def = self.resolve(&name).unwrap_or(DefId(0));
        let ret = self.return_type(node);

        let type_params = comptime_type_params(node);
        self.validate_decl_types(node, &type_params);

        self.push_scope();
        self.type_params_in_scope = type_params.clone();
        self.current_ret = ret.clone();
        let params = self.lower_params(node);
        let body = match node.child_of_kind(SyntaxKind::Block) {
            Some(b) => self.lower_block(&b),
            None => {
                self.err(node.span(), "proc without a body");
                Vec::new()
            }
        };

        self.type_params_in_scope.clear();
        self.current_ret = Ty::Void;
        self.pop_scope();

        self.items.push(Item::Proc(Proc {
            def,
            name,
            type_params,
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
        let def = self.resolve(&name).unwrap_or(DefId(0));
        let symbol = self.extern_symbol(node).unwrap_or_else(|| name.clone());
        let ret = self.return_type(node);
        self.push_scope();
        let params = self.lower_params(node);
        self.pop_scope();
        // `@T` may never appear in an `extern` signature.
        for p in &params {
            self.reject_rc_in_extern(
                &p.ty,
                &name,
                &format!("parameter `{}`", p.name),
                node.span(),
            );
        }
        self.reject_rc_in_extern(&ret, &name, "return type", node.span());
        self.items.push(Item::ExternProc(ExternProc {
            def,
            name,
            symbol,
            params,
            variadic: declares_variadic(node),
            ret,
        }));
    }

    fn reject_rc_in_extern(&mut self, ty: &Ty, proc_name: &str, where_: &str, span: Span) {
        if !mentions_rc(ty) {
            return;
        }
        self.err(
            span,
            format!(
                "the {where_} of `{proc_name}` contains `{}`, and a counted pointer \
                 cannot cross into C; use a raw pointer (`*T`) instead",
                type_name(ty)
            ),
        );
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
            SyntaxKind::ReturnStmt => self.lower_return(node),
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
        let init_node = self.first_value_of(node)?;
        let init = self.lower_value(&init_node, declared.as_ref());
        if let Some(d) = &declared {
            self.check_assignable(d, &init, "this binding", init_node.span());
        }
        let ty = declared.unwrap_or_else(|| default_ty(&init.ty));
        if self.declared_in_current_scope(&name) {
            self.err(
                node.span(),
                format!("`{name}` is already declared in this scope"),
            );
        }
        let id = self.add_def(name.clone(), DefKind::Local, ty.clone());
        self.bind_local(name.clone(), id);
        Some(Stmt::Let {
            def: id,
            name,
            ty,
            init,
        })
    }

    /// Check a call's arguments against the callee's declared parameter types.
    /// Silently does nothing for a callee that isn't a known proc
    fn check_call_args(&mut self, callee: &Expr, args: &[Expr], node: &SyntaxNode) {
        let ExprKind::Name { name, .. } = &callee.kind else {
            return;
        };
        let Some(params) = self.proc_signatures.get(name).cloned() else {
            return;
        };

        let arg_nodes: Vec<SyntaxNode> = node
            .child_of_kind(SyntaxKind::ArgList)
            .map(|al| al.children())
            .unwrap_or_default();

        for (i, (param_ty, arg)) in params.iter().zip(args).enumerate() {
            let span = arg_nodes.get(i).map_or_else(|| node.span(), |n| n.span());
            self.check_assignable(
                param_ty,
                arg,
                &format!("argument {} of `{name}`", i + 1),
                span,
            );
        }
    }

    /// `return [expr];`
    fn lower_return(&mut self, node: &SyntaxNode) -> Option<Stmt> {
        let value = self
            .first_expr(node)
            .map(|e| (self.lower_expr(&e), e.span()));
        let ret = self.current_ret.clone();
        match &value {
            Some((expr, span)) => {
                if matches!(ret, Ty::Void) {
                    self.err(
                        *span,
                        "this proc returns nothing, so `return` takes no value",
                    );
                } else {
                    self.check_assignable(&ret, expr, "this `return`", *span);
                }
            }
            None => {
                if !matches!(ret, Ty::Void | Ty::Infer) {
                    self.err(
                        node.span(),
                        format!(
                            "this proc returns `{}`, so `return` needs a value",
                            type_name(&ret)
                        ),
                    );
                }
            }
        }
        Some(Stmt::Return(value.map(|(e, _)| e)))
    }

    fn check_assign_target(&mut self, target: &Expr, span: Span) {
        let describe = match &target.kind {
            ExprKind::Name { def, name } => match self.defs.get(def.0 as usize).map(|d| &d.kind) {
                Some(DefKind::Local | DefKind::Param) => return,
                Some(DefKind::Proc | DefKind::ExternProc { .. }) => {
                    format!("`{name}` is a proc")
                }
                Some(DefKind::Struct) => format!("`{name}` is a struct type"),
                Some(DefKind::Enum) => format!("`{name}` is an enum type"),
                None => return,
            },
            ExprKind::Field { .. } | ExprKind::Index { .. } => return,
            ExprKind::Unary {
                op: UnOp::Deref, ..
            } => return,
            ExprKind::Paren(inner) => return self.check_assign_target(inner, span),
            _ => "this is not a place that can hold a value".to_string(),
        };
        self.err(span, format!("cannot assign to it: {describe}"));
    }

    fn lower_assign(&mut self, node: &SyntaxNode) -> Option<Stmt> {
        let parts = self.expr_children(node);
        if parts.len() != 2 {
            self.err(node.span(), "assignment needs a target and a value");
            return None;
        }
        let op = assign_op(node)?;
        let target = self.lower_expr(&parts[0]);
        let value = self.lower_value(&parts[1], Some(&target.ty));

        self.check_assign_target(&target, parts[0].span());
        self.check_assignable(&target.ty, &value, "this assignment", parts[1].span());

        if let Ty::Array(elem_ty, len) = &target.ty {
            let span = parts[0].span();
            let elem_ty = (**elem_ty).clone();
            let at = |this: &Self, base: &Expr, i: u64| Expr {
                kind: ExprKind::Index {
                    base: Box::new(base.clone()),
                    index: Box::new(*this.int_literal(i as i64, span)),
                },
                ty: elem_ty.clone(),
                span,
            };
            let sources: Vec<Expr> = match &value.kind {
                ExprKind::ArrayLit { elements, .. } => elements.clone(),
                _ => (0..*len).map(|i| at(self, &value, i)).collect(),
            };
            let stores = sources
                .into_iter()
                .enumerate()
                .map(|(i, source)| Stmt::Assign {
                    target: at(self, &target, i as u64),
                    op: AssignOp::Assign,
                    value: source,
                })
                .collect();
            return Some(Stmt::Block(stores));
        }
        Some(Stmt::Assign { target, op, value })
    }

    fn expect_bool(&mut self, value: &Expr, what: &str, span: Span) {
        if !matches!(value.ty, Ty::Bool | Ty::Infer) {
            self.err(
                span,
                format!(
                    "{what} needs a `bool`, but this is `{}`",
                    type_name(&value.ty)
                ),
            );
        }
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

    /// `for x in xs { ... }` and `for x, [i] in xs { ... }` over a built-in array
    /// or slice (§14). The compiler knows their shape, so this lowers straight to
    /// an indexed loop rather than just going through the iterator conventio
    fn lower_for_in(&mut self, node: &SyntaxNode) -> Option<Stmt> {
        let span = node
            .child_of_kind(SyntaxKind::Condition)
            .and_then(|c| self.first_expr(&c))
            .map_or_else(|| node.span(), |e| e.span());
        let names = loop_bindings(node);
        let element_name = names.first()?.clone();
        let index_name = names
            .get(1)
            .cloned()
            .unwrap_or_else(|| self.fresh_name("idx"));

        let seq_node = node
            .child_of_kind(SyntaxKind::Condition)
            .and_then(|c| self.first_expr(&c))?;
        let sequence = self.lower_expr(&seq_node);
        let elem_ty = match &sequence.ty {
            Ty::Array(elem, _) | Ty::Slice(elem) => (**elem).clone(),
            Ty::Infer => Ty::Infer,
            other => {
                self.err(
                    seq_node.span(),
                    format!(
                        "only an array or slice can be iterated with `for ... in`, not `{}`",
                        type_name(other)
                    ),
                );
                return None;
            }
        };

        let needs_temp =
            matches!(sequence.ty, Ty::Slice(_)) && !matches!(sequence.kind, ExprKind::Name { .. });

        self.push_scope();

        let seq_ty = sequence.ty.clone();
        let (seq_ref, bind_seq) = if needs_temp {
            let seq_name = self.fresh_name("seq");
            let def = self.add_def(seq_name.clone(), DefKind::Local, seq_ty.clone());
            self.bind_local(seq_name.clone(), def);
            let reference = Expr {
                kind: ExprKind::Name {
                    def,
                    name: seq_name.clone(),
                },
                ty: seq_ty.clone(),
                span,
            };
            let binding = Stmt::Let {
                def,
                name: seq_name,
                ty: seq_ty.clone(),
                init: sequence,
            };
            (reference, Some(binding))
        } else {
            (sequence, None)
        };

        // A fixed array's length is part of its typ, and a slice carries its own
        let length = match &seq_ty {
            Ty::Array(_, n) => *self.int_literal(*n as i64, span),
            _ => Expr {
                kind: ExprKind::Field {
                    recv: Box::new(seq_ref.clone()),
                    member: "len".to_string(),
                },
                ty: Ty::Int {
                    bits: IntWidth::W32,
                    signed: true,
                },
                span,
            },
        };

        let int32 = Ty::Int {
            bits: IntWidth::W32,
            signed: true,
        };
        let idx_def = self.add_def(index_name.clone(), DefKind::Local, int32.clone());
        self.bind_local(index_name.clone(), idx_def);
        let idx_ref = Expr {
            kind: ExprKind::Name {
                def: idx_def,
                name: index_name.clone(),
            },
            ty: int32.clone(),
            span,
        };

        let init = Stmt::Let {
            def: idx_def,
            name: index_name.clone(),
            ty: int32.clone(),
            init: *self.int_literal(0, span),
        };
        let cond = Expr {
            kind: ExprKind::Binary {
                op: BinOp::Lt,
                lhs: Box::new(idx_ref.clone()),
                rhs: Box::new(length),
            },
            ty: Ty::Bool,
            span,
        };
        let post = Stmt::Assign {
            target: idx_ref.clone(),
            op: AssignOp::Add,
            value: *self.int_literal(1, span),
        };

        // The element binding is the first statement of the body, so the loop body
        // sees it exactly as if the user had written it.
        self.push_scope();
        let elem_def = self.add_def(element_name.clone(), DefKind::Local, elem_ty.clone());
        self.bind_local(element_name.clone(), elem_def);
        let bind_elem = Stmt::Let {
            def: elem_def,
            name: element_name,
            ty: elem_ty.clone(),
            init: Expr {
                kind: ExprKind::Index {
                    base: Box::new(seq_ref.clone()),
                    index: Box::new(idx_ref),
                },
                ty: elem_ty,
                span,
            },
        };
        let mut body = vec![bind_elem];
        if let Some(b) = node.child_of_kind(SyntaxKind::Block) {
            body.extend(self.lower_block(&b));
        }
        self.pop_scope();
        self.pop_scope();

        let loop_stmt = Stmt::CFor {
            init: Some(Box::new(init)),
            cond: Some(cond),
            post: Some(Box::new(post)),
            body,
        };
        Some(match bind_seq {
            Some(binding) => Stmt::Block(vec![binding, loop_stmt]),
            None => loop_stmt,
        })
    }

    /// A compiler-generated local name that cannot collide with a user's.
    fn fresh_name(&mut self, what: &str) -> String {
        self.temp += 1;
        format!("__dray_{what}_{}", self.temp)
    }

    /// An integer literal expression, for the pieces of a desugaring.
    fn int_literal(&self, value: i64, span: Span) -> Box<Expr> {
        Box::new(Expr {
            kind: ExprKind::Int(value),
            ty: Ty::Int {
                bits: IntWidth::W32,
                signed: true,
            },
            span,
        })
    }

    fn lower_for(&mut self, node: &SyntaxNode) -> Option<Stmt> {
        if node.token_of_kind(SyntaxKind::KwIn).is_some() {
            return self.lower_for_in(node);
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
            SyntaxKind::SliceExpr => self.lower_slice_expr(node),
            SyntaxKind::CastExpr => self.lower_cast(node),
            SyntaxKind::AllocExpr => self.lower_alloc(node),
            SyntaxKind::CompositeLit => self.lower_struct_lit(node, None),
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
            SyntaxKind::StringLit => (
                ExprKind::Str(unquote(text)),
                Ty::Slice(Box::new(Ty::Int {
                    bits: IntWidth::W8,
                    signed: false,
                })),
            ),
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
                    UnOp::LogicNot => {
                        self.expect_bool(&operand, "`!`", node.span());
                        Ty::Bool
                    }
                    UnOp::AddrOf => Ty::Ptr(Box::new(operand.ty.clone())),
                    UnOp::Deref => match &operand.ty {
                        Ty::Ptr(inner) | Ty::Rc(inner) => (**inner).clone(),
                        Ty::Infer => Ty::Infer,
                        other => {
                            self.err(
                                node.span(),
                                format!(
                                    "only a pointer can be dereferenced, not `{}`",
                                    type_name(other)
                                ),
                            );
                            Ty::Infer
                        }
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

    /// `&&` and `||` take and produce `bool`
    fn check_logical_operands(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span) {
        if matches!(op, BinOp::And | BinOp::Or) {
            self.expect_bool(lhs, "this operator", span);
            self.expect_bool(rhs, "this operator", span);
        }
    }

    /// Both operands of an arithmetic, comparison or bitwise operator must have
    /// the same type. No impliciit conversion allowed.
    fn check_binary_operands(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span) {
        if matches!(op, BinOp::And | BinOp::Or) {
            return; // already required to be `bool`
        }

        if matches!(lhs.ty, Ty::Infer) || matches!(rhs.ty, Ty::Infer) {
            return;
        }

        if lhs.ty == rhs.ty || coerces_to(&lhs.ty, rhs) || coerces_to(&rhs.ty, lhs) {
            self.check_operator_domain(op, lhs, span);
            return;
        }
        self.err(
            span,
            format!(
                "`{}` needs both sides to have the same type, but this is `{}` and `{}`; add a `cast`",
                operator_name(op),
                type_name(&lhs.ty),
                type_name(&rhs.ty)
            ),
        );
    }

    fn check_operator_domain(&mut self, op: BinOp, operand: &Expr, span: Span) {
        let ty = &operand.ty;
        let integers_only = matches!(
            op,
            BinOp::Rem | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr
        );
        if integers_only && !matches!(ty, Ty::Int { .. } | Ty::Infer) {
            self.err(
                span,
                format!(
                    "`{}` is only defined for integers, but this is `{}`",
                    operator_name(op),
                    type_name(ty)
                ),
            );
            return;
        }
        let arithmetic = matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div);
        let ordered = matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge);
        if (arithmetic || ordered) && !matches!(ty, Ty::Int { .. } | Ty::Float { .. } | Ty::Infer) {
            self.err(
                span,
                format!(
                    "`{}` is only defined for numbers, but this is `{}`",
                    operator_name(op),
                    type_name(ty)
                ),
            );
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
        self.check_logical_operands(op, &lhs, &rhs, node.span());
        self.check_binary_operands(op, &lhs, &rhs, node.span());
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

        if let Some(cn) = &callee_node
            && cn.kind() == SyntaxKind::NameExpr
            && self.proc_type_params.contains_key(&ident_text(cn))
        {
            return self.lower_generic_call(node, &ident_text(cn));
        }

        let callee = match callee_node {
            Some(c) => self.lower_expr(&c),
            None => return (ExprKind::Unresolved("call".into()), Ty::Infer),
        };
        let args = self.call_args(node);
        // If the callee names a known proc, its argument count must match.
        if let ExprKind::Name { name, .. } = &callee.kind
            && let Some(&arity) = self.proc_arity.get(name)
            && !fits_arity(args.len(), arity, self.variadic_procs.contains(name))
        {
            self.err(
                node.span(),
                format!(
                    "proc `{name}` takes {}{arity} argument(s), but {} were given",
                    if self.variadic_procs.contains(name) {
                        "at least "
                    } else {
                        ""
                    },
                    args.len()
                ),
            );
        }
        self.check_call_args(&callee, &args, node);
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
        if let Ty::Slice(elem) = &recv.ty {
            let ty = match member.as_str() {
                "len" => Ty::Int {
                    bits: IntWidth::W32,
                    signed: true,
                },
                "ptr" => Ty::Ptr(elem.clone()),
                _ => {
                    self.err(
                        node.span(),
                        format!("a slice has only `len` and `ptr`, not `{member}`"),
                    );
                    Ty::Infer
                }
            };
            return (
                ExprKind::Field {
                    recv: Box::new(recv),
                    member,
                },
                ty,
            );
        }
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

    /// Lower a call to a generic proc, resolving its `comptime` type arguments.
    fn lower_generic_call(&mut self, node: &SyntaxNode, proc_name: &str) -> (ExprKind, Ty) {
        let type_params = self
            .proc_type_params
            .get(proc_name)
            .cloned()
            .unwrap_or_default();
        let param_types = self
            .proc_param_types
            .get(proc_name)
            .cloned()
            .unwrap_or_default();
        let ret = self
            .resolve(proc_name)
            .map(|id| self.defs[id.0 as usize].ty.clone())
            .unwrap_or(Ty::Infer);

        let arg_nodes: Vec<SyntaxNode> = node
            .child_of_kind(SyntaxKind::ArgList)
            .map(|al| al.children())
            .unwrap_or_default();
        let n_types = type_params.len();
        let n_values = param_types.len();

        // Either every type argument is written out, or none of them are
        let (explicit, value_nodes) = if arg_nodes.len() == n_types + n_values {
            arg_nodes.split_at(n_types)
        } else if arg_nodes.len() == n_values {
            arg_nodes.split_at(0)
        } else {
            self.err(
                node.span(),
                format!(
                    "proc `{proc_name}` takes {n_values} argument(s) (or {} with explicit type arguments), but {} were given",
                    n_types + n_values,
                    arg_nodes.len()
                ),
            );
            return (ExprKind::Unresolved(proc_name.to_string()), ret);
        };

        let args: Vec<Expr> = value_nodes.iter().map(|a| self.lower_expr(a)).collect();

        let mut bindings: HashMap<String, Ty> = HashMap::new();
        for (arg_node, param) in explicit.iter().zip(&type_params) {
            match expr_as_type(arg_node) {
                Some(ty) => {
                    self.check_type_in_scope(&ty, arg_node.span());
                    bindings.insert(param.clone(), ty);
                }
                None => self.err(arg_node.span(), format!("expected a type for `{param}`")),
            }
        }
        for (param_ty, arg) in param_types.iter().zip(&args) {
            infer_type_params(param_ty, &arg.ty, &type_params, &mut bindings);
        }

        let mut type_args = Vec::with_capacity(n_types);
        for param in &type_params {
            match bindings.get(param) {
                Some(ty) => type_args.push(ty.clone()),
                None => {
                    self.err(
                        node.span(),
                        format!(
                            "cannot infer type parameter `{param}` of `{proc_name}` from the arguments; pass it explicitly"
                        ),
                    );
                    type_args.push(Ty::Infer);
                }
            }
        }

        for ((param_ty, arg), arg_node) in param_types.iter().zip(&args).zip(value_nodes) {
            let concrete = subst_type_params(param_ty, &type_params, &type_args);
            self.check_assignable(
                &concrete,
                arg,
                &format!("argument to `{proc_name}`"),
                arg_node.span(),
            );
        }

        let ret = subst_type_params(&ret, &type_params, &type_args);
        (
            ExprKind::GenericCall {
                proc_name: proc_name.to_string(),
                type_args,
                args,
            },
            ret,
        )
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
                self.check_type_in_scope(&ty, arg.span());
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

    /// `xs[:]` a slice over the whole of an array, which is how a `[]T` value
    /// comes into existence. The slice borrows; it does not own the elements.
    fn lower_slice_expr(&mut self, node: &SyntaxNode) -> (ExprKind, Ty) {
        let Some(array_node) = self.first_expr(node) else {
            return (ExprKind::Unresolved("slice".into()), Ty::Infer);
        };
        let array = self.lower_expr(&array_node);
        let ty = match &array.ty {
            Ty::Array(elem, _) => Ty::Slice(elem.clone()),
            // Already a slice: `xs[:]` is then just `xs`.
            Ty::Slice(elem) => Ty::Slice(elem.clone()),
            Ty::Infer => Ty::Infer,
            other => {
                self.err(
                    node.span(),
                    format!(
                        "only an array or slice can be sliced, not `{}`",
                        type_name(other)
                    ),
                );
                Ty::Infer
            }
        };
        (
            ExprKind::SliceAll {
                array: Box::new(array),
            },
            ty,
        )
    }

    fn lower_index(&mut self, node: &SyntaxNode) -> (ExprKind, Ty) {
        let parts = self.expr_children(node);
        if parts.len() != 2 {
            return (ExprKind::Unresolved("index".into()), Ty::Infer);
        }
        let base = self.lower_expr(&parts[0]);
        let index = self.lower_expr(&parts[1]);
        // Index has to be a number, anything else is a mistake
        if !matches!(index.ty, Ty::Int { .. } | Ty::Infer) {
            self.err(
                parts[1].span(),
                format!(
                    "an index must be an integer, but this is `{}`",
                    type_name(&index.ty)
                ),
            );
        }
        if let (Ty::Array(_, len), Some(i)) = (&base.ty, const_int(&index))
            && (i < 0 || i as u64 >= *len)
        {
            self.err(
                parts[1].span(),
                format!("index {i} is outside this array's {len} element(s)"),
            );
        }

        let ty = match &base.ty {
            // A raw pointer, an array, and a slice all index to their element.
            Ty::Ptr(inner) | Ty::Array(inner, _) | Ty::Slice(inner) => (**inner).clone(),
            Ty::Infer => Ty::Infer,
            other => {
                self.err(
                    node.span(),
                    format!("`{}` cannot be indexed", type_name(other)),
                );
                Ty::Infer
            }
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
        // a cast is the C boundary, so `cchar` is meaningful in its target type
        let outer = std::mem::replace(&mut self.in_extern, true);
        let result = self.lower_cast_inner(node);
        self.in_extern = outer;
        result
    }

    fn lower_cast_inner(&mut self, node: &SyntaxNode) -> (ExprKind, Ty) {
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
        for (tn, at_boundary) in type_nodes {
            if let Some(ty) = lower_type(&tn) {
                let outer = self.in_extern;
                self.in_extern = outer || at_boundary;
                self.check_type(&ty, type_params, tn.span());
                self.in_extern = outer;
            }
        }
    }

    fn check_assignable(&mut self, expected: &Ty, value: &Expr, what: &str, span: Span) {
        if matches!(expected, Ty::Infer) || matches!(value.ty, Ty::Infer) {
            return;
        }
        if expected == &value.ty || coerces_to(expected, value) {
            return;
        }
        self.err(
            span,
            format!(
                "{what} expects `{}`, but this is `{}`",
                type_name(expected),
                type_name(&value.ty)
            ),
        );
    }

    fn check_type_in_scope(&mut self, ty: &Ty, span: Span) {
        let scoped = std::mem::take(&mut self.type_params_in_scope);
        self.check_type(ty, &scoped, span);
        self.type_params_in_scope = scoped;
    }

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
            Ty::Ptr(inner) | Ty::Rc(inner) | Ty::Array(inner, _) | Ty::Slice(inner) => {
                self.check_type(inner, type_params, span)
            }
            Ty::CChar => {
                if !self.in_extern {
                    self.err(
                        span,
                        "`cchar` is only for the C boundary: an `extern` signature or a `cast`; use `int8` elsewhere",
                    );
                }
            }
            Ty::Void | Ty::Bool | Ty::Int { .. } | Ty::Float { .. } | Ty::Infer => {}
        }
    }

    /// If `root` reaches itself through by-value struct fields, describe the path
    /// (`A -> B -> A`). Pointer fields break the cycle: they only need a forward
    /// declaration, so a linked structure recursing through `@T` is fine
    fn by_value_cycle(&self, root: &str) -> Option<String> {
        let mut path: Vec<String> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        self.walk_by_value(root, root, &mut path, &mut seen)
            .map(|()| format!("{} -> {root}", path.join(" -> ")))
    }

    fn walk_by_value(
        &self,
        current: &str,
        root: &str,
        path: &mut Vec<String>,
        seen: &mut Vec<String>,
    ) -> Option<()> {
        if seen.iter().any(|s| s == current) {
            return None; // already explored, and it did not reach `root`
        }
        seen.push(current.to_string());
        path.push(current.to_string());
        let fields = self.structs.get(current)?;
        for f in fields {
            // Only a plain named type is embedded by value; `@T`/`*T` are pointers.
            let (Ty::Named(next) | Ty::App(next, _)) = &f.ty else {
                continue;
            };
            if next == root {
                return Some(());
            }
            if self.walk_by_value(next, root, path, seen).is_some() {
                return Some(());
            }
        }
        path.pop();
        None
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

        if let Some(cycle) = self.by_value_cycle(&name) {
            self.err(
                node.span(),
                format!(
                    "`{name}` contains itself by value ({cycle}); make one of these fields a \
                     pointer (`@T`) to break the cycle"
                ),
            );
        }

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
        self.check_exhaustive(&scrutinee.ty, &arms, node.span());
        Some(Stmt::Switch { scrutinee, arms })
    }

    /// Every variant of the scrutinee's enum must be matched. (no `default` for now)
    fn check_exhaustive(&mut self, scrut_ty: &Ty, arms: &[Arm], span: Span) {
        let (Ty::Named(enum_name) | Ty::App(enum_name, _)) = scrut_ty else {
            return; // not an enum (or unresolved)
        };

        let Some(variants) = self.enums.get(enum_name).cloned() else {
            return;
        };

        let missing: Vec<&str> = variants
            .iter()
            .map(|v| v.name.as_str())
            .filter(|name| {
                !arms.iter().any(|a| match &a.pattern {
                    Pattern::Enum { variant, .. } => variant == name,
                    _ => false,
                })
            })
            .collect();

        if !missing.is_empty() {
            let list = missing
                .iter()
                .map(|v| format!("`{enum_name}.{v}`"))
                .collect::<Vec<_>>()
                .join(", ");
            self.err(
                span,
                format!("this `switch` does not cover every variant of `{enum_name}`: {list}"),
            );
        }
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
        match self.lower_composite(lit, None) {
            Some(Composite::Struct { ty, fields }) => (
                ExprKind::Alloc {
                    ty: ty.clone(),
                    fields,
                },
                Ty::Rc(Box::new(ty)),
            ),
            Some(Composite::Array { ty, .. }) => {
                self.err(
                    lit.span(),
                    format!("`alloc {}` is not supported yet", type_name(&ty)),
                );
                (ExprKind::Unresolved("composite".into()), Ty::Infer)
            }
            None => (ExprKind::Unresolved("composite".into()), Ty::Infer),
        }
    }

    /// `T{ ... }` or a bare `{ ... }` a by value struct literal.
    fn lower_struct_lit(&mut self, lit: &SyntaxNode, target: Option<&Ty>) -> (ExprKind, Ty) {
        match self.lower_composite(lit, target) {
            Some(Composite::Struct { ty, fields }) => (
                ExprKind::StructLit {
                    ty: ty.clone(),
                    fields,
                },
                ty,
            ),
            Some(Composite::Array { ty, elements }) => (
                ExprKind::ArrayLit {
                    ty: ty.clone(),
                    elements,
                },
                ty,
            ),
            None => (ExprKind::Unresolved("composite".into()), Ty::Infer),
        }
    }

    fn array_elements(&mut self, lit: &SyntaxNode, elem: &Ty, len: u64) -> Vec<Expr> {
        let mut out: Vec<Expr> = Vec::new();
        for el in lit.children() {
            if el.kind() != SyntaxKind::Element {
                continue;
            }
            if el.token_of_kind(SyntaxKind::Ident).is_some() && self.first_value(&el).is_some() {
                self.err(el.span(), "array elements are positional, not named");
            }
            let Some(value_node) = self.first_value(&el) else {
                continue;
            };
            let value = self.lower_value(&value_node, Some(elem));
            self.check_assignable(elem, &value, "this array element", value_node.span());
            out.push(value);
        }
        if out.len() as u64 > len {
            self.err(
                lit.span(),
                format!(
                    "this array holds {len} element(s), but {} were given",
                    out.len()
                ),
            );
            out.truncate(len as usize);
        }
        while (out.len() as u64) < len {
            out.push(Expr {
                kind: ExprKind::ZeroValue(elem.clone()),
                ty: elem.clone(),
                span: lit.span(),
            });
        }
        out
    }

    fn lower_composite(&mut self, lit: &SyntaxNode, target: Option<&Ty>) -> Option<Composite> {
        let ty = match self.composite_type(lit) {
            Some(t) => t,
            // A bare literal takes the type known from context.
            None => match target {
                Some(t) => t.clone(),
                None => {
                    self.err(
                        lit.span(),
                        "cannot tell what this `{ ... }` builds; give it a type",
                    );
                    return None;
                }
            },
        };

        if let Ty::Array(elem, len) = &ty {
            let elements = self.array_elements(lit, elem, *len);
            return Some(Composite::Array {
                ty: ty.clone(),
                elements,
            });
        }
        let struct_name = match &ty {
            Ty::Named(n) | Ty::App(n, _) => n.clone(),
            _ => {
                self.err(
                    lit.span(),
                    format!("`{}` cannot be built with `{{ ... }}`", type_name(&ty)),
                );
                return None;
            }
        };
        let Some(declared) = self.structs.get(&struct_name).cloned() else {
            self.err(
                lit.span(),
                format!("`{struct_name}` is not a struct, so it cannot be built with `{{ ... }}`"),
            );
            return None;
        };

        let type_params = self
            .struct_type_params
            .get(&struct_name)
            .cloned()
            .unwrap_or_default();
        let type_args: Vec<Ty> = match &ty {
            Ty::App(_, args) => args.clone(),
            _ => Vec::new(),
        };
        let field_ty = |name: &str| -> Option<Ty> {
            declared
                .iter()
                .find(|f| f.name == name)
                .map(|f| subst_type_params(&f.ty, &type_params, &type_args))
        };

        let mut fields: Vec<(String, Expr)> = Vec::new();
        for el in lit.children() {
            if el.kind() != SyntaxKind::Element {
                continue;
            }
            let fname = ident_text(&el);
            let expected = field_ty(&fname);
            if expected.is_none() {
                self.err(el.span(), format!("`{struct_name}` has no field `{fname}`"));
            }
            if fields.iter().any(|(n, _)| n == &fname) {
                self.err(
                    el.span(),
                    format!("field `{fname}` is given more than once"),
                );
            }
            let Some(value_node) = self.first_value(&el) else {
                continue;
            };
            let value = self.lower_value(&value_node, expected.as_ref());
            if let Some(want) = &expected {
                self.check_assignable(
                    want,
                    &value,
                    &format!("field `{fname}` of `{struct_name}`"),
                    value_node.span(),
                );
            }
            fields.push((fname, value));
        }

        for f in &declared {
            if fields.iter().any(|(n, _)| n == &f.name) {
                continue;
            }
            let concrete = subst_type_params(&f.ty, &type_params, &type_args);
            if let Some(zero) = self.zero_value(&concrete, lit.span(), &struct_name, &f.name) {
                fields.push((f.name.clone(), zero));
            }
        }

        fields.sort_by_key(|(n, _)| {
            declared
                .iter()
                .position(|f| &f.name == n)
                .unwrap_or(usize::MAX)
        });
        Some(Composite::Struct { ty, fields })
    }

    fn composite_type(&mut self, lit: &SyntaxNode) -> Option<Ty> {
        let head = lit.children().into_iter().find(|c| {
            is_type(c.kind()) || matches!(c.kind(), SyntaxKind::NameExpr | SyntaxKind::CallExpr)
        })?;
        let ty = if is_type(head.kind()) {
            self.checked_type(&head)?
        } else {
            expr_as_type(&head)?
        };
        self.check_type_in_scope(&ty, head.span());
        Some(ty)
    }

    /// A declaration's initializer node, which may be an ordinary expression or a
    /// bare composite literal
    fn first_value_of(&self, node: &SyntaxNode) -> Option<SyntaxNode> {
        self.first_expr(node)
    }

    /// The value node of a composite literal element, skipping the field-name
    /// token and any type prefix
    fn first_value(&self, el: &SyntaxNode) -> Option<SyntaxNode> {
        self.first_expr(el)
    }

    fn lower_value(&mut self, node: &SyntaxNode, target: Option<&Ty>) -> Expr {
        if node.kind() == SyntaxKind::CompositeLit {
            let (kind, ty) = self.lower_struct_lit(node, target);
            return Expr {
                kind,
                ty,
                span: node.span(),
            };
        }
        self.lower_expr(node)
    }

    fn zero_value(&mut self, ty: &Ty, span: Span, struct_name: &str, field: &str) -> Option<Expr> {
        let kind = match ty {
            Ty::Bool => ExprKind::Bool(false),
            Ty::CChar => ExprKind::Int(0),
            Ty::Int { .. } => ExprKind::Int(0),
            Ty::Float { .. } => ExprKind::Float(0.0),
            // Non-nullable by construction: there is no "absent" value to use.
            Ty::Ptr(_) | Ty::Rc(_) => {
                self.err(
                    span,
                    format!(
                        "field `{field}` of `{struct_name}` is a non-nullable pointer and has no \
                         zero value, so it must be given"
                    ),
                );
                return None;
            }
            Ty::Named(n) | Ty::App(n, _) => {
                return self.zero_aggregate(ty, n, span, struct_name, field);
            }
            Ty::Array(..) | Ty::Slice(_) => ExprKind::ZeroValue(ty.clone()),
            Ty::Void | Ty::Infer => {
                self.err(
                    span,
                    format!(
                        "field `{field}` of `{struct_name}` has no zero value, so it must be given"
                    ),
                );
                return None;
            }
        };
        Some(Expr {
            kind,
            ty: ty.clone(),
            span,
        })
    }

    /// The zero value of a named struct (recursively zeroed) or enum (its first
    /// payload-less variant:`.None` for `Maybe(T)`).
    ///
    /// The variant is constructed explicitly rather than relying on a zeroed tag:
    /// the tag's zero is the *first* variant, which for `Maybe(T) { Some(T), None }`
    /// would be `Some` with a garbage payload.
    fn zero_aggregate(
        &mut self,
        ty: &Ty,
        name: &str,
        span: Span,
        struct_name: &str,
        field: &str,
    ) -> Option<Expr> {
        if self.zeroing.iter().any(|n| n == name) {
            return None;
        }
        if let Some(declared) = self.structs.get(name).cloned() {
            let type_params = self
                .struct_type_params
                .get(name)
                .cloned()
                .unwrap_or_default();
            let type_args: Vec<Ty> = match ty {
                Ty::App(_, args) => args.clone(),
                _ => Vec::new(),
            };

            let mut fields = Vec::new();
            self.zeroing.push(name.to_string());
            for f in &declared {
                let concrete = subst_type_params(&f.ty, &type_params, &type_args);
                match self.zero_value(&concrete, span, name, &f.name) {
                    Some(zero) => fields.push((f.name.clone(), zero)),
                    None => {
                        self.zeroing.pop();
                        return None;
                    }
                }
            }

            self.zeroing.pop();
            return Some(Expr {
                kind: ExprKind::StructLit {
                    ty: ty.clone(),
                    fields,
                },
                ty: ty.clone(),
                span,
            });
        }

        if let Some(variants) = self.enums.get(name).cloned() {
            let Some(empty) = variants.iter().find(|v| v.payload.is_empty()) else {
                self.err(
                    span,
                    format!(
                        "field `{field}` of `{struct_name}` is an enum with no payload-less \
                         variant, so it has no zero value and must be given"
                    ),
                );
                return None;
            };
            return Some(Expr {
                kind: ExprKind::EnumInit {
                    enum_name: name.to_string(),
                    variant: empty.name.clone(),
                    args: Vec::new(),
                },
                ty: ty.clone(),
                span,
            });
        }

        self.err(
            span,
            format!("field `{field}` of `{struct_name}` has no zero value, so it must be given"),
        );
        None
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
        let lowered = self.lower_expr(&e);
        self.expect_bool(&lowered, "a condition", e.span());
        Some(lowered)
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

fn coerces_to(expected: &Ty, value: &Expr) -> bool {
    match expected {
        Ty::Int { .. } => is_untyped_int_literal(value),
        Ty::Float { .. } => is_untyped_float_literal(value),
        _ => false,
    }
}

/// An integer literal, seeing through parentheses and a leading sign
fn is_untyped_int_literal(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Int(_) => true,
        ExprKind::Paren(inner) => is_untyped_int_literal(inner),
        ExprKind::Unary {
            op: UnOp::Neg,
            operand,
        } => is_untyped_int_literal(operand),
        _ => false,
    }
}

fn is_untyped_float_literal(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Float(_) => true,
        ExprKind::Paren(inner) => is_untyped_float_literal(inner),
        ExprKind::Unary {
            op: UnOp::Neg,
            operand,
        } => is_untyped_float_literal(operand),
        _ => false,
    }
}

/// A type as it is written in Dray source, for diagnostics
fn type_name(ty: &Ty) -> String {
    match ty {
        Ty::Void => "void".to_string(),
        Ty::Bool => "bool".to_string(),
        Ty::CChar => "cchar".to_string(),
        Ty::Int { bits, signed } => {
            let prefix = if *signed { "int" } else { "uint" };
            match bits {
                IntWidth::Size => {
                    if *signed {
                        "isize".to_string()
                    } else {
                        "usize".to_string()
                    }
                }
                IntWidth::W8 => format!("{prefix}8"),
                IntWidth::W16 => format!("{prefix}16"),
                IntWidth::W32 => format!("{prefix}32"),
                IntWidth::W64 => format!("{prefix}64"),
            }
        }
        Ty::Float { bits } => format!("float{bits}"),
        Ty::Named(n) => n.clone(),
        Ty::App(n, args) => {
            let inner: Vec<String> = args.iter().map(type_name).collect();
            format!("{n}({})", inner.join(", "))
        }
        Ty::Ptr(inner) => format!("*{}", type_name(inner)),
        Ty::Array(elem, n) => format!("[{n}]{}", type_name(elem)),
        Ty::Slice(elem) => format!("[]{}", type_name(elem)),
        Ty::Rc(inner) => format!("@{}", type_name(inner)),
        Ty::Infer => "?".to_string(),
    }
}

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
            | SyntaxKind::SliceExpr
            | SyntaxKind::CastExpr
            | SyntaxKind::AllocExpr
            | SyntaxKind::CompositeLit
    )
}

fn first_ident(node: &SyntaxNode) -> Option<String> {
    node.token_of_kind(SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

enum Composite {
    Struct { ty: Ty, fields: Vec<(String, Expr)> },
    Array { ty: Ty, elements: Vec<Expr> },
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

fn declared_param_types(node: &SyntaxNode) -> Vec<Ty> {
    node.child_of_kind(SyntaxKind::ParamList)
        .map(|pl| {
            pl.children()
                .iter()
                .filter(|p| {
                    p.kind() == SyntaxKind::Param
                        && p.token_of_kind(SyntaxKind::KwComptime).is_none()
                })
                .map(|p| {
                    p.children()
                        .into_iter()
                        .find(|c| is_type(c.kind()))
                        .and_then(|t| lower_type(&t))
                        .unwrap_or(Ty::Infer)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Match a declared parameter type against an actual argument type, binding any
/// type parameter it mentions
fn infer_type_params(
    param_ty: &Ty,
    arg_ty: &Ty,
    type_params: &[String],
    bindings: &mut HashMap<String, Ty>,
) {
    match (param_ty, arg_ty) {
        (Ty::Named(p), actual) if type_params.iter().any(|tp| tp == p) => {
            bindings.entry(p.clone()).or_insert_with(|| actual.clone());
        }
        (Ty::Ptr(pi), Ty::Ptr(ai)) | (Ty::Rc(pi), Ty::Rc(ai)) => {
            infer_type_params(pi, ai, type_params, bindings)
        }
        (Ty::App(pn, pargs), Ty::App(an, aargs)) if pn == an => {
            for (p, a) in pargs.iter().zip(aargs) {
                infer_type_params(p, a, type_params, bindings);
            }
        }
        _ => {}
    }
}

fn const_int(e: &Expr) -> Option<i64> {
    match &e.kind {
        ExprKind::Int(v) => Some(*v),
        ExprKind::Paren(inner) => const_int(inner),
        ExprKind::Unary {
            op: UnOp::Neg,
            operand,
        } => const_int(operand).map(|v| -v),
        _ => None,
    }
}

fn declares_variadic(node: &SyntaxNode) -> bool {
    node.child_of_kind(SyntaxKind::ParamList).is_some_and(|pl| {
        pl.children_with_tokens()
            .iter()
            .any(|el| matches!(el, SyntaxElement::Token(t) if t.kind() == SyntaxKind::DotDotDot))
    })
}

fn fits_arity(given: usize, arity: usize, variadic: bool) -> bool {
    if variadic {
        given >= arity
    } else {
        given == arity
    }
}

fn mentions_rc(ty: &Ty) -> bool {
    match ty {
        Ty::Rc(_) => true,
        Ty::Ptr(inner) | Ty::Array(inner, _) | Ty::Slice(inner) => mentions_rc(inner),
        Ty::App(_, args) => args.iter().any(mentions_rc),
        _ => false,
    }
}

fn operator_name(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
    }
}

fn loop_bindings(node: &SyntaxNode) -> Vec<String> {
    let mut out = Vec::new();
    for el in node.children_with_tokens() {
        match el {
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::KwIn => break,
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::Ident => {
                out.push(t.text().to_string())
            }
            _ => {}
        }
    }
    out
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

fn collect_type_nodes(node: &SyntaxNode, out: &mut Vec<(SyntaxNode, bool)>) {
    collect_type_nodes_inner(node, false, out)
}

fn collect_type_nodes_inner(
    node: &SyntaxNode,
    at_boundary: bool,
    out: &mut Vec<(SyntaxNode, bool)>,
) {
    for child in node.children() {
        if child.kind() == SyntaxKind::Param
            && child.token_of_kind(SyntaxKind::KwComptime).is_some()
        {
            continue;
        }
        if is_type(child.kind()) {
            out.push((child, at_boundary));
        } else {
            let boundary = at_boundary || child.kind() == SyntaxKind::CastExpr;
            collect_type_nodes_inner(&child, boundary, out);
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
