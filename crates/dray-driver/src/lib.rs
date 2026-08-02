// SPDX-License-Identifier: Apache-2.0

//! Build orchestration for Dray

mod backend;
pub use backend::{Backend, CcInvocation};

use std::collections::{HashMap, VecDeque};
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

fn cc_spawn_error(cc: &str, e: std::io::Error) -> BuildError {
    BuildError::CC(format!(
        "could not run the C compiler `{cc}` ({e}). Install one, or set $CC to a compiler that \
         exists (for example `CC=gcc` or `CC=clang`)."
    ))
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_diagnostic(map: &dray_ir::SourceMap, offset: u32, message: &str) -> String {
    let (line, col) = map.line_col(offset);
    format!("{}:{}:{}: {}", map.file(), line, col, message)
}

fn source_to_hir(src: &str) -> Result<dray_hir::Hir, BuildError> {
    let parsed = parse(src);
    let map = dray_ir::SourceMap::new("<input>", src);
    if !parsed.errors.is_empty() {
        return Err(BuildError::Parse(
            parsed
                .errors
                .iter()
                .map(|e| format_diagnostic(&map, e.span.start, &e.message))
                .collect(),
        ));
    }

    let (hir, resolve_errors) = lower(&parsed.root);
    if !resolve_errors.is_empty() {
        return Err(BuildError::Resolve(
            resolve_errors
                .iter()
                .map(|e| format_diagnostic(&map, e.span.start, &e.message))
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

struct LoadedModule {
    parsed: dray_syntax::Parse,
    imports: Vec<(dray_syntax::ImportInfo, usize)>,
    src: String,
    path: String,
}

fn resolve_import_path(dir: &Path, path: &str, lib: Option<&Path>) -> Result<PathBuf, BuildError> {
    let filename = if Path::new(path).extension().is_some() {
        path.to_string()
    } else {
        format!("{path}.dray")
    };

    if let Ok(canon) = std::fs::canonicalize(dir.join(&filename)) {
        return Ok(canon);
    }
    if let Some(lib) = lib
        && let Ok(canon) = std::fs::canonicalize(lib.join(&filename))
    {
        return Ok(canon);
    }

    Err(BuildError::Parse(vec![format!(
        "cannot import \"{path}\": not found next to the file or in the lib directory"
    )]))
}

fn build_module_graph(loaded: &[LoadedModule], prelude_indices: &[usize]) -> dray_hir::ModuleGraph {
    let files = loaded
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let mut fi = dray_hir::FileImports::default();
            if !prelude_indices.contains(&i) {
                for &p in prelude_indices {
                    fi.globs.push(p);
                }
            }

            for (imp, target) in &m.imports {
                match &imp.only {
                    // `alias :: import("m")` qualified only
                    None => {
                        fi.aliases.push((imp.alias.clone(), *target));
                    }
                    // `x :: import("m") for a, b`
                    Some(names) => {
                        for n in names {
                            fi.selective.push((n.clone(), *target));
                        }
                    }
                }
            }
            fi
        })
        .collect();
    dray_hir::ModuleGraph { files }
}

fn source_to_c_with_imports(
    entry_src: &str,
    entry_path: &Path,
    preludes: &[PathBuf],
    lib_dir: Option<&Path>,
) -> Result<(dray_codegen::CModules, Vec<String>), BuildError> {
    let entry_canon =
        std::fs::canonicalize(entry_path).unwrap_or_else(|_| entry_path.to_path_buf());

    let mut index_of: HashMap<PathBuf, usize> = HashMap::new();
    let mut queue: VecDeque<(String, PathBuf)> = VecDeque::new();
    index_of.insert(entry_canon.clone(), 0);
    queue.push_back((entry_src.to_string(), entry_canon.clone()));
    let mut pending: Vec<Option<LoadedModule>> = vec![None];

    let mut prelude_indices: Vec<usize> = Vec::new();
    for prelude in preludes {
        let Ok(canon) = std::fs::canonicalize(prelude) else {
            continue;
        };
        if canon == entry_canon || index_of.contains_key(&canon) {
            continue;
        }
        let i = pending.len();
        index_of.insert(canon.clone(), i);
        pending.push(None);
        let src = std::fs::read_to_string(&canon).map_err(|e| {
            BuildError::Parse(vec![format!(
                "cannot read the prelude {}: {e}",
                canon.display()
            )])
        })?;
        queue.push_back((src, canon));
        prelude_indices.push(i);
    }

    while let Some((src, path)) = queue.pop_front() {
        let parsed = parse(&src);
        if !parsed.errors.is_empty() {
            let map = dray_ir::SourceMap::new(path.display().to_string(), &src);
            return Err(BuildError::Parse(
                parsed
                    .errors
                    .iter()
                    .map(|e| format_diagnostic(&map, e.span.start, &e.message))
                    .collect(),
            ));
        }

        let my_index = index_of[&path];
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut edges: Vec<(dray_syntax::ImportInfo, usize)> = Vec::new();

        for imp in dray_syntax::imports(&parsed.root) {
            let canon = resolve_import_path(&dir, &imp.path, lib_dir)?;
            // A file importing itself is always a mistake
            if canon == path {
                let map = dray_ir::SourceMap::new(path.display().to_string(), &src);
                return Err(BuildError::Resolve(vec![format_diagnostic(
                    &map,
                    imp.span.start,
                    &format!("a module cannot import itself (`{}`)", imp.path),
                )]));
            }

            let target = match index_of.get(&canon) {
                Some(&i) => i,
                None => {
                    let i = pending.len();
                    index_of.insert(canon.clone(), i);
                    pending.push(None);
                    let imported_src = std::fs::read_to_string(&canon).map_err(|e| {
                        BuildError::Parse(vec![format!("cannot import \"{}\": {e}", imp.path)])
                    })?;
                    queue.push_back((imported_src, canon));
                    i
                }
            };
            edges.push((imp, target));
        }

        pending[my_index] = Some(LoadedModule {
            parsed,
            imports: edges,
            src: src.clone(),
            path: path.display().to_string(),
        });
    }

    let loaded: Vec<LoadedModule> = pending
        .into_iter()
        .map(|slot| slot.expect("every module index is filled by the BFS"))
        .collect();

    let mut index_paths: Vec<PathBuf> = vec![PathBuf::new(); loaded.len()];
    for (path, &i) in &index_of {
        index_paths[i] = path.clone();
    }

    let graph = build_module_graph(&loaded, &prelude_indices);
    let roots: Vec<&dray_syntax::SyntaxNode> = loaded.iter().map(|m| &m.parsed.root).collect();
    let (hir, resolve_errors) = dray_hir::lower_files_with_graph(&roots, &graph);
    if !resolve_errors.is_empty() {
        let maps: Vec<dray_ir::SourceMap> = loaded
            .iter()
            .map(|m| dray_ir::SourceMap::new(m.path.clone(), &m.src))
            .collect();
        return Err(BuildError::Resolve(
            resolve_errors
                .iter()
                .map(|e| match maps.get(e.file) {
                    Some(map) => format_diagnostic(map, e.span.start, &e.message),
                    None => format!("{}..{}: {}", e.span.start, e.span.end, e.message),
                })
                .collect(),
        ));
    }

    let mono = dray_hir::monomorphize(hir).map_err(|e| BuildError::Monomorphize(e.to_string()))?;
    let mut ir = dray_ir::lower(&mono);
    ir.source = Some(dray_ir::SourceMap::new(
        &entry_path.display().to_string(),
        entry_src,
    ));
    ir.sources = loaded
        .iter()
        .map(|m| dray_ir::SourceMap::new(&m.path, &m.src))
        .collect();

    let stems: Vec<String> = loaded
        .iter()
        .enumerate()
        .map(|(i, _)| module_stem(&index_paths, i))
        .collect();
    let modules = dray_codegen::ir_to_c_modules(&ir, HEADER_NAME)
        .map_err(|e| BuildError::Codegen(e.to_string()))?;
    Ok((modules, stems))
}

const HEADER_NAME: &str = "dray_program.h";

/// The prelude modules: every `.dray` file in `<lib>/prelude/`. They are loaded
/// (sorted by name for a deterministic order) and implicitly glob imported into
/// every program, so their public names are always in scope. Adding a prelude
/// module is just dropping a file into that directory — no compiler change.
fn prelude_paths(opts: &BuildOptions) -> Vec<PathBuf> {
    let Some(lib_root) = system_lib_dir(opts)
        .ok()
        .and_then(|d| d.parent().map(Path::to_path_buf))
    else {
        return Vec::new();
    };

    let dir = lib_root.join("prelude");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "dray"))
        .collect();
    paths.sort();
    paths
}

fn module_stem(paths: &[PathBuf], i: usize) -> String {
    paths
        .get(i)
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("module{i}"))
}

pub fn source_to_c_from_file(src: &str, file: &str) -> Result<String, BuildError> {
    let mut ir = source_to_ir(src)?;
    ir.source = Some(dray_ir::SourceMap::new(file, src));
    dray_codegen::ir_to_c(&ir).map_err(|e| BuildError::Codegen(e.to_string()))
}

pub fn emit_c_from_file(
    src: &str,
    entry_path: &Path,
    preludes: &[PathBuf],
) -> Result<String, BuildError> {
    let (modules, stems) = source_to_c_with_imports(src, entry_path, preludes, None)?;
    let mut out = String::new();
    out.push_str(&format!("// ==== header: {HEADER_NAME} ====\n"));
    out.push_str(&modules.header);
    for (i, module_c) in modules.modules.iter().enumerate() {
        let name = stems
            .get(i)
            .map(|s| format!("{s}.c"))
            .unwrap_or_else(|| format!("module{i}.c"));
        out.push_str(&format!("\n// ==== file: {name} ====\n"));
        out.push_str(module_c);
    }
    Ok(out)
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
    if let Ok(exe) = std::env::current_exe()
        && let Some(bin) = exe.parent()
    {
        candidates.push(bin.join("../lib"));
        candidates.push(bin.join("../../../lib"));
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

/// Write `contents` to `path` only if it differs from what is already there, so
/// an unchanged file keeps its modification time
fn write_if_changed(path: &Path, contents: &str) -> std::io::Result<bool> {
    if let Ok(existing) = std::fs::read_to_string(path)
        && existing == contents
    {
        return Ok(false);
    }
    std::fs::write(path, contents)?;
    Ok(true)
}

/// Copy `from` to `to` only if the destination differs
fn copy_if_changed(from: &Path, to: &Path) -> std::io::Result<()> {
    let src = std::fs::read(from)?;
    if let Ok(dst) = std::fs::read(to)
        && dst == src
    {
        return Ok(());
    }
    std::fs::write(to, src)?;
    Ok(())
}

/// The last modified time of a file, if it exists and the OS reports one
fn file_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Whether `source` must be recompiled into `object`: true if the object is
/// missing, older than the source, or older than the newest shared header
fn needs_recompile(
    source: &Path,
    object: &Path,
    newest_header: Option<std::time::SystemTime>,
) -> bool {
    let (Some(obj_time), Some(src_time)) = (file_mtime(object), file_mtime(source)) else {
        return true;
    };
    if src_time > obj_time {
        return true;
    }
    match newest_header {
        Some(h) => h > obj_time,
        None => false,
    }
}

pub fn build_file(
    src_path: &Path,
    out_path: &Path,
    opts: &BuildOptions,
) -> Result<PathBuf, BuildError> {
    let src = std::fs::read_to_string(src_path)?;
    let abs_src = std::fs::canonicalize(src_path).unwrap_or_else(|_| src_path.to_path_buf());
    let lib_system = system_lib_dir(opts)?;
    let lib_root = lib_system.parent().map(Path::to_path_buf);
    let preludes = prelude_paths(opts);
    let (cmodules, stems) =
        source_to_c_with_imports(&src, &abs_src, &preludes, lib_root.as_deref())?;

    let dir = build_dir(opts, out_path);
    std::fs::create_dir_all(&dir)?;

    let header_path = dir.join(HEADER_NAME);
    write_if_changed(&header_path, &cmodules.header)?;

    let mut c_paths: Vec<PathBuf> = Vec::with_capacity(cmodules.modules.len());
    let mut used: HashMap<String, usize> = HashMap::new();
    for (i, source) in cmodules.modules.iter().enumerate() {
        let stem = stems
            .get(i)
            .cloned()
            .unwrap_or_else(|| format!("module{i}"));
        let n = used.entry(stem.clone()).or_insert(0);
        let name = if *n == 0 {
            format!("{stem}.c")
        } else {
            format!("{stem}.{n}.c")
        };
        *n += 1;
        let path = dir.join(name);
        write_if_changed(&path, source)?;
        c_paths.push(path);
    }

    let lib = &lib_system;
    let base_h = dir.join("draybase.h");
    let base_c = dir.join("draybase.c");
    let rc_h = dir.join("drayrc.h");
    let rc_c = dir.join("drayrc.c");

    copy_if_changed(&lib.join("draybase.h"), &base_h)?;
    copy_if_changed(&lib.join("draybase.c"), &base_c)?;
    copy_if_changed(&lib.join("drayrc.h"), &rc_h)?;
    copy_if_changed(&lib.join("drayrc.c"), &rc_c)?;

    let includes = [dir.clone()];
    let invocation = CcInvocation {
        cc: &opts.cc,
        include_dirs: &includes,
        backend: Backend::detect(&opts.cc),
        show_warnings: opts.show_c_warnings,
        extra: &opts.cflags,
    };

    let mut all_c = c_paths.clone();
    all_c.push(base_c.clone());
    all_c.push(rc_c.clone());

    // The shared header and runtime headers affect every module, so a change to
    // any of them forces a full recompile
    let newest_header = [&header_path, &base_h, &rc_h]
        .iter()
        .filter_map(|p| file_mtime(p))
        .max();

    let mut objects: Vec<PathBuf> = Vec::with_capacity(all_c.len());
    for c in &all_c {
        let obj = c.with_extension("o");
        if !needs_recompile(c, &obj, newest_header) {
            objects.push(obj);
            continue;
        }
        let output = invocation
            .compile_object(c, &obj)
            .output()
            .map_err(|e| cc_spawn_error(&opts.cc, e))?;
        if !output.status.success() {
            // Generated C failing to compile is a compiler bug, not a user
            // error — say so, and show what the C compiler reported.
            return Err(BuildError::CC(format!(
                "internal error: the C generated for {} did not compile — this is a bug in Dray, \
                 not your program. The generated C is in {} so it can be inspected or reported.\n\n\
                 {} said:\n{}",
                c.display(),
                dir.display(),
                opts.cc,
                indent(&String::from_utf8_lossy(&output.stderr)),
            )));
        }
        objects.push(obj);
    }

    let output = invocation
        .link_objects(&objects, out_path)
        .output()
        .map_err(|e| cc_spawn_error(&opts.cc, e))?;

    if !output.status.success() {
        // A link failure on generated objects is likewise a compiler/runtime
        // bug: a missing symbol means codegen referenced something the runtime
        // does not define. Surface the linker's own message, which names it.
        return Err(BuildError::CC(format!(
            "internal error: linking failed. This is likely a bug in Dray. A missing symbol usually means \
             the generated code referenced a runtime function that is not linked in. Objects are in \
             {}.\n\n{} said:\n{}",
            dir.display(),
            opts.cc,
            indent(&String::from_utf8_lossy(&output.stderr)),
        )));
    }

    Ok(c_paths.into_iter().next().unwrap_or(header_path))
}
