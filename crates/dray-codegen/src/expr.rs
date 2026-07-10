// SPDX-License-Identifier: Apache-2.0

//! Lowering Dray expression CST nodes to Tamago `Expr`.

use dray_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};
use tamago::{BinOp, Expr, UnaryOp};

use crate::{LowerError, Result, node_text, ty};

/// Lower any expression node to a Tamago `Expr`.
pub(crate) fn lower_expr(node: &SyntaxNode) -> Result<Expr> {
    match node.kind() {
        SyntaxKind::LiteralExpr => lower_literal(node),
        SyntaxKind::NameExpr => Ok(Expr::new_ident(node_text(node))),
        SyntaxKind::ParenExpr => {
            let inner = first_expr_child(node)?;
            Ok(Expr::new_parenthesized(lower_expr(&inner)?))
        }
        SyntaxKind::PrefixExpr => lower_prefix(node),
        SyntaxKind::BinaryExpr => lower_binary(node),
        SyntaxKind::CallExpr => lower_call(node),
        SyntaxKind::FieldExpr => lower_field(node),
        SyntaxKind::IndexExpr => lower_index(node),
        SyntaxKind::CastExpr => lower_cast(node),
        SyntaxKind::AllocExpr => Err(LowerError::new(
            "alloc/try_alloc implies RC allocation; deferred to the IR stage",
        )),
        other => Err(LowerError::new(format!(
            "unsupported expression node {other:?}"
        ))),
    }
}

/// A literal token wrapped in a `LiteralExpr`.
fn lower_literal(node: &SyntaxNode) -> Result<Expr> {
    let tok = node
        .children_with_tokens()
        .into_iter()
        .find_map(|e| match e {
            SyntaxElement::Token(t) if !t.kind().is_trivia() => Some(t),
            _ => None,
        })
        .ok_or_else(|| LowerError::new("empty literal"))?;

    let text = tok.text();
    match tok.kind() {
        SyntaxKind::IntLit => text
            .parse::<i64>()
            .map(Expr::Int)
            .map_err(|_| LowerError::new(format!("invalid integer literal `{text}`"))),
        SyntaxKind::FloatLit => text
            .parse::<f64>()
            .map(Expr::Double)
            .map_err(|_| LowerError::new(format!("invalid float literal `{text}`"))),
        SyntaxKind::StringLit => Ok(Expr::Str(unquote_string(text))),
        SyntaxKind::RuneLit => Ok(Expr::Char(unquote_rune(text)?)),
        SyntaxKind::KwTrue => Ok(Expr::Bool(true)),
        SyntaxKind::KwFalse => Ok(Expr::Bool(false)),
        other => Err(LowerError::new(format!(
            "unexpected literal token {other:?}"
        ))),
    }
}

fn lower_prefix(node: &SyntaxNode) -> Result<Expr> {
    let op_tok = leading_op_token(node)?;
    let operand = first_expr_child(node)?;
    let op = match op_tok.as_str() {
        "-" => UnaryOp::Neg,
        "!" => UnaryOp::LogicNeg,
        "~" => UnaryOp::BitNot,
        "&" => UnaryOp::AddrOf,
        "*" => UnaryOp::Deref,
        other => {
            return Err(LowerError::new(format!(
                "unknown prefix operator `{other}`"
            )));
        }
    };
    Ok(Expr::new_unary(lower_expr(&operand)?, op))
}

fn lower_binary(node: &SyntaxNode) -> Result<Expr> {
    let operands = expr_children(node);
    if operands.len() != 2 {
        return Err(LowerError::new("binary expression must have two operands"));
    }
    let op_tok = middle_op_token(node)?;
    let op = binop(&op_tok)?;
    Ok(Expr::new_binary(
        lower_expr(&operands[0])?,
        op,
        lower_expr(&operands[1])?,
    ))
}

fn lower_call(node: &SyntaxNode) -> Result<Expr> {
    // callee is the first expression child; the ArgList is a separate child node
    let callee = first_expr_child(node)?;
    let arglist = node
        .child_of_kind(SyntaxKind::ArgList)
        .ok_or_else(|| LowerError::new("call without an argument list"))?;
    let args = expr_children(&arglist)
        .iter()
        .map(lower_expr)
        .collect::<Result<Vec<_>>>()?;
    Ok(Expr::new_fn_call(lower_expr(&callee)?, args))
}

fn lower_field(node: &SyntaxNode) -> Result<Expr> {
    let recv = first_expr_child(node)?;
    let member = node
        .token_of_kind(SyntaxKind::Ident)
        .map(|t| t.text().to_string())
        .ok_or_else(|| LowerError::new("field access without a member name"))?;
    Ok(Expr::new_mem_access(lower_expr(&recv)?, member))
}

fn lower_index(node: &SyntaxNode) -> Result<Expr> {
    let parts = expr_children(node);
    if parts.len() != 2 {
        return Err(LowerError::new(
            "index expression must have array and index",
        ));
    }
    Ok(Expr::new_arr_index(
        lower_expr(&parts[0])?,
        lower_expr(&parts[1])?,
    ))
}

fn lower_cast(node: &SyntaxNode) -> Result<Expr> {
    // `cast ( Type ) UnaryExpr`
    let type_node = node
        .children()
        .into_iter()
        .find(|c| is_type_node(c.kind()))
        .ok_or_else(|| LowerError::new("cast without a target type"))?;
    let operand = node
        .children()
        .into_iter()
        .find(|c| !is_type_node(c.kind()))
        .ok_or_else(|| LowerError::new("cast without an operand"))?;
    Ok(Expr::new_cast(
        ty::lower_type(&type_node)?,
        lower_expr(&operand)?,
    ))
}

// ── operator tables ──────────────────────────────────────────────────────────

fn binop(glyph: &str) -> Result<BinOp> {
    Ok(match glyph {
        "+" => BinOp::Add,
        "-" => BinOp::Sub,
        "*" => BinOp::Mul,
        "/" => BinOp::Div,
        "%" => BinOp::Mod,
        "==" => BinOp::Eq,
        "!=" => BinOp::NEq,
        ">" => BinOp::GT,
        "<" => BinOp::LT,
        ">=" => BinOp::GTE,
        "<=" => BinOp::LTE,
        "&&" => BinOp::And,
        "||" => BinOp::Or,
        "&" => BinOp::BitAnd,
        "|" => BinOp::BitOr,
        "^" => BinOp::XOr,
        "<<" => BinOp::LShift,
        ">>" => BinOp::RShift,
        other => {
            return Err(LowerError::new(format!(
                "unknown binary operator `{other}`"
            )));
        }
    })
}

// ── child/token helpers ──────────────────────────────────────────────────────

/// All child *nodes* that are expressions.
pub(crate) fn expr_children(node: &SyntaxNode) -> Vec<SyntaxNode> {
    node.children()
        .into_iter()
        .filter(|c| is_expr_node(c.kind()))
        .collect()
}

fn first_expr_child(node: &SyntaxNode) -> Result<SyntaxNode> {
    expr_children(node)
        .into_iter()
        .next()
        .ok_or_else(|| LowerError::new("expected a subexpression"))
}

/// The operator token at the front of a prefix expression.
fn leading_op_token(node: &SyntaxNode) -> Result<String> {
    node.children_with_tokens()
        .into_iter()
        .find_map(|e| match e {
            SyntaxElement::Token(t) if !t.kind().is_trivia() => Some(t.text().to_string()),
            _ => None,
        })
        .ok_or_else(|| LowerError::new("prefix expression without an operator"))
}

/// The operator token sitting between the two operands of a binary expression.
fn middle_op_token(node: &SyntaxNode) -> Result<String> {
    for el in node.children_with_tokens() {
        if let SyntaxElement::Token(t) = el {
            if !t.kind().is_trivia() {
                return Ok(t.text().to_string());
            }
        }
    }
    Err(LowerError::new("binary expression without an operator"))
}

fn is_expr_node(kind: SyntaxKind) -> bool {
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

// ── literal text decoding ────────────────────────────────────────────────────

/// Strip the surrounding double quotes of a string literal. Escape sequences are
/// passed through verbatim: Tamago re-escapes on render, and Dray's escape set is
/// a subset of C's, so the bytes are already valid. (A fuller implementation
/// would decode then let Tamago re-encode; fine for the skeleton.)
fn unquote_string(text: &str) -> String {
    let trimmed = text
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(text);
    trimmed.to_string()
}

/// Decode a rune literal `'…'` to a single `char`, handling the common escapes.
fn unquote_rune(text: &str) -> Result<char> {
    let inner = text
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .ok_or_else(|| LowerError::new(format!("malformed rune literal `{text}`")))?;

    let mut chars = inner.chars();
    let first = chars
        .next()
        .ok_or_else(|| LowerError::new("empty rune literal"))?;
    if first != '\\' {
        return Ok(first);
    }
    // an escape sequence
    let esc = chars
        .next()
        .ok_or_else(|| LowerError::new("dangling escape in rune literal"))?;
    Ok(match esc {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        '0' => '\0',
        '\\' => '\\',
        '\'' => '\'',
        '"' => '"',
        other => {
            return Err(LowerError::new(format!(
                "rune escape `\\{other}` not lowered by the skeleton"
            )));
        }
    })
}
