// SPDX-License-Identifier: Apache-2.0

//! Talking to a C compiler.

use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CStandard {
    C99,
    #[default]
    C11,
    C17,
}

impl CStandard {
    /// The spelling used by gcc and clang.
    fn gnu_flag(self) -> &'static str {
        match self {
            CStandard::C99 => "-std=c99",
            CStandard::C11 => "-std=c11",
            CStandard::C17 => "-std=c17",
        }
    }

    fn msvc_flag(self) -> Option<&'static str> {
        match self {
            CStandard::C99 => None,
            CStandard::C11 => Some("/std:c11"),
            CStandard::C17 => Some("/std:c17"),
        }
    }

    pub fn parse(text: &str) -> Option<CStandard> {
        match text {
            "c99" | "C99" => Some(CStandard::C99),
            "c11" | "C11" => Some(CStandard::C11),
            "c17" | "c18" | "C17" | "C18" => Some(CStandard::C17),
            _ => None,
        }
    }
}

/// Which flag dialect a C compiler speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Gnu,
    Msvc,
}

impl Backend {
    pub fn detect(cc: &str) -> Backend {
        let file = cc.rsplit(['/', '\\']).next().unwrap_or(cc);
        let name = file
            .strip_suffix(".exe")
            .unwrap_or(file)
            .to_ascii_lowercase();
        if name == "cl" || name == "clang-cl" {
            Backend::Msvc
        } else {
            Backend::Gnu
        }
    }

    pub fn exe_suffix(self) -> &'static str {
        match self {
            Backend::Msvc => ".exe",
            Backend::Gnu if cfg!(windows) => ".exe",
            Backend::Gnu => "",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CcInvocation<'a> {
    pub cc: &'a str,
    pub backend: Backend,
    pub standard: CStandard,
    pub show_warnings: bool,
    pub extra: &'a [String],
}

impl CcInvocation<'_> {
    pub fn command(&self, source: &Path, output: &Path) -> Command {
        self.command_multi(std::slice::from_ref(&source.to_path_buf()), output)
    }

    pub fn command_multi(&self, sources: &[std::path::PathBuf], output: &Path) -> Command {
        let mut cmd = Command::new(self.cc);
        match self.backend {
            Backend::Gnu => {
                cmd.arg(self.standard.gnu_flag());
                if !self.show_warnings {
                    cmd.arg("-w");
                }
                cmd.args(self.extra);
                cmd.args(sources).arg("-o").arg(output);
            }
            Backend::Msvc => {
                if let Some(flag) = self.standard.msvc_flag() {
                    cmd.arg(flag);
                }
                if !self.show_warnings {
                    cmd.arg("/w");
                }
                cmd.args(self.extra);
                cmd.arg(format!("/Fe:{}", output.display()));
                cmd.args(sources);
            }
        }
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_gnu_compiler_uses_dash_o_and_std() {
        let inv = CcInvocation {
            cc: "clang",
            backend: Backend::Gnu,
            standard: CStandard::C11,
            show_warnings: false,
            extra: &[],
        };
        let args = args_of(&inv.command(Path::new("a.c"), Path::new("a.out")));
        assert!(args.contains(&"-std=c11".to_string()), "{args:?}");
        assert!(args.contains(&"-o".to_string()), "{args:?}");
        assert!(args.contains(&"-w".to_string()), "{args:?}");
    }

    #[test]
    fn msvc_uses_its_own_spellings() {
        let inv = CcInvocation {
            cc: "cl",
            backend: Backend::Msvc,
            standard: CStandard::C11,
            show_warnings: false,
            extra: &[],
        };
        let args = args_of(&inv.command(Path::new("a.c"), Path::new("a.exe")));
        assert!(args.contains(&"/std:c11".to_string()), "{args:?}");
        assert!(args.iter().any(|a| a.starts_with("/Fe:")), "{args:?}");
        assert!(!args.contains(&"-o".to_string()), "{args:?}");
    }

    #[test]
    fn warnings_are_shown_only_when_asked_for() {
        let inv = CcInvocation {
            cc: "gcc",
            backend: Backend::Gnu,
            standard: CStandard::C11,
            show_warnings: true,
            extra: &[],
        };
        let args = args_of(&inv.command(Path::new("a.c"), Path::new("a.out")));
        assert!(!args.contains(&"-w".to_string()), "{args:?}");
    }

    #[test]
    fn the_backend_is_guessed_from_the_program_name() {
        assert_eq!(Backend::detect("cc"), Backend::Gnu);
        assert_eq!(Backend::detect("clang"), Backend::Gnu);
        assert_eq!(Backend::detect("/usr/bin/gcc-13"), Backend::Gnu);
        assert_eq!(Backend::detect("cl"), Backend::Msvc);
        assert_eq!(Backend::detect(r"C:\VC\bin\cl.exe"), Backend::Msvc);
    }
}
