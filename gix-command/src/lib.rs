//! Launch commands very similarly to `Command`, but with `git` specific capabilities and adjustments.
//!
//! ## Examples
//!
//! ```
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let output = gix_command::prepare("git")
//!     .arg("--version")
//!     .spawn()?
//!     .wait_with_output()?;
//!
//! assert!(output.status.success());
//! assert!(String::from_utf8(output.stdout)?.starts_with("git version "));
//! # Ok(()) }
//! ```
#![deny(missing_docs)]
#![forbid(unsafe_code)]

use std::{
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
};

use bstr::{BString, ByteSlice};

///
pub mod parse;

mod prepare;

///
pub mod shebang {
    use std::{ffi::OsString, path::PathBuf};

    use bstr::{BStr, ByteSlice};

    /// Parse `buf` to extract all shebang information.
    pub fn parse(buf: &BStr) -> Option<Data> {
        let mut line = buf.lines().next()?;
        line = line.strip_prefix(b"#!")?;

        let slash_idx = line.rfind_byteset(br"/\")?;
        if let Some(space_idx) = line[slash_idx..].find_byte(b' ') {
            line = &line[..slash_idx + space_idx];
        }
        Some(Data {
            interpreter: gix_path::try_from_byte_slice(line.trim()).ok()?.to_owned(),
            args: Vec::new(),
        })
    }

    /// Shebang information as [parsed](parse()) from a buffer that should contain at least one line.
    #[derive(Debug, Clone, PartialEq, Eq, Ord, PartialOrd, Hash)]
    pub struct Data {
        /// The interpreter to run.
        pub interpreter: PathBuf,
        /// Reserved for compatibility and always empty, as Git for Windows ignores interpreter options.
        pub args: Vec<OsString>,
    }
}

/// A structure to keep settings to use when invoking a command via [`spawn()`][Prepare::spawn()],
/// after creating it with [`prepare()`].
pub struct Prepare {
    /// The command to invoke, either directly or with a shell depending on `use_shell`.
    pub command: OsString,
    /// Additional information to be passed to the spawned command.
    pub context: Option<Context>,
    /// The way standard input is configured.
    pub stdin: std::process::Stdio,
    /// The way standard output is configured.
    pub stdout: std::process::Stdio,
    /// The way standard error is configured.
    pub stderr: std::process::Stdio,
    /// The arguments to pass to the process being spawned.
    pub args: Vec<OsString>,
    /// Environment variables to set for the spawned process.
    pub env: Vec<(OsString, OsString)>,
    /// If `true`, we will use `shell_program` or `sh` to execute the `command`.
    pub use_shell: bool,
    /// If `true`, `command` is assumed to be a command or path to the program to execute, and it
    /// will be shell-quoted to assure it will be executed as is and without splitting across
    /// whitespace.
    pub quote_command: bool,
    /// The name or path to the shell program to use instead of `sh`.
    pub shell_program: Option<OsString>,
    /// If `true` (default `true` on Windows and `false` everywhere else) we will see if it's safe
    /// to manually invoke `command` after splitting its arguments as a shell would do.
    ///
    /// Note that outside of Windows, it's generally not advisable as this removes support for
    /// literal shell scripts with shell-builtins.
    ///
    /// This mimics the behaviour we see with `git` on Windows, which also won't invoke the shell
    /// there at all.
    ///
    /// Only effective if `use_shell` is `true` as well, as the shell will be used as a fallback if
    /// it's not possible to split arguments as the command-line contains 'scripting'.
    pub allow_manual_arg_splitting: bool,
}

/// Additional information that is relevant to spawned processes, which typically receive
/// a wealth of contextual information when spawned from `git`.
///
/// See [the git source code](https://github.com/git/git/blob/cfb8a6e9a93adbe81efca66e6110c9b4d2e57169/git.c#L191)
/// for details.
#[derive(Debug, Default, Clone)]
pub struct Context {
    /// The `.git` directory that contains the repository.
    ///
    /// If set, it will be used to set the `GIT_DIR` environment variable.
    pub git_dir: Option<PathBuf>,
    /// Set the `GIT_WORK_TREE` environment variable with the given path.
    pub worktree_dir: Option<PathBuf>,
    /// If `true`, set `GIT_NO_REPLACE_OBJECTS` to `1`, which turns off object replacements, or `0` otherwise.
    /// If `None`, the variable won't be set.
    pub no_replace_objects: Option<bool>,
    /// Set the `GIT_NAMESPACE` variable with the given value, effectively namespacing all
    /// operations on references.
    pub ref_namespace: Option<BString>,
    /// If `true`, set `GIT_LITERAL_PATHSPECS` to `1`, which makes globs literal and prefixes as well, or `0` otherwise.
    /// If `None`, the variable won't be set.
    pub literal_pathspecs: Option<bool>,
    /// If `true`, set `GIT_GLOB_PATHSPECS` to `1`, which lets wildcards not match the `/` character, and equals the `:(glob)` prefix.
    /// If `false`, set `GIT_NOGLOB_PATHSPECS` to `1` which lets globs match only themselves.
    /// If `None`, the variable won't be set.
    pub glob_pathspecs: Option<bool>,
    /// If `true`, set `GIT_ICASE_PATHSPECS` to `1`, to let patterns match case-insensitively, or `0` otherwise.
    /// If `None`, the variable won't be set.
    pub icase_pathspecs: Option<bool>,
    /// If `true`, inherit `stderr` just like it's the default when spawning processes.
    /// If `false`, suppress all stderr output.
    /// If not `None`, this will override any value set with [`Prepare::stderr()`].
    pub stderr: Option<bool>,
}

fn is_exe(executable: &Path) -> bool {
    executable.extension() == Some(std::ffi::OsStr::new("exe"))
}

/// Try to find `command` in the `path_value` (the value of `PATH`) as separated by `;`, or return `None`.
/// Has special handling for `.exe` extensions, as these will be appended automatically if needed.
/// Note that just like Git, no lookup is performed if a slash or backslash is in `command`.
fn win_path_lookup(command: &Path, path_value: &std::ffi::OsStr) -> Option<PathBuf> {
    fn lookup(root: &bstr::BStr, command: &Path, is_exe: bool) -> Option<PathBuf> {
        let mut path = gix_path::try_from_bstr(root).ok()?.join(command);
        if !is_exe {
            path.set_extension("exe");
        }
        if path.is_file() {
            return Some(path);
        }
        if is_exe {
            return None;
        }
        path.set_extension("");
        path.is_file().then_some(path)
    }
    if command.components().take(2).count() == 2 {
        return None;
    }
    let path = gix_path::os_str_into_bstr(path_value).ok()?;
    let is_exe = is_exe(command);

    for root in path.split(|b| *b == b';') {
        if let Some(executable) = lookup(root.as_bstr(), command, is_exe) {
            return Some(executable);
        }
    }
    None
}

/// Parse the shebang (`#!<path>`) from the first line of `executable`, and return the shebang
/// data when available.
pub fn extract_interpreter(executable: &Path) -> Option<shebang::Data> {
    #[cfg(windows)]
    if is_exe(executable) {
        return None;
    }
    let mut buf = [0; 100]; // Note: just like Git
    let mut file = std::fs::File::open(executable).ok()?;
    let n = file.read(&mut buf).ok()?;
    shebang::parse(buf[..n].as_bstr())
}

/// Prepare `cmd` for [spawning][std::process::Command::spawn()] by configuring it with various builder methods.
///
/// Note that the default IO is configured for typical API usage, that is
///
/// - `stdin` is null to prevent blocking unexpectedly on consumption of stdin
/// - `stdout` is captured for consumption by the caller
/// - `stderr` is inherited to allow the command to provide context to the user
///
/// On Windows, terminal Windows will be suppressed automatically.
///
/// ### Warning
///
/// When using this method, be sure that the invoked program doesn't rely on the current working dir and/or
/// environment variables to know its context. If so, call instead [`Prepare::with_context()`] to provide
/// additional information.
pub fn prepare(cmd: impl Into<OsString>) -> Prepare {
    Prepare {
        command: cmd.into(),
        shell_program: None,
        context: None,
        stdin: std::process::Stdio::null(),
        stdout: std::process::Stdio::piped(),
        stderr: std::process::Stdio::inherit(),
        args: Vec::new(),
        env: Vec::new(),
        use_shell: false,
        quote_command: false,
        allow_manual_arg_splitting: cfg!(windows),
    }
}

#[cfg(test)]
mod tests;
