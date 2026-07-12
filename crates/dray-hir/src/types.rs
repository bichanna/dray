// SPDX-License-Identifier: Apache-2.0

//! Type representation helpers and best-effort inference (for now).

use dray_syntax::{SyntaxKind, SyntaxNode};

use crate::hir::{IntWidth, Ty};

pub(crate) fn lower_type(node: &SyntaxNode) -> Option<Ty> {
    match node.kind() {
        SyntaxKind::NameType => Some(name_to_ty(node.text().trim())),
        SyntaxKind::PointerType | SyntaxKind::RcPointerType => {
            let inner = node.children().into_iter().find(|c| is_type(c.kind()))?;
            Some(Ty::Ptr(Box::new(lower_type(&inner)?)))
        }
        // Not modeled yet.
        SyntaxKind::SliceType | SyntaxKind::ArrayType | SyntaxKind::GenericType => None,
        _ => None,
    }
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

fn name_to_ty(name: &str) -> Ty {
    match name {
        "void" => Ty::Void,
        "bool" => Ty::Bool,
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
