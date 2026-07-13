// SPDX-License-Identifier: Apache-2.0

//! The `dray` command-line entry point.

use std::io::Read;
use std::process::ExitCode;

use dray_driver::{BuildOptions, build_file, source_to_c, source_to_ir};
use dray_hir::{dump_hir, lower};
use dray_ir::dump_ir;
use dray_syntax::{DumpOptions, dump_cst_with, dump_tokens, dump_tokens_no_trivia, parse};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(CliError::Usage(msg)) => {
            eprintln!("dray: {msg}");
            eprintln!();
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
        Err(CliError::Failed(msg)) => {
            if !msg.is_empty() {
                eprintln!("{msg}");
            }
            ExitCode::FAILURE
        }
    }
}

enum CliError {
    /// The command was used incorrectly — show the usage block.
    Usage(String),
    /// The input was read fine but failed to compile — the diagnostics are
    /// already rendered in the message (or already printed), so no usage block.
    Failed(String),
}

impl From<String> for CliError {
    fn from(msg: String) -> CliError {
        CliError::Usage(msg)
    }
}

impl From<&str> for CliError {
    fn from(msg: &str) -> CliError {
        CliError::Usage(msg.to_string())
    }
}

const USAGE: &str = "\
usage:
  dray dump-tokens [--no-trivia] <file>
  dray dump-cst    [--trivia] [--no-spans] [--shape] <file>
  dray dump-hir    <file>              resolve + type, print the HIR
  dray dump-ir     <file>              lower to IR, print it (retain/release shown)
  dray emit-c      <file>              generate C and print it (use - for stdin)
  dray build       [-o <out>] [--emit-c] <file>   compile to an executable

  <file>   a .dray source file (or - for stdin, where supported)

dump-cst options:
  --trivia     include whitespace/comment leaves
  --no-spans   omit @start..end byte spans
  --shape      structure only (implies --no-spans, no text, no trivia)

build options:
  -o <out>     output executable path (default: a.out)
  --emit-c     keep the generated .c file next to the executable";

fn run(args: &[String]) -> Result<(), CliError> {
    let cmd = args.first().ok_or("expected a subcommand")?;
    match cmd.as_str() {
        "dump-tokens" => dump_tokens_cmd(&args[1..]),
        "dump-cst" => dump_cst_cmd(&args[1..]),
        "dump-hir" => dump_hir_cmd(&args[1..]),
        "dump-ir" => dump_ir_cmd(&args[1..]),
        "emit-c" => emit_c_cmd(&args[1..]),
        "build" => build_cmd(&args[1..]),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown subcommand `{other}`").into()),
    }
}

fn dump_tokens_cmd(args: &[String]) -> Result<(), CliError> {
    let mut no_trivia = false;
    let mut path: Option<&str> = None;
    for a in args {
        match a.as_str() {
            "--no-trivia" => no_trivia = true,
            flag if flag.starts_with("--") => {
                return Err(format!("unknown flag `{flag}` for dump-tokens").into());
            }
            positional => {
                if path.replace(positional).is_some() {
                    return Err("more than one input given".into());
                }
            }
        }
    }
    let src = read_source(path.ok_or("no input file given")?)?;
    let out = if no_trivia {
        dump_tokens_no_trivia(&src)
    } else {
        dump_tokens(&src)
    };
    print!("{out}");
    Ok(())
}

fn dump_cst_cmd(args: &[String]) -> Result<(), CliError> {
    let mut opts = DumpOptions::default();
    let mut shape = false;
    let mut path: Option<&str> = None;
    for a in args {
        match a.as_str() {
            "--trivia" => opts = opts.with_trivia(true),
            "--no-spans" => opts = opts.with_spans(false),
            "--shape" => shape = true,
            flag if flag.starts_with("--") => {
                return Err(format!("unknown flag `{flag}` for dump-cst").into());
            }
            positional => {
                if path.replace(positional).is_some() {
                    return Err("more than one input given".into());
                }
            }
        }
    }
    if shape {
        opts = DumpOptions::shape_only();
    }
    let src = read_source(path.ok_or("no input file given")?)?;
    let parsed = parse(&src);
    print!("{}", dump_cst_with(&parsed.root, opts));

    // Spit out in stderr instead
    if !parsed.errors.is_empty() {
        eprintln!("\n{} parse error(s):", parsed.errors.len());
        for e in &parsed.errors {
            eprintln!("  {}..{}: {}", e.span.start, e.span.end, e.message);
        }
    }
    Ok(())
}

fn emit_c_cmd(args: &[String]) -> Result<(), CliError> {
    let mut path: Option<&str> = None;
    for a in args {
        match a.as_str() {
            flag if flag.starts_with("--") => {
                return Err(format!("unknown flag `{flag}` for emit-c").into());
            }
            positional => {
                if path.replace(positional).is_some() {
                    return Err("more than one input given".into());
                }
            }
        }
    }
    let src = read_source(path.ok_or("no input file given")?)?;
    match source_to_c(&src) {
        Ok(c) => {
            print!("{c}");
            Ok(())
        }
        // Build errors already render their own diagnostics.
        Err(e) => Err(CliError::Failed(e.to_string())),
    }
}

fn build_cmd(args: &[String]) -> Result<(), CliError> {
    let mut out = "a.out".to_string();
    let mut emit_c = false;
    let mut path: Option<&str> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" => {
                out = it.next().ok_or("`-o` needs an output path")?.clone();
            }
            "--emit-c" => emit_c = true,
            flag if flag.starts_with("--") || flag == "-o" => {
                return Err(format!("unknown flag `{flag}` for build").into());
            }
            positional => {
                if path.replace(positional).is_some() {
                    return Err("more than one input given".into());
                }
            }
        }
    }
    let path = path.ok_or("no input file given")?;
    if path == "-" {
        return Err("build needs a file path, not stdin".into());
    }

    let opts = BuildOptions {
        emit_c,
        ..Default::default()
    };
    match build_file(
        std::path::Path::new(path),
        std::path::Path::new(&out),
        &opts,
    ) {
        Ok(c_path) => {
            eprintln!("built {out}");
            if emit_c {
                eprintln!("C written to {}", c_path.display());
            }
            Ok(())
        }
        Err(e) => Err(CliError::Failed(e.to_string())),
    }
}

fn dump_hir_cmd(args: &[String]) -> Result<(), CliError> {
    let mut path: Option<&str> = None;
    for a in args {
        match a.as_str() {
            flag if flag.starts_with("--") => {
                return Err(format!("unknown flag `{flag}` for dump-hir").into());
            }
            positional => {
                if path.replace(positional).is_some() {
                    return Err("more than one input given".into());
                }
            }
        }
    }
    let src = read_source(path.ok_or("no input file given")?)?;
    let parsed = parse(&src);
    if !parsed.errors.is_empty() {
        for e in &parsed.errors {
            eprintln!(
                "parse error {}..{}: {}",
                e.span.start, e.span.end, e.message
            );
        }
        return Err(CliError::Failed(format!(
            "{} parse error(s); cannot build HIR",
            parsed.errors.len()
        )));
    }
    let (hir, errors) = lower(&parsed.root);
    print!("{}", dump_hir(&hir));
    if !errors.is_empty() {
        eprintln!("\n{} resolution error(s):", errors.len());
        for e in &errors {
            eprintln!("  {}..{}: {}", e.span.start, e.span.end, e.message);
        }
    }
    Ok(())
}

fn dump_ir_cmd(args: &[String]) -> Result<(), CliError> {
    let mut path: Option<&str> = None;
    for a in args {
        match a.as_str() {
            flag if flag.starts_with("--") => {
                return Err(format!("unknown flag `{flag}` for dump-ir").into());
            }
            positional => {
                if path.replace(positional).is_some() {
                    return Err("more than one input given".into());
                }
            }
        }
    }
    let src = read_source(path.ok_or("no input file given")?)?;
    match source_to_ir(&src) {
        Ok(ir) => {
            print!("{}", dump_ir(&ir));
            Ok(())
        }
        Err(e) => Err(CliError::Failed(e.to_string())),
    }
}

fn read_source(path: &str) -> Result<String, String> {
    if path == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("reading stdin: {e}"))?;
        Ok(buf)
    } else {
        std::fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}"))
    }
}
