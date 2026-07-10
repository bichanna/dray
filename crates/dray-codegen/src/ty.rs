// SPDX-License-Identifier: Apache-2.0

//! Lowering Dray type CST nodes to Tamago `Type`.

use dray_syntax::{SyntaxKind, SyntaxNode};
use tamago::{BaseType, Type};

use crate::{LowerError, Result, node_text, require_child};

/// Lower a Dray type node (`NameType`, `PointerType`, `RcPointerType`) to a C type.
pub(crate) fn lower_type(node: &SyntaxNode) -> Result<Type> {
    match node.kind() {
        SyntaxKind::NameType => Ok(lower_name_type(node)),
        SyntaxKind::PointerType => {
            let inner = require_child(node, SyntaxKind::NameType, "pointee type")
                .or_else(|_| pointee(node))?;
            Ok(Type::ptr(lower_type(&inner)?))
        }
        SyntaxKind::RcPointerType => {
            let inner = pointee(node)?;
            Ok(Type::ptr(lower_type(&inner)?))
        }
        SyntaxKind::SliceType => Err(LowerError::new(
            "slice types ([]T) need a runtime representation; deferred to a later stage",
        )),
        SyntaxKind::ArrayType => Err(LowerError::new(
            "array types ([N]T) are not lowered by the skeleton yet",
        )),
        SyntaxKind::GenericType => Err(LowerError::new(
            "generic types need monomorphization (HIR stage); not lowered by the skeleton",
        )),
        other => Err(LowerError::new(format!("unexpected type node {other:?}"))),
    }
}

fn pointee(node: &SyntaxNode) -> Result<SyntaxNode> {
    node.children()
        .into_iter()
        .find(|c| is_type_node(c.kind()))
        .ok_or_else(|| LowerError::new("pointer type without a pointee"))
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

/// Map a Dray primitive name to a C `BaseType`. Unknown names become an opaque
/// typedef reference of the same name (so user structs still name-check as C
/// once they exist), which is the best a name-resolution-free skeleton can do.
fn lower_name_type(node: &SyntaxNode) -> Type {
    let name = node_text(node);
    let base = match name.as_str() {
        "void" => BaseType::Void,
        "bool" => BaseType::Bool,
        "int8" => BaseType::Int8,
        "int16" => BaseType::Int16,
        "int32" => BaseType::Int32,
        "int64" | "int" => BaseType::Int64,
        "uint8" => BaseType::UInt8,
        "uint16" => BaseType::UInt16,
        "uint32" => BaseType::UInt32,
        "uint64" | "uint" => BaseType::UInt64,
        "float32" => BaseType::Float,
        "float64" | "float" => BaseType::Double,
        "usize" | "size" => BaseType::Size,
        _ => BaseType::TypeDef(name),
    };
    Type::base(base)
}
