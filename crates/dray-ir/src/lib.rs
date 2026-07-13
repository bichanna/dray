// SPDX-License-Identifier: Apache-2.0

//! `dray-ir` — the mid-level IR: the HIR with reference counting spelled out.

use dray_hir::{DefId, DefInfo, DefKind, Expr, ExprKind, Hir, Ty};

pub use dray_hir::{AssignOp, BinOp, UnOp};

mod debug;
pub use debug::dump_ir;

#[derive(Debug, Clone)]
pub struct Ir {
    pub items: Vec<Item>,
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
    /// strong += 1
    Retain(String),
    /// strong -= 1, free at zero
    Release(String),
    /// manual `free` (spec §4.6); emitted like a release for now
    Free(String),
}

pub fn lower(hir: &Hir) -> Ir {
    let mut lw = Lowerer {
        defs: hir.defs.clone(),
        uses_rc: false,
        temp: 0,
    };
    let items = hir.items.iter().map(|it| lw.item(it)).collect();
    Ir {
        items,
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
    fn item(&mut self, item: &dray_hir::Item) -> Item {
        match item {
            dray_hir::Item::Include(h) => Item::Include(h.clone()),
            dray_hir::Item::ExternProc(e) => Item::ExternProc(ExternProc {
                name: e.name.clone(),
                symbol: e.symbol.clone(),
                params: e.params.iter().map(param).collect(),
                ret: e.ret.clone(),
            }),
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
                Item::Proc(Proc {
                    name: p.name.clone(),
                    params: p.params.iter().map(param).collect(),
                    ret: p.ret.clone(),
                    body,
                })
            }
        }
    }

    /// Lower one HIR statement into `out`, updating the open-scope stack. This is
    /// where rules 1 and 2 (retain-or-not on a binding) live.
    fn stmt(&mut self, s: &dray_hir::Stmt, scopes: &mut Scopes, out: &mut Vec<Stmt>) {
        use dray_hir::Stmt as H;
        match s {
            H::Let { name, ty, init, .. } => {
                out.push(Stmt::Let {
                    name: name.clone(),
                    ty: ty.clone(),
                    init: init.clone(),
                });
                if matches!(ty, Ty::Rc(_)) {
                    scopes.last_mut().unwrap().push(name.clone());
                    if is_rc_copy(init) {
                        // rule 2: a copy of an existing @T → retain.
                        self.emit_retain(name.clone(), out);
                    }
                    // rule 1: fresh from `alloc` → already owned, nothing to emit.
                }
            }
            H::Assign { target, op, value } => out.push(Stmt::Assign {
                target: target.clone(),
                op: *op,
                value: value.clone(),
            }),
            H::Return(expr) => self.lower_return(expr.as_ref(), scopes, out),
            H::Break => out.push(Stmt::Break),
            H::Continue => out.push(Stmt::Continue),
            H::Expr(e) => out.push(Stmt::Expr(e.clone())),
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
        let live: Vec<String> = scopes.iter().flatten().cloned().collect();
        match expr {
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

/// A binding's initializer is a "copy" of another `@T` when it's a bare name of
/// RC type (`b := a`) — as opposed to a fresh `alloc` or a transfer.
fn is_rc_copy(init: &Expr) -> bool {
    matches!(init.kind, ExprKind::Name { .. }) && matches!(init.ty, Ty::Rc(_))
}

fn ends_in_return(stmts: &[dray_hir::Stmt]) -> bool {
    matches!(stmts.last(), Some(dray_hir::Stmt::Return(_)))
}
