// SPDX-License-Identifier: Apache-2.0

//! Type representation helpers and best-effort inference (for now).

use dray_syntax::{SyntaxKind, SyntaxNode};

use crate::hir::{IntWidth, Ty};

pub(crate) fn lower_type(node: &SyntaxNode) -> Option<Ty> {
    match node.kind() {
        SyntaxKind::NameType => {
            let name = node
                .token_of_kind(SyntaxKind::Ident)
                .map(|t| t.text().to_string())
                .unwrap_or_else(|| node.text().trim().to_string());
            Some(name_to_ty(&name))
        }
        SyntaxKind::PointerType => {
            let inner = node.children().into_iter().find(|c| is_type(c.kind()))?;
            Some(Ty::Ptr(Box::new(lower_type(&inner)?)))
        }
        SyntaxKind::RcPointerType => {
            let inner = node.children().into_iter().find(|c| is_type(c.kind()))?;
            Some(Ty::Rc(Box::new(lower_type(&inner)?)))
        }
        SyntaxKind::GenericType => {
            let name = node
                .token_of_kind(SyntaxKind::Ident)
                .map(|t| t.text().to_string())?;
            let args = node
                .child_of_kind(SyntaxKind::TypeArgList)
                .map(|al| {
                    al.children()
                        .into_iter()
                        .filter(|c| is_type(c.kind()))
                        .filter_map(|t| lower_type(&t))
                        .collect()
                })
                .unwrap_or_default();
            Some(Ty::App(name, args))
        }
        SyntaxKind::SliceType => {
            let elem = node.children().into_iter().find(|c| is_type(c.kind()))?;
            Some(Ty::Slice(Box::new(lower_type(&elem)?)))
        }
        SyntaxKind::ArrayType => {
            let elem = node.children().into_iter().find(|c| is_type(c.kind()))?;
            let len = array_length(node)?;
            Some(Ty::Array(Box::new(lower_type(&elem)?), len))
        }
        _ => None,
    }
}

/// The literal length written in an `[N]T` type
fn array_length(node: &SyntaxNode) -> Option<u64> {
    let size = node
        .children()
        .into_iter()
        .find(|c| c.kind() == SyntaxKind::LiteralExpr)?;
    let text = size.token_of_kind(SyntaxKind::IntLit)?.text().to_string();
    text.replace('_', "").parse().ok()
}

pub(crate) fn is_type(kind: SyntaxKind) -> bool {
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

pub(crate) fn name_to_ty(name: &str) -> Ty {
    match name {
        "void" => Ty::Void,
        "bool" => Ty::Bool,
        "cchar" => Ty::CChar,
        "int8" => int(IntWidth::W8, true),
        "int16" => int(IntWidth::W16, true),
        "int32" => int(IntWidth::W32, true),
        "int64" | "int" => int(IntWidth::W64, true),
        "uint8" => int(IntWidth::W8, false),
        "uint16" => int(IntWidth::W16, false),
        "uint32" => int(IntWidth::W32, false),
        "uint64" | "uint" => int(IntWidth::W64, false),
        "usize" | "size" => int(IntWidth::Size, false),
        "float32" => Ty::Float { bits: 32 },
        "float64" | "float" => Ty::Float { bits: 64 },
        other => Ty::Named(other.to_string()),
    }
}

fn int(bits: IntWidth, signed: bool) -> Ty {
    Ty::Int { bits, signed }
}
