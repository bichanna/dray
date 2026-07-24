// SPDX-License-Identifier: Apache-2.0

//! Build orchestration for Dray

mod backend;
pub use backend::{Backend, CcInvocation};

use std::path::{Path, PathBuf};

use dray_hir::lower;
use dray_syntax::parse;

/// Anything that can go wrong building a Dray program.
#[derive(Debug)]
pub enum BuildError {
    /// The source failed to parse. Carries rendered diagnostics.
    Parse(Vec<String>),
    /// Name resolution / HIR lowering failed.
    Resolve(Vec<String>),
    /// Monomorphization failed (e.g. an infinitely recursive generic type)
    Monomorphize(String),
    /// Lowering the HIR to C failed.
    Codegen(String),
    /// An I/O error reading source or writing outputs.
    Io(std::io::Error),
    /// The C compiler was not found or failed.
    CC(String),
    /// Dray's own `lib/system/` could not be located.
    MissingLib(Vec<PathBuf>),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::Parse(errs) => render_list(f, "parse", errs),
            BuildError::Resolve(errs) => render_list(f, "name resolution", errs),
            BuildError::Monomorphize(m) => write!(f, "monomorphization error: {m}"),
            BuildError::Codegen(m) => write!(f, "{m}"),
            BuildError::Io(e) => write!(f, "io error: {e}"),
            BuildError::MissingLib(tried) => {
                writeln!(
                    f,
                    "cannot find Dray's runtime library (lib/system/draybase.h)"
                )?;
                writeln!(f, "  looked in:")?;
                for p in tried {
                    writeln!(f, "    {}", p.display())?;
                }
                write!(f, "  set $DRAY_LIB or pass --lib to point at it")
            }
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
    /// Forward the C compiler's warnings instead of silencing them
    pub show_c_warnings: bool,
    /// Extra flags handed to the C compiler untouched.
    pub cflags: Vec<String>,
    /// Where to put generated C. Defaults to `build/<program>/`.
    pub build_dir: Option<PathBuf>,
    /// Where Dray's own `lib/` lives. Searched if not given.
    pub lib_dir: Option<PathBuf>,
}

impl Default for BuildOptions {
    fn default() -> Self {
        BuildOptions {
            cc: std::env::var("CC").unwrap_or_else(|_| "cc".to_string()),
            emit_c: false,
            show_c_warnings: false,
            cflags: Vec::new(),
            build_dir: None,
            lib_dir: None,
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
    let hir = dray_hir::monomorphize(source_to_hir(src)?)
        .map_err(|e| BuildError::Monomorphize(e.to_string()))?;
    Ok(dray_ir::lower(&hir))
}

/// The full front end: parse → HIR → IR → C source.
pub fn source_to_c(src: &str) -> Result<String, BuildError> {
    let ir = source_to_ir(src)?;
    dray_codegen::ir_to_c(&ir).map_err(|e| BuildError::Codegen(e.to_string()))
}

pub fn source_to_c_from_file(src: &str, file: &str) -> Result<String, BuildError> {
    let mut ir = source_to_ir(src)?;
    ir.source = Some(dray_ir::SourceMap::new(file, src));
    dray_codegen::ir_to_c(&ir).map_err(|e| BuildError::Codegen(e.to_string()))
}

/// Build a Dray source file into an executable at `out_path`. Returns the path
/// to the generated C file.
fn system_lib_dir(opts: &BuildOptions) -> Result<PathBuf, BuildError> {
    let mut tried: Vec<PathBuf> = Vec::new();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = &opts.lib_dir {
        candidates.push(dir.clone());
    }
    if let Ok(dir) = std::env::var("DRAY_LIB") {
        candidates.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin) = exe.parent() {
            candidates.push(bin.join("../lib"));
            candidates.push(bin.join("../../../lib"));
        }
    }
    candidates.push(PathBuf::from("lib"));

    for base in candidates {
        let system = base.join("system");
        if system.join("draybase.h").is_file() {
            return Ok(system);
        }
        tried.push(system);
    }
    Err(BuildError::MissingLib(tried))
}

/// Where generated C for this build lives.
fn build_dir(opts: &BuildOptions, out_path: &Path) -> PathBuf {
    if let Some(dir) = &opts.build_dir {
        return dir.clone();
    }
    let name = out_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "dray".to_string());
    out_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("build")
        .join(name)
}

pub fn build_file(
    src_path: &Path,
    out_path: &Path,
    opts: &BuildOptions,
) -> Result<PathBuf, BuildError> {
    let src = std::fs::read_to_string(src_path)?;
    let abs_src = std::fs::canonicalize(src_path).unwrap_or_else(|_| src_path.to_path_buf());
    let c_code = source_to_c_from_file(&src, &abs_src.display().to_string())?;

    let dir = build_dir(opts, out_path);
    std::fs::create_dir_all(&dir)?;

    let stem = src_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "main".to_string());
    let c_path = dir.join(format!("{stem}.c"));
    std::fs::write(&c_path, &c_code)?;

    let lib = system_lib_dir(opts)?;
    let base_h = dir.join("draybase.h");
    let base_c = dir.join("draybase.c");
    std::fs::copy(lib.join("draybase.h"), &base_h)?;
    std::fs::copy(lib.join("draybase.c"), &base_c)?;

    let includes = [dir.clone()];
    let invocation = CcInvocation {
        cc: &opts.cc,
        include_dirs: &includes,
        backend: Backend::detect(&opts.cc),
        show_warnings: opts.show_c_warnings,
        extra: &opts.cflags,
    };
    let status = invocation
        .command_multi(&[c_path.clone(), base_c.clone()], out_path)
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

    Ok(c_path)
}
