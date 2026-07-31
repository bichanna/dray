// SPDX-License-Identifier: Apache-2.0

//! Talking to a C compiler.

use std::path::Path;
use std::process::Command;

const C_STANDARD_GNU: &str = "-std=c11";
const C_STANDARD_MSVC: &str = "/std:c11";

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
    /// Directories to search for headers. The build directory goes here, since
    /// that is where `draybase.h` is copied to.
    pub include_dirs: &'a [std::path::PathBuf],
    pub backend: Backend,
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
                cmd.arg(C_STANDARD_GNU);
                for dir in self.include_dirs {
                    cmd.arg(format!("-I{}", dir.display()));
                }
                if !self.show_warnings {
                    cmd.arg("-w");
                }
                cmd.args(self.extra);
                cmd.args(sources).arg("-o").arg(output);
            }
            Backend::Msvc => {
                cmd.arg(C_STANDARD_MSVC);
                for dir in self.include_dirs {
                    cmd.arg(format!("/I{}", dir.display()));
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

    pub fn compile_object(&self, source: &Path, object: &Path) -> Command {
        let mut cmd = Command::new(self.cc);
        match self.backend {
            Backend::Gnu => {
                cmd.arg(C_STANDARD_GNU);
                for dir in self.include_dirs {
                    cmd.arg(format!("-I{}", dir.display()));
                }
                if !self.show_warnings {
                    cmd.arg("-w");
                }
                cmd.args(self.extra);
                cmd.arg("-c").arg(source).arg("-o").arg(object);
            }
            Backend::Msvc => {
                cmd.arg(C_STANDARD_MSVC);
                for dir in self.include_dirs {
                    cmd.arg(format!("/I{}", dir.display()));
                }
                if !self.show_warnings {
                    cmd.arg("/w");
                }
                cmd.args(self.extra);
                cmd.arg("/c")
                    .arg(source)
                    .arg(format!("/Fo:{}", object.display()));
            }
        }
        cmd
    }

    /// Link already compiled object files into the final executable.
    pub fn link_objects(&self, objects: &[std::path::PathBuf], output: &Path) -> Command {
        let mut cmd = Command::new(self.cc);
        match self.backend {
            Backend::Gnu => {
                cmd.args(self.extra);
                cmd.args(objects).arg("-o").arg(output);
            }
            Backend::Msvc => {
                cmd.args(self.extra);
                cmd.arg(format!("/Fe:{}", output.display()));
                cmd.args(objects);
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
            include_dirs: &[],
            backend: Backend::Gnu,
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
            include_dirs: &[],
            backend: Backend::Msvc,
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
            include_dirs: &[],
            backend: Backend::Gnu,
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

    #[test]
    fn compiling_an_object_uses_dash_c_and_does_not_link() {
        let inv = CcInvocation {
            cc: "cc",
            include_dirs: &[],
            backend: Backend::Gnu,
            show_warnings: false,
            extra: &[],
        };
        let args = args_of(&inv.compile_object(Path::new("mod.c"), Path::new("mod.o")));
        assert!(args.contains(&"-c".to_string()), "{args:?}");
        assert!(args.contains(&"mod.o".to_string()), "{args:?}");
    }

    #[test]
    fn linking_objects_omits_the_c_standard_flag() {
        let inv = CcInvocation {
            cc: "cc",
            include_dirs: &[],
            backend: Backend::Gnu,
            show_warnings: false,
            extra: &[],
        };
        let objs = vec![
            Path::new("a.o").to_path_buf(),
            Path::new("b.o").to_path_buf(),
        ];
        let args = args_of(&inv.link_objects(&objs, Path::new("prog")));
        // Linking does not compile, so no `-c` and no `-std=`.
        assert!(!args.contains(&"-c".to_string()), "{args:?}");
        assert!(!args.iter().any(|a| a.starts_with("-std=")), "{args:?}");
        assert!(
            args.contains(&"a.o".to_string()) && args.contains(&"b.o".to_string()),
            "{args:?}"
        );
        assert!(args.contains(&"-o".to_string()), "{args:?}");
    }
}
