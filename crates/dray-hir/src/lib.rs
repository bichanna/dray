// SPDX-License-Identifier: Apache-2.0

//! `dray-hir` — the High-level IR: a resolved, typed tree lowered from the CST.

pub mod debug;
pub mod hir;
mod lower;
mod types;

pub use debug::dump_hir;
pub use dray_syntax::Span;
pub use hir::*;
pub use lower::{ResolveError, lower};
