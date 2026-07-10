// SPDX-License-Identifier: Apache-2.0

//! The `dray` command-line entry point.

use std::io::Read;
use std::process::ExitCode;

use dray_driver::{BuildOptions, build_file, source_to_c};
use dray_syntax::{DumpOptions, dump_cst_with, dump_tokens, dump_tokens_no_trivia, parse};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("dray: {msg}");
            eprintln!();
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "\
usage:
  dray dump-tokens [--no-trivia] <file>
  dray dump-cst    [--trivia] [--no-spans] [--shape] <file>
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

fn run(args: &[String]) -> Result<(), String> {
    let cmd = args.first().ok_or("expected a subcommand")?;
    match cmd.as_str() {
        "dump-tokens" => dump_tokens_cmd(&args[1..]),
        "dump-cst" => dump_cst_cmd(&args[1..]),
        "emit-c" => emit_c_cmd(&args[1..]),
        "build" => build_cmd(&args[1..]),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown subcommand `{other}`")),
    }
}

fn dump_tokens_cmd(args: &[String]) -> Result<(), String> {
    let mut no_trivia = false;
    let mut path: Option<&str> = None;
    for a in args {
        match a.as_str() {
            "--no-trivia" => no_trivia = true,
            flag if flag.starts_with("--") => {
                return Err(format!("unknown flag `{flag}` for dump-tokens"));
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

fn dump_cst_cmd(args: &[String]) -> Result<(), String> {
    let mut opts = DumpOptions::default();
    let mut shape = false;
    let mut path: Option<&str> = None;
    for a in args {
        match a.as_str() {
            "--trivia" => opts = opts.with_trivia(true),
            "--no-spans" => opts = opts.with_spans(false),
            "--shape" => shape = true,
            flag if flag.starts_with("--") => {
                return Err(format!("unknown flag `{flag}` for dump-cst"));
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

fn emit_c_cmd(args: &[String]) -> Result<(), String> {
    let mut path: Option<&str> = None;
    for a in args {
        match a.as_str() {
            flag if flag.starts_with("--") => {
                return Err(format!("unknown flag `{flag}` for emit-c"));
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
        Err(e) => Err(e.to_string()),
    }
}

fn build_cmd(args: &[String]) -> Result<(), String> {
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
                return Err(format!("unknown flag `{flag}` for build"));
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
        Err(e) => Err(e.to_string()),
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
