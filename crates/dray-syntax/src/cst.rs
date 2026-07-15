// SPDX-License-Identifier: Apache-2.0

//! The concrete syntax tree.

use crate::token::{Span, TokenKind};
use std::rc::Rc;

/// The kind of a CST node or token.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum SyntaxKind {
    // ── tokens (leaf elements) ───────────────────────────────────────────────
    // Literals
    IntLit,
    FloatLit,
    StringLit,
    RuneLit,
    // Identifiers / keywords
    Ident,
    KwProc,
    KwStruct,
    KwEnum,
    KwSwitch,
    KwCase,
    KwDefault,
    KwIf,
    KwElse,
    KwFor,
    KwIn,
    KwReturn,
    KwAlloc,
    KwTryAlloc,
    KwCast,
    KwPub,
    KwImport,
    KwExtern,
    KwCHeader,
    KwBreak,
    KwContinue,
    KwComptime,
    KwTrue,
    KwFalse,
    // Binding / assignment operators
    ColonColonEq,
    ColonColon,
    ColonEq,
    Colon,
    Eq,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    AmpEq,
    PipeEq,
    CaretEq,
    ShlEq,
    ShrEq,
    // Arithmetic / bitwise
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Shl,
    Shr,
    // Comparison / logical
    EqEq,
    BangEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    AmpAmp,
    PipePipe,
    Bang,
    // Punctuation
    At,
    Arrow,
    DotDotDot,
    Dot,
    Comma,
    Semi,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    // Trivia tokens
    Whitespace,
    LineComment,
    BlockComment,
    // A lexer error token, carried into the tree so nothing is lost.
    LexError,

    // ── nodes (interior) ─────────────────────────────────────────────────────
    /// The whole file: `{ TopLevelDecl }`.
    SourceFile,
    /// `[ "pub" ] identifier "::" ConstExpr` for the proc case.
    ProcDef,
    /// `c_header ( string_lit ) ;`
    CHeaderDecl,
    /// `[ "pub" ] identifier "::" "extern" string_lit "proc" "(" ParamList ")"
    /// [ "->" Type ] ";"` — an externally-linked C function (spec §16).
    ExternProcDecl,
    /// `[ \"pub\" ] identifier \"::\" \"struct\" \"{\" { FieldDecl } \"}\"` — a struct
    /// type declaration. (The generic-parameter receiver form is deferred.)
    StructDef,
    /// `identifier \":\" Type` — one field inside a `StructDef`.
    FieldDecl,
    /// `[ \"pub\" ] identifier \"::\" \"enum\" \"{\" { EnumVariant } \"}\"` — an
    /// algebraic enum declaration (the generic-parameter form is deferred).
    EnumDef,
    /// `identifier [ \"(\" TypeList \")\" ]` — one variant of an `EnumDef`.
    EnumVariant,
    /// `Type { \",\" Type }` — the payload type list of an `EnumVariant`.
    TypeList,
    /// `\"switch\" [ Expression ] \"{\" { CaseClause } \"}\"`.
    SwitchStmt,
    /// `\"case\" PatternList \":\" { Statement }`.
    CaseClause,
    /// `TypeName \".\" identifier [ \"(\" IdentifierList \")\" ]` — an enum pattern.
    EnumPattern,
    /// `( ParamList )`
    ParamList,
    /// `[ "comptime" ] identifier ":" Type`
    Param,
    /// The `-> Type` return clause.
    RetType,
    /// `{ Statement }`
    Block,

    // statements
    /// A variable declaration, either bare
    /// (`identifier ( "::" | ":=" | "::=" ) Expression`) or explicit-type
    /// (`identifier ":" Type ( ":" | "=" | ":=" ) Expression`), plus `;`.
    VarDecl,
    /// `Expression AssignOp Expression ;` — a single-target assignment.
    AssignStmt,
    /// `if [ SimpleStmt ";" ] Expression Block [ "else" ( Block | IfStmt ) ]`
    IfStmt,
    /// The `else ( Block | IfStmt )` tail of an `if`.
    ElseClause,
    /// A `for` loop in any of its four forms (infinite / while / C-style / range).
    ForStmt,
    /// A wrapper around the condition expression of an `if`/`for`/while, so it's
    /// distinguishable from a C-style loop's init/post statements. Not a grammar
    /// nonterminal — a CST grouping node.
    Condition,
    /// `return [ Expression ] ;`
    ReturnStmt,
    /// `break ;`
    BreakStmt,
    /// `continue ;`
    ContinueStmt,
    /// `Expression ;`
    ExprStmt,

    // types
    /// `*Type`
    PointerType,
    /// `@Type`
    RcPointerType,
    /// `[]Type`
    SliceType,
    /// `[ Expression ] Type`
    ArrayType,
    /// A bare type name: `int32`, `Node`, ...
    NameType,
    /// `TypeName ( ArgumentList )` — e.g. `Stack(int32)`, `Maybe(@Node)`.
    GenericType,

    // expressions
    /// A literal expression wrapping one literal token.
    LiteralExpr,
    /// A bare identifier used as an expression.
    NameExpr,
    /// `( Expression )`
    ParenExpr,
    /// A binary operation; the operator token sits between two operand nodes.
    BinaryExpr,
    /// A prefix unary operation: `-x`, `!x`, `~x`, `*x`, `&x`.
    PrefixExpr,
    /// `cast ( Type ) UnaryExpr`
    CastExpr,
    /// `( "alloc" | "try_alloc" ) ( CompositeLit | Type )`.
    AllocExpr,
    /// `[ Type ] "{" [ ElementList ] "}"` — a composite (struct) literal, e.g.
    /// `Node{ value: 1, next: n }`.
    CompositeLit,
    /// `[ identifier ":" ] Expression` — one initializer inside a `CompositeLit`.
    Element,
    /// A postfix call: `callee ( ArgumentList )`.
    CallExpr,
    /// A field selector: `receiver . identifier`.
    FieldExpr,
    /// A single-element index: `receiver [ Expression ]` (slice form deferred).
    IndexExpr,
    /// `Expression { "," Expression }` inside a `Call`.
    ArgList,

    /// A synthetic node the parser emits when it can't make progress, so the tree
    /// stays lossless and error-recovering (arch §5, §17.3).
    Error,
}

impl SyntaxKind {
    /// Map a lexer `TokenKind` to the matching leaf `SyntaxKind`.
    pub fn from_token(kind: TokenKind) -> SyntaxKind {
        match kind {
            TokenKind::IntLit => SyntaxKind::IntLit,
            TokenKind::FloatLit => SyntaxKind::FloatLit,
            TokenKind::StringLit => SyntaxKind::StringLit,
            TokenKind::RuneLit => SyntaxKind::RuneLit,
            TokenKind::Ident => SyntaxKind::Ident,
            TokenKind::KwProc => SyntaxKind::KwProc,
            TokenKind::KwStruct => SyntaxKind::KwStruct,
            TokenKind::KwEnum => SyntaxKind::KwEnum,
            TokenKind::KwSwitch => SyntaxKind::KwSwitch,
            TokenKind::KwCase => SyntaxKind::KwCase,
            TokenKind::KwDefault => SyntaxKind::KwDefault,
            TokenKind::KwIf => SyntaxKind::KwIf,
            TokenKind::KwElse => SyntaxKind::KwElse,
            TokenKind::KwFor => SyntaxKind::KwFor,
            TokenKind::KwIn => SyntaxKind::KwIn,
            TokenKind::KwReturn => SyntaxKind::KwReturn,
            TokenKind::KwAlloc => SyntaxKind::KwAlloc,
            TokenKind::KwTryAlloc => SyntaxKind::KwTryAlloc,
            TokenKind::KwCast => SyntaxKind::KwCast,
            TokenKind::KwPub => SyntaxKind::KwPub,
            TokenKind::KwImport => SyntaxKind::KwImport,
            TokenKind::KwExtern => SyntaxKind::KwExtern,
            TokenKind::KwCHeader => SyntaxKind::KwCHeader,
            TokenKind::KwBreak => SyntaxKind::KwBreak,
            TokenKind::KwContinue => SyntaxKind::KwContinue,
            TokenKind::KwComptime => SyntaxKind::KwComptime,
            TokenKind::KwTrue => SyntaxKind::KwTrue,
            TokenKind::KwFalse => SyntaxKind::KwFalse,
            TokenKind::ColonColonEq => SyntaxKind::ColonColonEq,
            TokenKind::ColonColon => SyntaxKind::ColonColon,
            TokenKind::ColonEq => SyntaxKind::ColonEq,
            TokenKind::Colon => SyntaxKind::Colon,
            TokenKind::Eq => SyntaxKind::Eq,
            TokenKind::PlusEq => SyntaxKind::PlusEq,
            TokenKind::MinusEq => SyntaxKind::MinusEq,
            TokenKind::StarEq => SyntaxKind::StarEq,
            TokenKind::SlashEq => SyntaxKind::SlashEq,
            TokenKind::PercentEq => SyntaxKind::PercentEq,
            TokenKind::AmpEq => SyntaxKind::AmpEq,
            TokenKind::PipeEq => SyntaxKind::PipeEq,
            TokenKind::CaretEq => SyntaxKind::CaretEq,
            TokenKind::ShlEq => SyntaxKind::ShlEq,
            TokenKind::ShrEq => SyntaxKind::ShrEq,
            TokenKind::Plus => SyntaxKind::Plus,
            TokenKind::Minus => SyntaxKind::Minus,
            TokenKind::Star => SyntaxKind::Star,
            TokenKind::Slash => SyntaxKind::Slash,
            TokenKind::Percent => SyntaxKind::Percent,
            TokenKind::Amp => SyntaxKind::Amp,
            TokenKind::Pipe => SyntaxKind::Pipe,
            TokenKind::Caret => SyntaxKind::Caret,
            TokenKind::Tilde => SyntaxKind::Tilde,
            TokenKind::Shl => SyntaxKind::Shl,
            TokenKind::Shr => SyntaxKind::Shr,
            TokenKind::EqEq => SyntaxKind::EqEq,
            TokenKind::BangEq => SyntaxKind::BangEq,
            TokenKind::Lt => SyntaxKind::Lt,
            TokenKind::LtEq => SyntaxKind::LtEq,
            TokenKind::Gt => SyntaxKind::Gt,
            TokenKind::GtEq => SyntaxKind::GtEq,
            TokenKind::AmpAmp => SyntaxKind::AmpAmp,
            TokenKind::PipePipe => SyntaxKind::PipePipe,
            TokenKind::Bang => SyntaxKind::Bang,
            TokenKind::At => SyntaxKind::At,
            TokenKind::Arrow => SyntaxKind::Arrow,
            TokenKind::DotDotDot => SyntaxKind::DotDotDot,
            TokenKind::Dot => SyntaxKind::Dot,
            TokenKind::Comma => SyntaxKind::Comma,
            TokenKind::Semi => SyntaxKind::Semi,
            TokenKind::LParen => SyntaxKind::LParen,
            TokenKind::RParen => SyntaxKind::RParen,
            TokenKind::LBrace => SyntaxKind::LBrace,
            TokenKind::RBrace => SyntaxKind::RBrace,
            TokenKind::LBracket => SyntaxKind::LBracket,
            TokenKind::RBracket => SyntaxKind::RBracket,
            TokenKind::Whitespace => SyntaxKind::Whitespace,
            TokenKind::LineComment => SyntaxKind::LineComment,
            TokenKind::BlockComment => SyntaxKind::BlockComment,
            TokenKind::Eof => SyntaxKind::Error, // Eof never becomes a tree leaf
            TokenKind::Error(_) => SyntaxKind::LexError,
        }
    }

    /// Trivia leaves: skipped by the typed AST view, retained in the tree.
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            SyntaxKind::Whitespace | SyntaxKind::LineComment | SyntaxKind::BlockComment
        )
    }
}

// ── green tree ───────────────────────────────────────────────────────────────

/// An immutable leaf: a token kind plus its source text.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GreenToken {
    pub kind: SyntaxKind,
    pub text: String,
}

impl GreenToken {
    pub fn new(kind: SyntaxKind, text: impl Into<String>) -> GreenToken {
        GreenToken {
            kind,
            text: text.into(),
        }
    }

    pub fn text_len(&self) -> u32 {
        self.text.len() as u32
    }
}

/// An immutable interior node: a kind plus an ordered list of child elements.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GreenNode {
    pub kind: SyntaxKind,
    pub children: Vec<GreenElement>,
    len: u32,
}

impl GreenNode {
    pub fn new(kind: SyntaxKind, children: Vec<GreenElement>) -> Rc<GreenNode> {
        let len = children.iter().map(GreenElement::text_len).sum();
        Rc::new(GreenNode {
            kind,
            children,
            len,
        })
    }

    pub fn text_len(&self) -> u32 {
        self.len
    }
}

/// A child of a green node: either a subtree or a leaf token.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum GreenElement {
    Node(Rc<GreenNode>),
    Token(GreenToken),
}

impl GreenElement {
    fn text_len(&self) -> u32 {
        match self {
            GreenElement::Node(n) => n.text_len(),
            GreenElement::Token(t) => t.text_len(),
        }
    }
}

// ── red tree ─────────────────────────────────────────────────────────────────

/// A parent-aware, absolutely-positioned view over a `GreenNode`. Cheap to
/// clone (it's `Rc`s and an offset); created lazily as you navigate.
#[derive(Clone)]
pub struct SyntaxNode {
    green: Rc<GreenNode>,
    /// Absolute byte offset of this node's start in the original source.
    offset: u32,
    parent: Option<Rc<SyntaxNode>>,
}

impl SyntaxNode {
    /// The root of a tree: offset 0, no parent.
    pub fn new_root(green: Rc<GreenNode>) -> SyntaxNode {
        SyntaxNode {
            green,
            offset: 0,
            parent: None,
        }
    }

    pub fn kind(&self) -> SyntaxKind {
        self.green.kind
    }

    pub fn span(&self) -> Span {
        Span::new(self.offset, self.offset + self.green.text_len())
    }

    pub fn parent(&self) -> Option<SyntaxNode> {
        self.parent.as_ref().map(|p| (**p).clone())
    }

    /// All child elements (nodes and tokens, trivia included) in order.
    pub fn children_with_tokens(&self) -> Vec<SyntaxElement> {
        let me = Rc::new(self.clone());
        let mut out = Vec::with_capacity(self.green.children.len());
        let mut cursor = self.offset;
        for child in &self.green.children {
            match child {
                GreenElement::Node(n) => {
                    out.push(SyntaxElement::Node(SyntaxNode {
                        green: n.clone(),
                        offset: cursor,
                        parent: Some(me.clone()),
                    }));
                    cursor += n.text_len();
                }
                GreenElement::Token(t) => {
                    out.push(SyntaxElement::Token(SyntaxToken {
                        green: t.clone(),
                        offset: cursor,
                        _parent: Some(me.clone()),
                    }));
                    cursor += t.text_len();
                }
            }
        }
        out
    }

    /// Only the child *nodes* (interior), skipping tokens.
    pub fn children(&self) -> Vec<SyntaxNode> {
        self.children_with_tokens()
            .into_iter()
            .filter_map(|e| match e {
                SyntaxElement::Node(n) => Some(n),
                SyntaxElement::Token(_) => None,
            })
            .collect()
    }

    /// The first child node of a given kind, if any (a red-tree accessor
    /// building block, arch §18's "find matching child").
    pub fn child_of_kind(&self, kind: SyntaxKind) -> Option<SyntaxNode> {
        self.children().into_iter().find(|n| n.kind() == kind)
    }

    /// The first child *token* of a given kind, skipping trivia matches.
    pub fn token_of_kind(&self, kind: SyntaxKind) -> Option<SyntaxToken> {
        self.children_with_tokens()
            .into_iter()
            .find_map(|e| match e {
                SyntaxElement::Token(t) if t.kind() == kind => Some(t),
                _ => None,
            })
    }

    /// Reconstruct the exact source text this node covers — the losslessness
    /// guarantee (arch §5). Walks every descendant leaf, trivia included.
    pub fn text(&self) -> String {
        let mut buf = String::new();
        collect_text(&self.green, &mut buf);
        buf
    }

    /// Every `Error`/`LexError` node or token anywhere in this subtree, as a flat
    /// list of (kind, span). Used by tests and diagnostics to find failures.
    pub fn errors(&self) -> Vec<Span> {
        let mut out = Vec::new();
        self.collect_errors(&mut out);
        out
    }

    fn collect_errors(&self, out: &mut Vec<Span>) {
        if self.kind() == SyntaxKind::Error {
            out.push(self.span());
        }
        for el in self.children_with_tokens() {
            match el {
                SyntaxElement::Node(n) => n.collect_errors(out),
                SyntaxElement::Token(t) if t.kind() == SyntaxKind::LexError => out.push(t.span()),
                SyntaxElement::Token(_) => {}
            }
        }
    }
}

fn collect_text(node: &GreenNode, buf: &mut String) {
    for child in &node.children {
        match child {
            GreenElement::Node(n) => collect_text(n, buf),
            GreenElement::Token(t) => buf.push_str(&t.text),
        }
    }
}

impl std::fmt::Debug for SyntaxNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}@{:?}", self.kind(), self.span())
    }
}

/// A positioned leaf view.
#[derive(Clone)]
pub struct SyntaxToken {
    green: GreenToken,
    offset: u32,
    _parent: Option<Rc<SyntaxNode>>,
}

impl SyntaxToken {
    pub fn kind(&self) -> SyntaxKind {
        self.green.kind
    }

    pub fn text(&self) -> &str {
        &self.green.text
    }

    pub fn span(&self) -> Span {
        Span::new(self.offset, self.offset + self.green.text_len())
    }
}

impl std::fmt::Debug for SyntaxToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}@{:?} {:?}", self.kind(), self.span(), self.text())
    }
}

/// A red-tree child: node or token.
#[derive(Clone, Debug)]
pub enum SyntaxElement {
    Node(SyntaxNode),
    Token(SyntaxToken),
}

/// Pretty-print a tree as an indented outline — invaluable in tests and for
/// eyeballing structure. Trivia tokens are shown too, so losslessness is visible.
pub fn debug_tree(node: &SyntaxNode) -> String {
    let mut buf = String::new();
    fmt_node(node, 0, &mut buf);
    buf
}

fn fmt_node(node: &SyntaxNode, depth: usize, buf: &mut String) {
    use std::fmt::Write;
    let _ = writeln!(
        buf,
        "{:indent$}{:?}@{:?}",
        "",
        node.kind(),
        node.span(),
        indent = depth * 2
    );
    for el in node.children_with_tokens() {
        match el {
            SyntaxElement::Node(n) => fmt_node(&n, depth + 1, buf),
            SyntaxElement::Token(t) => {
                let _ = writeln!(
                    buf,
                    "{:indent$}{:?}@{:?} {:?}",
                    "",
                    t.kind(),
                    t.span(),
                    t.text(),
                    indent = (depth + 1) * 2
                );
            }
        }
    }
}
