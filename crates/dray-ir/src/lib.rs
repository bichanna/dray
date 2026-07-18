// SPDX-License-Identifier: Apache-2.0

//! `dray-ir` — the mid-level IR: the HIR with reference counting spelled out.

use dray_hir::{DefId, DefInfo, DefKind, Expr, ExprKind, Hir, Ty};

pub use dray_hir::{Arm, AssignOp, BinOp, EnumDef, Field, Pattern, StructDef, UnOp, Variant};

mod debug;
pub use debug::dump_ir;

#[derive(Debug, Clone)]
pub struct Ir {
    pub items: Vec<Item>,
    pub structs: Vec<StructDef>,
    /// Enum type declarations, carried straight through from HIR.
    pub enums: Vec<EnumDef>,
    /// The definition arena, carried over from HIR plus any temporaries this pass
    /// introduces (see [`Lowerer::fresh_temp`]).
    pub defs: Vec<DefInfo>,
    /// True once any RC op was emitted. Codegen only pulls in the RC runtime then.
    pub uses_rc: bool,
}

impl Ir {
    pub fn def(&self, id: DefId) -> &DefInfo {
        &self.defs[id.0 as usize]
    }
}

#[derive(Debug, Clone)]
pub enum Item {
    Include(String),
    Proc(Proc),
    ExternProc(ExternProc),
}

#[derive(Debug, Clone)]
pub struct Proc {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Ty,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct ExternProc {
    pub name: String,
    pub symbol: String,
    pub params: Vec<Param>,
    pub ret: Ty,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Ty,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        name: String,
        ty: Ty,
        init: Expr,
    },
    Assign {
        target: Expr,
        op: AssignOp,
        value: Expr,
    },
    Return(Option<Expr>),
    Break,
    Continue,
    Expr(Expr),
    If {
        cond: Expr,
        then_branch: Vec<Stmt>,
        else_branch: Option<Vec<Stmt>>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    CFor {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        post: Option<Box<Stmt>>,
        body: Vec<Stmt>,
    },
    Loop {
        body: Vec<Stmt>,
    },
    Switch {
        scrutinee: Expr,
        arms: Vec<SwitchArm>,
    },
    /// strong += 1
    Retain(String),
    /// strong -= 1, free at zero
    Release(String),
    /// manual `free` (spec §4.6); emitted like a release for now
    Free(String),
}

#[derive(Debug, Clone)]
pub struct SwitchArm {
    pub pattern: Pattern,
    pub body: Vec<Stmt>,
}

pub fn lower(hir: &Hir) -> Ir {
    let mut lw = Lowerer {
        defs: hir.defs.clone(),
        uses_rc: false,
        temp: 0,
    };
    let items = hir.items.iter().filter_map(|it| lw.item(it)).collect();
    let structs = hir
        .items
        .iter()
        .filter_map(|it| match it {
            dray_hir::Item::Struct(sd) => Some(sd.clone()),
            _ => None,
        })
        .collect();
    let enums = hir
        .items
        .iter()
        .filter_map(|it| match it {
            dray_hir::Item::Enum(ed) => Some(ed.clone()),
            _ => None,
        })
        .collect();
    Ir {
        items,
        structs,
        enums,
        defs: lw.defs,
        uses_rc: lw.uses_rc,
    }
}

struct Lowerer {
    defs: Vec<DefInfo>,
    uses_rc: bool,
    temp: u32,
}

/// The `@T` locals currently in scope, one `Vec` per open block (innermost last).
/// This is the entire state the RC pass needs: to release at a block's end we pop
/// its `Vec`; to release at a `return` we flatten the whole stack.
type Scopes = Vec<Vec<String>>;

impl Lowerer {
    fn item(&mut self, item: &dray_hir::Item) -> Option<Item> {
        match item {
            dray_hir::Item::Struct(_) => None, // collected into Ir.structs directly
            dray_hir::Item::Enum(_) => None,   // collected into Ir.enums directly
            dray_hir::Item::Include(h) => Some(Item::Include(h.clone())),
            dray_hir::Item::ExternProc(e) => Some(Item::ExternProc(ExternProc {
                name: e.name.clone(),
                symbol: e.symbol.clone(),
                params: e.params.iter().map(param).collect(),
                ret: e.ret.clone(),
            })),
            dray_hir::Item::Proc(p) => {
                let mut scopes: Scopes = vec![Vec::new()];
                let mut body = Vec::new();
                for s in &p.body {
                    self.stmt(s, &mut scopes, &mut body);
                }
                // If control falls off the end (no trailing return), release the
                // proc's top-level @T locals here.
                if !ends_in_return(&p.body) {
                    let top = scopes.last().unwrap().clone();
                    self.release(&top, &mut body);
                }
                Some(Item::Proc(Proc {
                    name: p.name.clone(),
                    params: p.params.iter().map(param).collect(),
                    ret: p.ret.clone(),
                    body,
                }))
            }
        }
    }

    /// Lower one HIR statement into `out`, updating the open-scope stack. This is
    /// where rules 1 and 2 (retain-or-not on a binding) live.
    fn stmt(&mut self, s: &dray_hir::Stmt, scopes: &mut Scopes, out: &mut Vec<Stmt>) {
        use dray_hir::Stmt as H;
        match s {
            H::Let { name, ty, init, .. } => {
                // Composite `alloc T{...}` initializers with @T fields whose values
                // are Names of live @T locals need those sources retained: the new
                // allocation is about to hold the same pointer, so the source
                // binding's implicit +1 must be duplicated
                self.emit_field_retains(init, out);

                out.push(Stmt::Let {
                    name: name.clone(),
                    ty: ty.clone(),
                    init: init.clone(),
                });
                if matches!(ty, Ty::Rc(_)) {
                    scopes.last_mut().unwrap().push(name.clone());
                    if is_rc_borrow(init) {
                        // rule 2: a borrowed @T (Name, Field, …) → retain the
                        // new binding so the slot holds its own +1.
                        self.emit_retain(name.clone(), out);
                    }
                    // rule 1: fresh from `alloc` / call → already owned, nothing
                    // to emit.
                }
            }
            H::Assign { target, op, value } => self.lower_assign(target, *op, value, scopes, out),
            H::Return(expr) => self.lower_return(expr.as_ref(), scopes, out),
            H::Break => out.push(Stmt::Break),
            H::Continue => out.push(Stmt::Continue),
            H::Expr(e) => {
                self.emit_field_retains(e, out);
                out.push(Stmt::Expr(e.clone()));
            }
            H::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let then_branch = self.block(then_branch, scopes);
                let else_branch = else_branch.as_ref().map(|b| self.block(b, scopes));
                out.push(Stmt::If {
                    cond: cond.clone(),
                    then_branch,
                    else_branch,
                });
            }
            H::While { cond, body } => {
                let body = self.block(body, scopes);
                out.push(Stmt::While {
                    cond: cond.clone(),
                    body,
                });
            }
            H::Loop { body } => {
                let body = self.block(body, scopes);
                out.push(Stmt::Loop { body });
            }
            H::CFor {
                init,
                cond,
                post,
                body,
            } => {
                scopes.push(Vec::new());
                let init = init.as_ref().map(|s| Box::new(self.single(s, scopes)));
                let post = post.as_ref().map(|s| Box::new(self.single(s, scopes)));
                let body = self.block(body, scopes);
                scopes.pop();
                out.push(Stmt::CFor {
                    init,
                    cond: cond.clone(),
                    post,
                    body,
                });
            }
            H::Switch { scrutinee, arms } => {
                let arms = arms
                    .iter()
                    .map(|a| SwitchArm {
                        pattern: a.pattern.clone(),
                        body: self.block(&a.body, scopes),
                    })
                    .collect();
                out.push(Stmt::Switch {
                    scrutinee: scrutinee.clone(),
                    arms,
                });
            }
        }
    }

    fn block(&mut self, stmts: &[dray_hir::Stmt], scopes: &mut Scopes) -> Vec<Stmt> {
        scopes.push(Vec::new());
        let mut out = Vec::new();
        for s in stmts {
            self.stmt(s, scopes, &mut out);
        }
        let scope = scopes.pop().unwrap();
        if !ends_in_return(stmts) {
            self.release(&scope, &mut out);
        }
        out
    }

    fn single(&mut self, s: &dray_hir::Stmt, scopes: &mut Scopes) -> Stmt {
        let mut tmp = Vec::new();
        self.stmt(s, scopes, &mut tmp);
        tmp.into_iter().next().unwrap_or(Stmt::Break)
    }

    fn lower_return(&mut self, expr: Option<&Expr>, scopes: &Scopes, out: &mut Vec<Stmt>) {
        let transferred = expr.and_then(|e| transferred_local(scopes, e));
        let mut live: Vec<String> = scopes.iter().flatten().cloned().collect();
        if let Some(t) = &transferred
            && let Some(idx) = live.iter().rposition(|n| n == t)
        {
            live.remove(idx);
        }

        match expr {
            Some(e) if transferred.is_some() => {
                self.release(&live, out);
                out.push(Stmt::Return(Some(e.clone())));
            }
            Some(e) if !live.is_empty() => {
                let tmp = self.fresh_temp(e.ty.clone());
                let name_expr = self.name_expr(&tmp, e.ty.clone(), e.span);
                out.push(Stmt::Let {
                    name: tmp,
                    ty: e.ty.clone(),
                    init: e.clone(),
                });
                self.release(&live, out);
                out.push(Stmt::Return(Some(name_expr)));
            }
            Some(e) => out.push(Stmt::Return(Some(e.clone()))),
            None => {
                self.release(&live, out);
                out.push(Stmt::Return(None));
            }
        }
    }

    fn lower_assign(
        &mut self,
        target: &Expr,
        op: AssignOp,
        value: &Expr,
        scopes: &Scopes,
        out: &mut Vec<Stmt>,
    ) {
        // Field retains apply regardless of whether the target is @T.
        self.emit_field_retains(value, out);

        let target_name = if matches!(op, AssignOp::Assign)
            && matches!(target.ty, Ty::Rc(_))
            && let ExprKind::Name { name, .. } = &target.kind
            && is_live_rc_local(scopes, name)
        {
            Some(name.clone())
        } else {
            None
        };

        if let Some(target_name) = target_name {
            // Full ARC assign sequence:
            //   1. Save the current pointer in a synthetic local — NOT tracked
            //      in `scopes` so there's no auto-retain and no scope-exit
            //      release; this is a raw copy just to keep the old value
            //      reachable through the assignment.
            //   2. Do the assignment. The RHS is evaluated first (in C's
            //      normal order), so patterns like `n = n.next` still read
            //      through the *current* target before we drop it.
            //   3. If the RHS is a borrow (any @T that isn't a fresh `alloc`
            //      or a call return), retain the target's new pointee — the
            //      slot needs its own +1 on top of whatever the source binding
            //      still holds.
            //   4. Release the saved old value.
            let old = self.fresh_temp(target.ty.clone());
            out.push(Stmt::Let {
                name: old.clone(),
                ty: target.ty.clone(),
                init: target.clone(),
            });
            out.push(Stmt::Assign {
                target: target.clone(),
                op,
                value: value.clone(),
            });
            if is_rc_borrow(value) {
                self.emit_retain(target_name, out);
            }
            self.uses_rc = true;
            out.push(Stmt::Release(old));
        } else {
            out.push(Stmt::Assign {
                target: target.clone(),
                op,
                value: value.clone(),
            });
        }
    }

    fn emit_field_retains(&mut self, e: &Expr, out: &mut Vec<Stmt>) {
        match &e.kind {
            ExprKind::Alloc { fields, .. } => {
                for (_, val) in fields {
                    self.emit_field_retains(val, out);
                    if matches!(val.ty, Ty::Rc(_))
                        && let ExprKind::Name { name, .. } = &val.kind
                    {
                        self.emit_retain(name.clone(), out);
                    }
                }
            }
            ExprKind::Call { callee, args } => {
                self.emit_field_retains(callee, out);
                for a in args {
                    self.emit_field_retains(a, out);
                }
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                self.emit_field_retains(lhs, out);
                self.emit_field_retains(rhs, out);
            }
            ExprKind::Unary { operand, .. } => self.emit_field_retains(operand, out),
            ExprKind::Paren(inner) => self.emit_field_retains(inner, out),
            ExprKind::Cast { operand, .. } => self.emit_field_retains(operand, out),
            ExprKind::Field { recv, .. } => self.emit_field_retains(recv, out),
            ExprKind::Index { base, index } => {
                self.emit_field_retains(base, out);
                self.emit_field_retains(index, out);
            }
            _ => {}
        }
    }

    fn emit_retain(&mut self, name: String, out: &mut Vec<Stmt>) {
        self.uses_rc = true;
        out.push(Stmt::Retain(name));
    }

    /// Emit releases for `names` in reverse declaration order (LIFO — last one
    /// bound is the first one freed).
    fn release(&mut self, names: &[String], out: &mut Vec<Stmt>) {
        for name in names.iter().rev() {
            self.uses_rc = true;
            out.push(Stmt::Release(name.clone()));
        }
    }

    /// Mint a fresh local (used to hold a return value across its releases) and
    /// register it in the def arena so codegen can name it.
    fn fresh_temp(&mut self, ty: Ty) -> String {
        let name = format!("__rc_tmp_{}", self.temp);
        self.temp += 1;
        self.defs.push(DefInfo {
            name: name.clone(),
            kind: DefKind::Local,
            ty,
        });
        name
    }

    fn name_expr(&self, name: &str, ty: Ty, span: dray_hir::Span) -> Expr {
        let def = DefId((self.defs.len() - 1) as u32);
        Expr {
            kind: ExprKind::Name {
                def,
                name: name.to_string(),
            },
            ty,
            span,
        }
    }
}

fn param(p: &dray_hir::Param) -> Param {
    Param {
        name: p.name.clone(),
        ty: p.ty.clone(),
    }
}

fn is_rc_borrow(e: &Expr) -> bool {
    matches!(e.ty, Ty::Rc(_)) && !matches!(e.kind, ExprKind::Alloc { .. } | ExprKind::Call { .. })
}

fn is_live_rc_local(scopes: &Scopes, name: &str) -> bool {
    scopes.iter().any(|scope| scope.iter().any(|n| n == name))
}

fn transferred_local(scopes: &Scopes, e: &Expr) -> Option<String> {
    if let ExprKind::Name { name, .. } = &e.kind
        && matches!(e.ty, Ty::Rc(_))
        && is_live_rc_local(scopes, name)
    {
        Some(name.clone())
    } else {
        None
    }
}

fn ends_in_return(stmts: &[dray_hir::Stmt]) -> bool {
    matches!(stmts.last(), Some(dray_hir::Stmt::Return(_)))
}
