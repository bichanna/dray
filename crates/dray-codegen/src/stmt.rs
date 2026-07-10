// SPDX-License-Identifier: Apache-2.0

//! Lowering Dray statement CST nodes to Tamago `Statement` / `Block`.

use dray_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};
use tamago::{
    AssignOp, Block, BlockBuilder, Expr, ForBuilder, ForInit, IfBuilder, Statement, Type,
    VariableBuilder, WhileBuilder,
};

use crate::expr::{expr_children, lower_expr};
use crate::{LowerError, Result, first_ident, ty};

/// Lower a `Block` node (`{ Statement* }`) to a Tamago `Block`.
pub(crate) fn lower_block(node: &SyntaxNode) -> Result<Block> {
    let mut b = BlockBuilder::new();
    for stmt in node.children() {
        if is_stmt_node(stmt.kind()) {
            b = b.statement(lower_stmt(&stmt)?);
        }
    }
    Ok(b.build())
}

/// Lower one statement node to a Tamago `Statement`.
pub(crate) fn lower_stmt(node: &SyntaxNode) -> Result<Statement> {
    match node.kind() {
        SyntaxKind::ReturnStmt => lower_return(node),
        SyntaxKind::BreakStmt => Ok(Statement::Break),
        SyntaxKind::ContinueStmt => Ok(Statement::Continue),
        SyntaxKind::ExprStmt => {
            let e = expr_children(node)
                .into_iter()
                .next()
                .ok_or_else(|| LowerError::new("empty expression statement"))?;
            Ok(Statement::Expr(lower_expr(&e)?))
        }
        SyntaxKind::VarDecl => Ok(Statement::Variable(lower_var_decl(node)?)),
        SyntaxKind::AssignStmt => Ok(Statement::Expr(lower_assign(node)?)),
        SyntaxKind::IfStmt => Ok(Statement::If(lower_if(node)?)),
        SyntaxKind::ForStmt => lower_for(node),
        SyntaxKind::Block => Err(LowerError::new(
            "nested bare blocks are not lowered by the skeleton yet",
        )),
        other => Err(LowerError::new(format!("unsupported statement {other:?}"))),
    }
}

fn lower_return(node: &SyntaxNode) -> Result<Statement> {
    match expr_children(node).into_iter().next() {
        Some(e) => Ok(Statement::Return(Some(lower_expr(&e)?))),
        None => Ok(Statement::Return(None)),
    }
}

/// Lower a var decl. Uses the explicit type if present; otherwise `int` (see
/// the skeleton note — real type inference is an HIR concern).
fn lower_var_decl(node: &SyntaxNode) -> Result<tamago::Variable> {
    let name = first_ident(node).ok_or_else(|| LowerError::new("var decl without a name"))?;

    let declared_type = node.children().into_iter().find(|c| is_type_node(c.kind()));
    let c_type: Type = match declared_type {
        Some(t) => ty::lower_type(&t)?,
        None => Type::base(tamago::BaseType::Int),
    };

    let init = expr_children(node)
        .into_iter()
        .next()
        .ok_or_else(|| LowerError::new("var decl without an initializer"))?;

    Ok(VariableBuilder::new_with_str(&name, c_type)
        .value(lower_expr(&init)?)
        .build())
}

/// Lower `lhs <op> rhs` to a Tamago assignment expression.
fn lower_assign(node: &SyntaxNode) -> Result<Expr> {
    let parts = expr_children(node);
    if parts.len() != 2 {
        return Err(LowerError::new("assignment needs a target and a value"));
    }
    let op_glyph = assign_op_token(node)?;
    let op = assign_op(&op_glyph)?;
    Ok(Expr::new_assign(
        lower_expr(&parts[0])?,
        op,
        lower_expr(&parts[1])?,
    ))
}

fn lower_if(node: &SyntaxNode) -> Result<tamago::If> {
    if node.child_of_kind(SyntaxKind::VarDecl).is_some()
        || node.child_of_kind(SyntaxKind::AssignStmt).is_some()
    {
        return Err(LowerError::new(
            "if-init clauses are not lowered by the skeleton yet",
        ));
    }

    let cond_node =
        require_condition(node).ok_or_else(|| LowerError::new("if without a condition"))?;
    let cond = lower_expr(&cond_node)?;

    let then_block = node
        .child_of_kind(SyntaxKind::Block)
        .ok_or_else(|| LowerError::new("if without a then-block"))?;
    let mut builder = IfBuilder::new(cond).then(lower_block(&then_block)?);

    // else / else-if
    if let Some(else_clause) = node.child_of_kind(SyntaxKind::ElseClause) {
        if let Some(inner_if) = else_clause.child_of_kind(SyntaxKind::IfStmt) {
            // else-if: wrap the nested If as the sole statement of the else block
            let nested = lower_if(&inner_if)?;
            let else_block = BlockBuilder::new().statement(Statement::If(nested)).build();
            builder = builder.other(else_block);
        } else if let Some(else_block) = else_clause.child_of_kind(SyntaxKind::Block) {
            builder = builder.other(lower_block(&else_block)?);
        }
    }
    Ok(builder.build())
}

/// Lower a `for` in whichever of the four Dray forms it is.
fn lower_for(node: &SyntaxNode) -> Result<Statement> {
    let has_condition = node.child_of_kind(SyntaxKind::Condition).is_some();
    let has_semis = for_has_semicolons(node);
    let is_range = node.token_of_kind(SyntaxKind::KwIn).is_some();

    let body_node = node
        .child_of_kind(SyntaxKind::Block)
        .ok_or_else(|| LowerError::new("for without a body"))?;
    let body = lower_block(&body_node)?;

    if is_range {
        return Err(LowerError::new(
            "for-in range loops need the iterable's type; deferred to a later stage",
        ));
    }

    if has_semis {
        // C-style: for [init] ; [cond] ; [post] { }
        let mut fb = ForBuilder::new();
        // init: the first VarDecl or AssignStmt child (may be absent)
        if let Some(init) = node.child_of_kind(SyntaxKind::VarDecl) {
            fb = fb.init(ForInit::Decl(lower_var_decl(&init)?));
        } else if let Some(init) = node.child_of_kind(SyntaxKind::AssignStmt) {
            fb = fb.init(ForInit::Expr(lower_assign(&init)?));
        }
        // cond: the Condition wrapper (may be absent -> C allows empty)
        if let Some(cond) = node.child_of_kind(SyntaxKind::Condition) {
            let e = expr_children(&cond)
                .into_iter()
                .next()
                .ok_or_else(|| LowerError::new("empty for-condition"))?;
            fb = fb.cond(lower_expr(&e)?);
        }
        // post: an AssignStmt/ExprStmt after the condition. We pick the last
        // assignment/expr child that isn't the init.
        if let Some(post) = for_post_statement(node) {
            fb = fb.step(post?);
        }
        Ok(Statement::For(fb.body(body).build()))
    } else if has_condition {
        // while-style: for cond { }  ->  while (cond) { }
        let cond = node.child_of_kind(SyntaxKind::Condition).unwrap();
        let e = expr_children(&cond)
            .into_iter()
            .next()
            .ok_or_else(|| LowerError::new("empty while-condition"))?;
        Ok(Statement::While(
            WhileBuilder::new(lower_expr(&e)?).body(body).build(),
        ))
    } else {
        // infinite: for { }  ->  for (;;) { }
        Ok(Statement::For(ForBuilder::new().body(body).build()))
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// The condition expression of an if/while (the expr inside the `Condition`
/// grouping node).
fn require_condition(node: &SyntaxNode) -> Option<SyntaxNode> {
    let cond = node.child_of_kind(SyntaxKind::Condition)?;
    expr_children(&cond).into_iter().next()
}

/// Does this `for` header contain top-level `;` tokens (i.e. C-style)?
fn for_has_semicolons(node: &SyntaxNode) -> bool {
    node.children_with_tokens().iter().any(|e| match e {
        SyntaxElement::Token(t) => t.kind() == SyntaxKind::Semi,
        _ => false,
    })
}

/// The post-statement of a C-style for: the assignment/expression that follows
/// the condition. Returns the lowered `Expr`, or `None` if there's no post.
fn for_post_statement(node: &SyntaxNode) -> Option<Result<Expr>> {
    let stmts: Vec<SyntaxNode> = node
        .children()
        .into_iter()
        .filter(|c| matches!(c.kind(), SyntaxKind::AssignStmt | SyntaxKind::ExprStmt))
        .collect();
    let has_decl_init = node.child_of_kind(SyntaxKind::VarDecl).is_some();
    let post = if has_decl_init {
        stmts.into_iter().next_back()
    } else if stmts.len() >= 2 {
        stmts.into_iter().next_back()
    } else {
        None
    };
    post.map(|p| match p.kind() {
        SyntaxKind::AssignStmt => lower_assign(&p),
        SyntaxKind::ExprStmt => {
            let e = expr_children(&p)
                .into_iter()
                .next()
                .ok_or_else(|| LowerError::new("empty for-post"))?;
            lower_expr(&e)
        }
        _ => unreachable!(),
    })
}

fn assign_op_token(node: &SyntaxNode) -> Result<String> {
    for el in node.children_with_tokens() {
        if let SyntaxElement::Token(t) = el {
            if is_assign_glyph(t.kind()) {
                return Ok(t.text().to_string());
            }
        }
    }
    Err(LowerError::new("assignment without an operator"))
}

fn is_assign_glyph(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Eq
            | SyntaxKind::PlusEq
            | SyntaxKind::MinusEq
            | SyntaxKind::StarEq
            | SyntaxKind::SlashEq
            | SyntaxKind::PercentEq
            | SyntaxKind::AmpEq
            | SyntaxKind::PipeEq
            | SyntaxKind::CaretEq
            | SyntaxKind::ShlEq
            | SyntaxKind::ShrEq
    )
}

fn assign_op(glyph: &str) -> Result<AssignOp> {
    Ok(match glyph {
        "=" => AssignOp::Assign,
        "+=" => AssignOp::AddAssign,
        "-=" => AssignOp::SubAssign,
        "*=" => AssignOp::MulAssign,
        "/=" => AssignOp::DivAssign,
        "%=" => AssignOp::ModAssign,
        "&=" => AssignOp::BitAndAssign,
        "|=" => AssignOp::BitOrAssign,
        "^=" => AssignOp::BitXOrAssign,
        "<<=" => AssignOp::LShiftAssign,
        ">>=" => AssignOp::RShiftAssign,
        other => {
            return Err(LowerError::new(format!(
                "unknown assign operator `{other}`"
            )));
        }
    })
}

fn is_stmt_node(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::ReturnStmt
            | SyntaxKind::BreakStmt
            | SyntaxKind::ContinueStmt
            | SyntaxKind::ExprStmt
            | SyntaxKind::VarDecl
            | SyntaxKind::AssignStmt
            | SyntaxKind::IfStmt
            | SyntaxKind::ForStmt
            | SyntaxKind::Block
    )
}

fn is_type_node(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::NameType
            | SyntaxKind::PointerType
            | SyntaxKind::RcPointerType
            | SyntaxKind::SliceType
            | SyntaxKind::ArrayType
            | SyntaxKind::GenericType
    )
}
