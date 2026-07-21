// SPDX-License-Identifier: Apache-2.0

//! Monomorphization

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::hir::*;

const INSTANTIATION_DEPTH_LIMIT: usize = 128;

/// Monomorphization failed. Currently only the depth-limit case, which almost
/// always means an unintentionally infinite generic type
#[derive(Debug, Clone)]
pub struct MonoError {
    pub message: String,
}

impl fmt::Display for MonoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

fn app_depth(ty: &Ty) -> usize {
    match ty {
        Ty::Ptr(inner) | Ty::Rc(inner) => app_depth(inner),
        Ty::App(_, args) => 1 + args.iter().map(app_depth).max().unwrap_or(0),
        _ => 0,
    }
}

pub fn monomorphize(mut hir: Hir) -> Result<Hir, MonoError> {
    // Split the generic templates out from the code that is kept as-is.
    let mut templates: HashMap<String, StructDef> = HashMap::new();
    let mut enum_templates: HashMap<String, EnumDef> = HashMap::new();
    let mut proc_templates: HashMap<String, Proc> = HashMap::new();
    let mut kept: Vec<Item> = Vec::with_capacity(hir.items.len());
    for item in hir.items {
        match item {
            Item::Struct(sd) if !sd.type_params.is_empty() => {
                templates.insert(sd.name.clone(), sd);
            }
            Item::Enum(ed) if !ed.type_params.is_empty() => {
                enum_templates.insert(ed.name.clone(), ed);
            }
            Item::Proc(p) if !p.type_params.is_empty() => {
                proc_templates.insert(p.name.clone(), p);
            }
            other => kept.push(other),
        }
    }

    let mut mono = Mono {
        templates,
        enum_templates,
        proc_templates,
        generated: HashMap::new(),
        generated_enums: HashMap::new(),
        generated_procs: HashMap::new(),
        done: HashSet::new(),
    };

    let mut queue: Vec<(String, Vec<Ty>)> = Vec::new();
    let mut proc_queue: Vec<(String, Vec<Ty>)> = Vec::new();
    for item in &mut kept {
        each_structural_ty(item, &mut |ty| collect_apps(ty, &mut queue));
        each_expr_in_item(item, &mut |e| {
            if let ExprKind::GenericCall {
                proc_name,
                type_args,
                ..
            } = &e.kind
            {
                proc_queue.push((proc_name.clone(), type_args.clone()));
            }
        });
    }

    while !queue.is_empty() || !proc_queue.is_empty() {
        while let Some((name, args)) = queue.pop() {
            if 1 + args.iter().map(app_depth).max().unwrap_or(0) > INSTANTIATION_DEPTH_LIMIT {
                return Err(MonoError {
                    message: format!(
                        "generic instantiation of `{name}` nests deeper than the limit of \
                         {INSTANTIATION_DEPTH_LIMIT} (a generic type is likely infinitely recursive)"
                    ),
                });
            }
            mono.instantiate(&name, &args, &mut queue);
        }
        while let Some((name, args)) = proc_queue.pop() {
            if 1 + args.iter().map(app_depth).max().unwrap_or(0) > INSTANTIATION_DEPTH_LIMIT {
                return Err(MonoError {
                    message: format!(
                        "generic instantiation of proc `{name}` nests deeper than the limit of \
                         {INSTANTIATION_DEPTH_LIMIT} (a generic proc is likely infinitely recursive)"
                    ),
                });
            }
            mono.instantiate_proc(&name, &args, &mut queue, &mut proc_queue);
        }
    }

    let mut generated_procs: Vec<Proc> = mono.generated_procs.into_values().collect();
    generated_procs.sort_by(|a, b| a.name.cmp(&b.name));
    kept.extend(generated_procs.into_iter().map(Item::Proc));

    for item in &mut kept {
        each_structural_ty(item, &mut rewrite_ty);
        each_expr_in_item(item, &mut rewrite_generic_call);
    }

    let mut generated: Vec<StructDef> = mono.generated.into_values().collect();
    generated.sort_by(|a, b| a.name.cmp(&b.name));
    for mut sd in generated {
        for field in &mut sd.fields {
            rewrite_ty(&mut field.ty);
        }
        kept.push(Item::Struct(sd));
    }

    let mut generated_enums: Vec<EnumDef> = mono.generated_enums.into_values().collect();
    generated_enums.sort_by(|a, b| a.name.cmp(&b.name));
    for mut ed in generated_enums {
        for v in &mut ed.variants {
            for ty in &mut v.payload {
                rewrite_ty(ty);
            }
        }
        kept.push(Item::Enum(ed));
    }

    hir.items = kept;
    Ok(hir)
}

struct Mono {
    templates: HashMap<String, StructDef>,
    enum_templates: HashMap<String, EnumDef>,
    proc_templates: HashMap<String, Proc>,
    /// Mangled name → concrete struct.
    generated: HashMap<String, StructDef>,
    /// Mangled name → concrete enum.
    generated_enums: HashMap<String, EnumDef>,
    /// Mangled name → concrete proc.
    generated_procs: HashMap<String, Proc>,
    /// Mangled names already generated, to avoid repeating work.
    done: HashSet<String>,
}

impl Mono {
    fn instantiate_proc(
        &mut self,
        name: &str,
        args: &[Ty],
        types: &mut Vec<(String, Vec<Ty>)>,
        procs: &mut Vec<(String, Vec<Ty>)>,
    ) {
        let mangled = mangle(name, args);
        if self.done.contains(&mangled) {
            return;
        }
        let Some(template) = self.proc_templates.get(name) else {
            return; // not a generic proc
        };
        self.done.insert(mangled.clone());

        let subst = param_map(&template.type_params, args);
        let concrete = Proc {
            def: template.def,
            name: mangled.clone(),
            type_params: Vec::new(),
            params: template
                .params
                .iter()
                .map(|p| Param {
                    def: p.def,
                    name: p.name.clone(),
                    ty: subst_ty(&p.ty, &subst),
                })
                .collect(),
            ret: subst_ty(&template.ret, &subst),
            body: template.body.clone(),
        };

        // Substitute the type parameters everywhere in the cloned body.
        let mut item = Item::Proc(concrete);
        each_structural_ty(&mut item, &mut |ty| *ty = subst_ty(ty, &subst));
        // The instantiated body may name generic types or call generic procs
        each_structural_ty(&mut item, &mut |ty| collect_apps(ty, types));
        each_expr_in_item(&mut item, &mut |e| {
            if let ExprKind::GenericCall {
                proc_name,
                type_args,
                ..
            } = &e.kind
            {
                procs.push((proc_name.clone(), type_args.clone()));
            }
        });

        if let Item::Proc(concrete) = item {
            self.generated_procs.insert(mangled, concrete);
        }
    }

    fn instantiate(&mut self, name: &str, args: &[Ty], queue: &mut Vec<(String, Vec<Ty>)>) {
        let mangled = mangle(name, args);
        if self.done.contains(&mangled) {
            return;
        }

        if let Some(template) = self.templates.get(name) {
            self.done.insert(mangled.clone());
            let subst = param_map(&template.type_params, args);
            let fields: Vec<Field> = template
                .fields
                .iter()
                .map(|f| Field {
                    name: f.name.clone(),
                    ty: subst_ty(&f.ty, &subst),
                })
                .collect();

            for f in &fields {
                collect_apps(&f.ty, queue);
            }

            self.generated.insert(
                mangled.clone(),
                StructDef {
                    def: template.def,
                    name: mangled,
                    type_params: Vec::new(),
                    fields,
                },
            );
        } else if let Some(template) = self.enum_templates.get(name) {
            self.done.insert(mangled.clone());
            let subst = param_map(&template.type_params, args);
            let variants: Vec<Variant> = template
                .variants
                .iter()
                .map(|v| Variant {
                    name: v.name.clone(),
                    payload: v.payload.iter().map(|t| subst_ty(t, &subst)).collect(),
                })
                .collect();

            // A nested generic payload (`Some(Box(int32))`) is an instantiation too.
            for v in &variants {
                for ty in &v.payload {
                    collect_apps(ty, queue);
                }
            }

            self.generated_enums.insert(
                mangled.clone(),
                EnumDef {
                    def: template.def,
                    name: mangled,
                    type_params: Vec::new(),
                    variants,
                },
            );
        }
        // Otherwise: not a generic template (an ordinary named type, or a name
        // caught by resolution). nothing to instantiate
    }
}

fn param_map<'a>(type_params: &'a [String], args: &'a [Ty]) -> HashMap<&'a str, &'a Ty> {
    type_params.iter().map(String::as_str).zip(args).collect()
}

/// Substitute type parameters for their concrete arguments.
fn subst_ty(ty: &Ty, subst: &HashMap<&str, &Ty>) -> Ty {
    match ty {
        Ty::Named(n) => match subst.get(n.as_str()) {
            Some(concrete) => (*concrete).clone(),
            None => ty.clone(),
        },
        Ty::Ptr(inner) => Ty::Ptr(Box::new(subst_ty(inner, subst))),
        Ty::Rc(inner) => Ty::Rc(Box::new(subst_ty(inner, subst))),
        Ty::App(n, args) => Ty::App(n.clone(), args.iter().map(|a| subst_ty(a, subst)).collect()),
        _ => ty.clone(),
    }
}

fn collect_apps(ty: &Ty, out: &mut Vec<(String, Vec<Ty>)>) {
    match ty {
        Ty::Ptr(inner) | Ty::Rc(inner) => collect_apps(inner, out),
        Ty::App(name, args) => {
            for a in args {
                collect_apps(a, out);
            }
            out.push((name.clone(), args.clone()));
        }
        _ => {}
    }
}

/// Rewrite `Ty::App` in place to the `Ty::Named` of its mangled concrete type
fn rewrite_ty(ty: &mut Ty) {
    match ty {
        Ty::Ptr(inner) | Ty::Rc(inner) | Ty::Array(inner, _) | Ty::Slice(inner) => {
            rewrite_ty(inner)
        }
        Ty::App(name, args) => {
            for a in args.iter_mut() {
                rewrite_ty(a);
            }
            *ty = Ty::Named(mangle(name, args));
        }
        _ => {}
    }
}

/// `Box(int32)` → `"Box_int32"`, `Box(@Node)` → `"Box_rc_Node"`.
fn mangle(name: &str, args: &[Ty]) -> String {
    let mut out = name.to_string();
    for a in args {
        out.push('_');
        out.push_str(&mangle_ty(a));
    }
    out
}

fn mangle_ty(ty: &Ty) -> String {
    match ty {
        Ty::Void => "void".to_string(),
        Ty::Bool => "bool".to_string(),
        Ty::CChar => "cchar".to_string(),
        Ty::Int { bits, signed } => {
            format!("{}{}", if *signed { "int" } else { "uint" }, width(bits))
        }
        Ty::Float { bits } => format!("float{bits}"),
        Ty::Ptr(inner) => format!("ptr_{}", mangle_ty(inner)),
        Ty::Rc(inner) => format!("rc_{}", mangle_ty(inner)),
        Ty::Array(elem, n) => format!("arr{n}_{}", mangle_ty(elem)),
        Ty::Slice(elem) => format!("slice_{}", mangle_ty(elem)),
        Ty::Named(n) => n.clone(),
        Ty::App(n, args) => mangle(n, args),
        Ty::Infer => "infer".to_string(),
    }
}

fn width(bits: &IntWidth) -> &'static str {
    match bits {
        IntWidth::W8 => "8",
        IntWidth::W16 => "16",
        IntWidth::W32 => "32",
        IntWidth::W64 => "64",
        IntWidth::Size => "size",
    }
}

fn each_structural_ty(item: &mut Item, f: &mut impl FnMut(&mut Ty)) {
    match item {
        Item::Struct(sd) => {
            for field in &mut sd.fields {
                f(&mut field.ty);
            }
        }
        Item::Enum(ed) => {
            for v in &mut ed.variants {
                for ty in &mut v.payload {
                    f(ty);
                }
            }
        }
        Item::Proc(p) => {
            for param in &mut p.params {
                f(&mut param.ty);
            }
            f(&mut p.ret);
            for st in &mut p.body {
                each_ty_in_stmt(st, f);
            }
        }
        Item::Include(_) | Item::ExternProc(_) => {}
    }
}

fn each_ty_in_stmt(s: &mut Stmt, f: &mut impl FnMut(&mut Ty)) {
    match s {
        Stmt::Let { ty, init, .. } => {
            f(ty);
            each_ty_in_expr(init, f);
        }
        Stmt::Assign { target, value, .. } => {
            each_ty_in_expr(target, f);
            each_ty_in_expr(value, f);
        }
        Stmt::Return(Some(e)) | Stmt::Expr(e) => each_ty_in_expr(e, f),
        Stmt::Block(body) => each_ty_in_block(body, f),
        Stmt::StaticAssert { cond, .. } => each_ty_in_expr(cond, f),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        Stmt::If {
            cond,
            then_branch,
            else_branch,
        } => {
            each_ty_in_expr(cond, f);
            each_ty_in_block(then_branch, f);
            if let Some(eb) = else_branch {
                each_ty_in_block(eb, f);
            }
        }
        Stmt::While { cond, body } => {
            each_ty_in_expr(cond, f);
            each_ty_in_block(body, f);
        }
        Stmt::CFor {
            init,
            cond,
            post,
            body,
        } => {
            if let Some(i) = init {
                each_ty_in_stmt(i, f);
            }
            if let Some(c) = cond {
                each_ty_in_expr(c, f);
            }
            if let Some(p) = post {
                each_ty_in_stmt(p, f);
            }
            each_ty_in_block(body, f);
        }
        Stmt::Loop { body } => each_ty_in_block(body, f),
        Stmt::Switch { scrutinee, arms } => {
            each_ty_in_expr(scrutinee, f);
            for arm in arms {
                each_ty_in_block(&mut arm.body, f);
            }
        }
    }
}

fn each_ty_in_block(body: &mut [Stmt], f: &mut impl FnMut(&mut Ty)) {
    for st in body {
        each_ty_in_stmt(st, f);
    }
}

fn rewrite_generic_call(e: &mut Expr) {
    let ExprKind::GenericCall {
        proc_name,
        type_args,
        args,
    } = &mut e.kind
    else {
        return;
    };
    let mangled = mangle(proc_name, type_args);
    let callee = Expr {
        kind: ExprKind::Name {
            def: DefId(0),
            name: mangled,
        },
        ty: e.ty.clone(),
        span: e.span,
    };
    e.kind = ExprKind::Call {
        callee: Box::new(callee),
        args: std::mem::take(args),
    };
}

fn each_expr_in_item(item: &mut Item, f: &mut impl FnMut(&mut Expr)) {
    if let Item::Proc(p) = item {
        each_expr_in_block(&mut p.body, f);
    }
}

fn each_expr_in_block(body: &mut [Stmt], f: &mut impl FnMut(&mut Expr)) {
    for st in body {
        each_expr_in_stmt(st, f);
    }
}

fn each_expr_in_stmt(s: &mut Stmt, f: &mut impl FnMut(&mut Expr)) {
    match s {
        Stmt::Let { init, .. } => each_expr(init, f),
        Stmt::Assign { target, value, .. } => {
            each_expr(target, f);
            each_expr(value, f);
        }
        Stmt::Return(Some(e)) | Stmt::Expr(e) => each_expr(e, f),
        Stmt::Block(body) => each_expr_in_block(body, f),
        Stmt::StaticAssert { cond, .. } => each_expr(cond, f),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        Stmt::If {
            cond,
            then_branch,
            else_branch,
        } => {
            each_expr(cond, f);
            each_expr_in_block(then_branch, f);
            if let Some(eb) = else_branch {
                each_expr_in_block(eb, f);
            }
        }
        Stmt::While { cond, body } => {
            each_expr(cond, f);
            each_expr_in_block(body, f);
        }
        Stmt::CFor {
            init,
            cond,
            post,
            body,
        } => {
            if let Some(i) = init {
                each_expr_in_stmt(i, f);
            }
            if let Some(c) = cond {
                each_expr(c, f);
            }
            if let Some(p) = post {
                each_expr_in_stmt(p, f);
            }
            each_expr_in_block(body, f);
        }
        Stmt::Loop { body } => each_expr_in_block(body, f),
        Stmt::Switch { scrutinee, arms } => {
            each_expr(scrutinee, f);
            for arm in arms {
                each_expr_in_block(&mut arm.body, f);
            }
        }
    }
}

fn each_expr(e: &mut Expr, f: &mut impl FnMut(&mut Expr)) {
    f(e);
    match &mut e.kind {
        ExprKind::Unary { operand, .. } | ExprKind::Paren(operand) => each_expr(operand, f),
        ExprKind::Binary { lhs, rhs, .. } => {
            each_expr(lhs, f);
            each_expr(rhs, f);
        }
        ExprKind::Call { callee, args } => {
            each_expr(callee, f);
            for a in args {
                each_expr(a, f);
            }
        }
        ExprKind::Field { recv, .. } => each_expr(recv, f),
        ExprKind::Index { base, index } => {
            each_expr(base, f);
            each_expr(index, f);
        }
        ExprKind::Cast { operand, .. } => each_expr(operand, f),
        ExprKind::Alloc { fields, .. } | ExprKind::StructLit { fields, .. } => {
            for (_, fe) in fields {
                each_expr(fe, f);
            }
        }
        ExprKind::ArrayLit { elements, .. } => {
            for e in elements {
                each_expr(e, f);
            }
        }
        ExprKind::SliceAll { array } => each_expr(array, f),
        ExprKind::EnumInit { args, .. } | ExprKind::GenericCall { args, .. } => {
            for a in args {
                each_expr(a, f);
            }
        }
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Char(_)
        | ExprKind::Bool(_)
        | ExprKind::SizeOf(_)
        | ExprKind::ZeroValue(_)
        | ExprKind::Name { .. }
        | ExprKind::Unresolved(_) => {}
    }
}

fn each_ty_in_expr(e: &mut Expr, f: &mut impl FnMut(&mut Ty)) {
    f(&mut e.ty);
    match &mut e.kind {
        ExprKind::Unary { operand, .. } | ExprKind::Paren(operand) => each_ty_in_expr(operand, f),
        ExprKind::SizeOf(ty) => f(ty),
        ExprKind::Binary { lhs, rhs, .. } => {
            each_ty_in_expr(lhs, f);
            each_ty_in_expr(rhs, f);
        }
        ExprKind::Call { callee, args } => {
            each_ty_in_expr(callee, f);
            for a in args {
                each_ty_in_expr(a, f);
            }
        }
        ExprKind::Field { recv, .. } => each_ty_in_expr(recv, f),
        ExprKind::Index { base, index } => {
            each_ty_in_expr(base, f);
            each_ty_in_expr(index, f);
        }
        ExprKind::Cast { ty, operand } => {
            f(ty);
            each_ty_in_expr(operand, f);
        }
        ExprKind::Alloc { ty, fields } | ExprKind::StructLit { ty, fields } => {
            f(ty);
            for (_, fe) in fields {
                each_ty_in_expr(fe, f);
            }
        }
        ExprKind::ArrayLit { ty, elements } => {
            f(ty);
            for e in elements {
                each_ty_in_expr(e, f);
            }
        }
        ExprKind::ZeroValue(ty) => f(ty),
        ExprKind::SliceAll { array } => each_ty_in_expr(array, f),
        ExprKind::EnumInit { args, .. } => {
            for a in args {
                each_ty_in_expr(a, f);
            }
        }
        ExprKind::GenericCall {
            type_args, args, ..
        } => {
            for ty in type_args {
                f(ty);
            }
            for a in args {
                each_ty_in_expr(a, f);
            }
        }
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Char(_)
        | ExprKind::Bool(_)
        | ExprKind::Name { .. }
        | ExprKind::Unresolved(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower;

    fn mono_items(src: &str) -> Vec<String> {
        let parsed = dray_syntax::parse(src);
        let (hir, _errs) = lower(&parsed.root);
        monomorphize(hir)
            .expect("monomorphization should succeed")
            .items
            .iter()
            .filter_map(|it| match it {
                Item::Struct(sd) => Some(sd.name.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn instantiation_generates_concrete_struct_and_consumes_template() {
        let names = mono_items(
            "Box :: struct(comptime T: type) { value: T }\n\
             main :: proc() -> int32 { b := alloc Box(int32){ value: 1 }; return b.value; }\n",
        );
        assert!(names.contains(&"Box_int32".to_string()), "{names:?}");
        // The generic template itself is consumed, never emitted.
        assert!(
            !names.contains(&"Box".to_string()),
            "template leaked: {names:?}"
        );
    }

    #[test]
    fn distinct_instantiations_are_separate_types() {
        let names = mono_items(
            "Pair :: struct(comptime A: type, comptime B: type) { first: A, second: B }\n\
             main :: proc() -> int32 {\n\
                 p := alloc Pair(int32, bool){ first: 1, second: true };\n\
                 q := alloc Pair(int32, int32){ first: 1, second: 2 };\n\
                 return 0;\n\
             }\n",
        );
        assert!(names.contains(&"Pair_int32_bool".to_string()), "{names:?}");
        assert!(names.contains(&"Pair_int32_int32".to_string()), "{names:?}");
    }

    #[test]
    fn rc_type_argument_is_mangled() {
        let names = mono_items(
            "Node :: struct { value: int32 }\n\
             Box :: struct(comptime T: type) { value: T }\n\
             main :: proc() -> int32 { b := alloc Box(@Node){ value: alloc Node{ value: 1 } }; return 0; }\n",
        );
        assert!(names.contains(&"Box_rc_Node".to_string()), "{names:?}");
    }

    fn mono_enum_names(src: &str) -> Vec<String> {
        let parsed = dray_syntax::parse(src);
        let (hir, _errs) = lower(&parsed.root);
        monomorphize(hir)
            .expect("monomorphization should succeed")
            .items
            .iter()
            .filter_map(|it| match it {
                Item::Enum(ed) => Some(ed.name.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn generic_enum_instantiations_are_concrete_and_distinct() {
        let names = mono_enum_names(
            "Maybe :: enum(comptime T: type) { Some(T), None }\n\
             main :: proc() -> int32 {\n\
                 a := Maybe(int32).Some(1);\n\
                 b := Maybe(bool).None;\n\
                 return 0;\n\
             }\n",
        );
        assert!(names.contains(&"Maybe_int32".to_string()), "{names:?}");
        assert!(names.contains(&"Maybe_bool".to_string()), "{names:?}");
        assert!(
            !names.contains(&"Maybe".to_string()),
            "template leaked: {names:?}"
        );
    }

    #[test]
    fn infinitely_recursive_generic_is_rejected_not_hung() {
        let parsed = dray_syntax::parse(
            "Wrap :: struct(comptime T: type) { inner: @Wrap(Wrap(T)) }\n\
             f :: proc(w: @Wrap(int32)) -> int32 { return 0; }\n\
             main :: proc() -> int32 { return 0; }\n",
        );
        let (hir, _errs) = lower(&parsed.root);
        assert!(monomorphize(hir).is_err());
    }
}
