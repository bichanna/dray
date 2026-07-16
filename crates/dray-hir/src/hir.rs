// SPDX-License-Identifier: Apache-2.0

//! The High-level IR (HIR): a resolved, typed tree produced from the CST.

use dray_syntax::Span;

/// A stable identifier for a definition: a proc, an extern, a parameter, or a
/// local variable. Indexes into the [`Hir::defs`] arena
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DefId(pub u32);

#[derive(Debug, Clone)]
pub struct Hir {
    pub items: Vec<Item>,
    pub defs: Vec<DefInfo>,
}

impl Hir {
    pub fn def(&self, id: DefId) -> &DefInfo {
        &self.defs[id.0 as usize]
    }
}

/// What a `DefId` refers to.
#[derive(Debug, Clone)]
pub struct DefInfo {
    pub name: String,
    pub kind: DefKind,
    pub ty: Ty,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DefKind {
    /// A Dray `proc`. Its C name is the Dray name.
    Proc,
    /// An `extern "symbol" proc`. Its C name is `symbol`, which may differ from
    /// the Dray binding name — this is what fixes call-site aliasing.
    ExternProc { symbol: String },
    /// A proc parameter.
    Param,
    /// A local variable (`:=` / `: T =` / …).
    Local,
    /// A struct type. Used to resolve `Ty::Named` and to type field access.
    Struct,
    /// An enum type. Used to resolve `Ty::Named` and enum construction/patterns.
    Enum,
}

/// A top-level item.
#[derive(Debug, Clone)]
pub enum Item {
    Include(String),
    Proc(Proc),
    ExternProc(ExternProc),
    Struct(StructDef),
    Enum(EnumDef),
}

/// An algebraic enum: an ordered list of variants, each with a (possibly empty)
/// tuple payload.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDef {
    pub def: DefId,
    pub name: String,
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub name: String,
    /// The payload types, in order. Empty for a unit variant (`Nothing`).
    pub payload: Vec<Ty>,
}

/// A struct type declaration: an ordered list of typed fields
#[derive(Debug, Clone, PartialEq)]
pub struct StructDef {
    pub def: DefId,
    pub name: String,
    /// Comptime type-parameter names (`["T"]` for `Box(comptime T: type)`).
    /// Empty for a non-generic struct.
    pub type_params: Vec<String>,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
}

#[derive(Debug, Clone)]
pub struct Proc {
    pub def: DefId,
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Ty,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct ExternProc {
    pub def: DefId,
    /// The Dray binding name.
    pub name: String,
    /// The linked C symbol (from `extern "symbol"`).
    pub symbol: String,
    pub params: Vec<Param>,
    pub ret: Ty,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub def: DefId,
    pub name: String,
    pub ty: Ty,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    /// A variable binding. `ty` is the declared type if given, else inferred.
    Let {
        def: DefId,
        name: String,
        ty: Ty,
        init: Expr,
    },
    /// `target <op> value`.
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
    /// `for cond { … }` (surface while-style).
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    /// `for init; cond; post { … }` (surface C-style). Any part may be absent.
    CFor {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        post: Option<Box<Stmt>>,
        body: Vec<Stmt>,
    },
    /// `for { … }` (surface infinite).
    Loop {
        body: Vec<Stmt>,
    },
    /// `switch scrutinee { case Pat: … }`.
    Switch {
        scrutinee: Expr,
        arms: Vec<Arm>,
    },
}

/// One `case` of a switch: a pattern and the statements to run on a match.
#[derive(Debug, Clone)]
pub struct Arm {
    pub pattern: Pattern,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub enum Pattern {
    /// `Enum.Variant(bindings)`
    Enum {
        enum_name: String,
        variant: String,
        bindings: Vec<String>,
    },
    /// A value pattern (e.g. `case 3:` or `case true:`).
    Value(Expr),
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Ty,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Str(String),
    Char(char),
    Bool(bool),
    /// A resolved reference to a definition (var/param/proc/extern).
    Name {
        def: DefId,
        name: String,
    },
    /// A name that failed to resolve. Kept so codegen can still be attempted and
    /// so one bad name doesn't sink the whole lowering. Always paired with a
    /// resolve error.
    Unresolved(String),
    Unary {
        op: UnOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    Field {
        recv: Box<Expr>,
        member: String,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Cast {
        ty: Ty,
        operand: Box<Expr>,
    },
    Alloc {
        ty: Ty,
        fields: Vec<(String, Expr)>,
    },
    EnumInit {
        enum_name: String,
        variant: String,
        args: Vec<Expr>,
    },
    Paren(Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    LogicNot,
    BitNot,
    AddrOf,
    Deref,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl BinOp {
    /// Comparison and logical operators produce a boolean regardless of operand
    /// type; arithmetic/bitwise ones produce the operand type.
    pub fn is_boolean(self) -> bool {
        matches!(
            self,
            BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::And
                | BinOp::Or
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

// ── types ────────────────────────────────────────────────────────────────────

/// A resolved type. `Named` is an unrecognized name (a future user type); `Infer`
/// is a placeholder the inferencer couldn't pin down (defaults to `int32` at
/// codegen). No generics/slices/arrays yet — those are rejected during lowering.
#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Void,
    Bool,
    /// Signed/unsigned integer of a bit width (8/16/32/64) or pointer-size.
    Int {
        bits: IntWidth,
        signed: bool,
    },
    /// 32- or 64-bit float.
    Float {
        bits: u8,
    },
    Ptr(Box<Ty>),
    Rc(Box<Ty>),
    Named(String),
    App(String, Vec<Ty>),
    Infer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntWidth {
    W8,
    W16,
    W32,
    W64,
    Size,
}

impl Ty {
    pub fn i32() -> Ty {
        Ty::Int {
            bits: IntWidth::W32,
            signed: true,
        }
    }
    pub fn i64() -> Ty {
        Ty::Int {
            bits: IntWidth::W64,
            signed: true,
        }
    }
    pub fn i8() -> Ty {
        Ty::Int {
            bits: IntWidth::W8,
            signed: true,
        }
    }
    pub fn f64() -> Ty {
        Ty::Float { bits: 64 }
    }
}
