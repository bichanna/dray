// SPDX-License-Identifier: Apache-2.0

//! Build orchestration for Dray

use std::path::{Path, PathBuf};
use std::process::Command;

use dray_hir::lower;
use dray_syntax::parse;

/// Anything that can go wrong building a Dray program.
#[derive(Debug)]
pub enum BuildError {
    /// The source failed to parse. Carries rendered diagnostics.
    Parse(Vec<String>),
    /// Name resolution / HIR lowering failed.
    Resolve(Vec<String>),
    /// Lowering the HIR to C failed.
    Codegen(String),
    /// An I/O error reading source or writing outputs.
    Io(std::io::Error),
    /// The C compiler was not found or failed.
    CC(String),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::Parse(errs) => render_list(f, "parse", errs),
            BuildError::Resolve(errs) => render_list(f, "name resolution", errs),
            BuildError::Codegen(m) => write!(f, "{m}"),
            BuildError::Io(e) => write!(f, "io error: {e}"),
            BuildError::CC(m) => write!(f, "C compiler error: {m}"),
        }
    }
}

fn render_list(f: &mut std::fmt::Formatter<'_>, stage: &str, errs: &[String]) -> std::fmt::Result {
    writeln!(f, "{stage} failed with {} error(s):", errs.len())?;
    for e in errs {
        writeln!(f, "  {e}")?;
    }
    Ok(())
}

impl std::error::Error for BuildError {}

impl From<std::io::Error> for BuildError {
    fn from(e: std::io::Error) -> Self {
        BuildError::Io(e)
    }
}

pub struct BuildOptions {
    /// The C compiler to invoke (default `cc`, overridable via `$CC`).
    pub cc: String,
    /// Keep the generated `.c` file next to the output instead of removing it.
    pub emit_c: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        BuildOptions {
            cc: std::env::var("CC").unwrap_or_else(|_| "cc".to_string()),
            emit_c: false,
        }
    }
}

fn source_to_hir(src: &str) -> Result<dray_hir::Hir, BuildError> {
    let parsed = parse(src);
    if !parsed.errors.is_empty() {
        return Err(BuildError::Parse(
            parsed
                .errors
                .iter()
                .map(|e| format!("{}..{}: {}", e.span.start, e.span.end, e.message))
                .collect(),
        ));
    }

    let (hir, resolve_errors) = lower(&parsed.root);
    if !resolve_errors.is_empty() {
        return Err(BuildError::Resolve(
            resolve_errors
                .iter()
                .map(|e| format!("{}..{}: {}", e.span.start, e.span.end, e.message))
                .collect(),
        ));
    }
    Ok(hir)
}

/// Parse → HIR → IR (the RC-annotated mid-level form). Used by `dump-ir`.
pub fn source_to_ir(src: &str) -> Result<dray_ir::Ir, BuildError> {
    Ok(dray_ir::lower(&source_to_hir(src)?))
}

/// The full front end: parse → HIR → IR → C source.
pub fn source_to_c(src: &str) -> Result<String, BuildError> {
    let ir = source_to_ir(src)?;
    dray_codegen::ir_to_c(&ir).map_err(|e| BuildError::Codegen(e.to_string()))
}

/// Build a Dray source file into an executable at `out_path`. Returns the path
/// to the generated C file.
pub fn build_file(
    src_path: &Path,
    out_path: &Path,
    opts: &BuildOptions,
) -> Result<PathBuf, BuildError> {
    let src = std::fs::read_to_string(src_path)?;
    let c_code = source_to_c(&src)?;

    let c_path = c_output_path(out_path);
    std::fs::write(&c_path, &c_code)?;

    let status = Command::new(&opts.cc)
        .arg(&c_path)
        .arg("-o")
        .arg(out_path)
        .status()
        .map_err(|e| {
            BuildError::CC(format!(
                "failed to run `{}` (is a C compiler installed?): {e}",
                opts.cc
            ))
        })?;

    if !status.success() {
        return Err(BuildError::CC(format!(
            "`{}` exited with {}; generated C left at {}",
            opts.cc,
            status,
            c_path.display()
        )));
    }

    if !opts.emit_c {
        let _ = std::fs::remove_file(&c_path);
    }
    Ok(c_path)
}

fn c_output_path(out_path: &Path) -> PathBuf {
    let mut s = out_path.as_os_str().to_os_string();
    s.push(".c");
    PathBuf::from(s)
}
