// SPDX-License-Identifier: Apache-2.0

//! Top-level lowering: a Dray `SourceFile` CST → a Tamago `Scope`

use dray_syntax::{SyntaxKind, SyntaxNode};
use tamago::{
    FunctionBuilder, GlobalStatement, IncludeBuilder, ParameterBuilder, Scope, ScopeBuilder, Type,
};

use crate::stmt::lower_block;
use crate::{LowerError, Result, first_ident, ty};

/// Lower the whole `SourceFile` to a C translation unit.
pub fn lower_source_file(root: &SyntaxNode) -> Result<Scope> {
    if root.kind() != SyntaxKind::SourceFile {
        return Err(LowerError::new("expected a SourceFile root"));
    }

    let mut scope = ScopeBuilder::new();

    scope = scope.global_statement(GlobalStatement::Include(
        IncludeBuilder::new_system_with_str("stdint.h").build(),
    ));

    let mut first = true;

    for decl in root.children() {
        if !first {
            scope = scope.new_line();
        }
        first = false;

        match decl.kind() {
            SyntaxKind::CHeaderDecl => {
                scope = scope.global_statement(lower_c_header(&decl)?);
            }
            SyntaxKind::ExternProcDecl => {
                scope =
                    scope.global_statement(GlobalStatement::Function(lower_extern_proc(&decl)?));
            }
            SyntaxKind::ProcDef => {
                scope = scope.global_statement(GlobalStatement::Function(lower_proc_def(&decl)?));
            }
            SyntaxKind::Error => {
                return Err(LowerError::new(
                    "source contains an Error node; fix parse errors before codegen",
                ));
            }
            other => {
                return Err(LowerError::new(format!(
                    "top-level {other:?} is not lowered by the skeleton yet"
                )));
            }
        }
    }

    Ok(scope.build())
}

/// `c_header("stdio.h");` → `#include <stdio.h>`.
fn lower_c_header(node: &SyntaxNode) -> Result<GlobalStatement> {
    let path = node
        .token_of_kind(SyntaxKind::StringLit)
        .map(|t| unquote(t.text()))
        .ok_or_else(|| LowerError::new("c_header without a header string"))?;
    Ok(GlobalStatement::Include(
        IncludeBuilder::new_system_with_str(&path).build(),
    ))
}

/// `name :: extern "sym" proc(params) -> T;` → a C prototype.
fn lower_extern_proc(node: &SyntaxNode) -> Result<tamago::Function> {
    let name = first_ident(node).ok_or_else(|| LowerError::new("extern without a name"))?;
    let ret = return_type(node)?;
    let mut fb = FunctionBuilder::new_with_str(&name, ret).make_extern();
    for param in params(node)? {
        fb = fb.param(param);
    }
    // No .body() → renders as a prototype/declaration.
    Ok(fb.build())
}

/// `name :: proc(params) -> T { body }` → a C function definition.
fn lower_proc_def(node: &SyntaxNode) -> Result<tamago::Function> {
    let name = first_ident(node).ok_or_else(|| LowerError::new("proc without a name"))?;
    let ret = return_type(node)?;
    let mut fb = FunctionBuilder::new_with_str(&name, ret);
    for param in params(node)? {
        fb = fb.param(param);
    }
    let body = node
        .child_of_kind(SyntaxKind::Block)
        .ok_or_else(|| LowerError::new("proc without a body block"))?;
    fb = fb.body(lower_block(&body)?);
    Ok(fb.build())
}

/// The return type: the `-> T` clause if present, else `void`.
fn return_type(node: &SyntaxNode) -> Result<Type> {
    match node.child_of_kind(SyntaxKind::RetType) {
        Some(rt) => {
            let t = rt
                .children()
                .into_iter()
                .find(|c| is_type_node(c.kind()))
                .ok_or_else(|| LowerError::new("-> with no return type"))?;
            ty::lower_type(&t)
        }
        None => Ok(Type::base(tamago::BaseType::Void)),
    }
}

/// The parameters from the `ParamList` child.
fn params(node: &SyntaxNode) -> Result<Vec<tamago::Parameter>> {
    let list = match node.child_of_kind(SyntaxKind::ParamList) {
        Some(l) => l,
        None => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for p in list.children() {
        if p.kind() != SyntaxKind::Param {
            continue;
        }
        // `comptime` params can't cross into C. reject them for now
        if p.token_of_kind(SyntaxKind::KwComptime).is_some() {
            return Err(LowerError::new(
                "comptime parameters need monomorphization (HIR); not lowered by the skeleton",
            ));
        }
        let pname = first_ident(&p).ok_or_else(|| LowerError::new("parameter without a name"))?;
        let ptype = p
            .children()
            .into_iter()
            .find(|c| is_type_node(c.kind()))
            .ok_or_else(|| LowerError::new(format!("parameter `{pname}` has no type")))?;
        out.push(ParameterBuilder::new_with_str(&pname, ty::lower_type(&ptype)?).build());
    }
    Ok(out)
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

/// Strip the surrounding double quotes from a string-literal token's text.
fn unquote(text: &str) -> String {
    text.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(text)
        .to_string()
}
