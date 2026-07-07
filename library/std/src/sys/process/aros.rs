//! Processes for AROS, over dos `SystemTagList` (via the `aros_system` glue).
//!
//! AROS has no fork/exec: the shell runs a command *line*. So `Command` is turned
//! into a properly-quoted line and handed to `SystemTagList`, which runs it
//! synchronously and returns the command's exit code.
//!
//! Scope (honest caveats): `output()` captures stdout/stderr by redirecting the
//! child to `T:` temp files and reading them back (uses the `fs` pal); `spawn()` runs
//! synchronously with inherited stdio, so `status()` works but there are no live
//! pipes and no true background child (a `spawn()`d process has already finished when
//! the handle returns). `cwd`, per-command `env`, `kill`, and piped/`Null`/file stdio
//! on `spawn` are not wired yet. Enough for "run a `C:` command, capture its output,
//! read its exit code".

use super::env::{CommandEnv, CommandEnvs, CommandResolvedEnvs};
pub use crate::ffi::OsString as EnvKey;
use crate::ffi::{CString, OsStr, OsString, c_char, c_long};
use crate::num::NonZero;
use crate::path::Path;
use crate::process::StdioPipes;
use crate::sync::atomic::{AtomicU32, Ordering};
use crate::sys::fs::File;
use crate::sys::unsupported;
use crate::{fmt, io};

unsafe extern "C" {
    fn aros_system(cmdline: *const c_char, out_path: *const c_char, err_path: *const c_char) -> c_long;
}

////////////////////////////////////////////////////////////////////////////////
// Command
////////////////////////////////////////////////////////////////////////////////

pub struct Command {
    program: OsString,
    args: Vec<OsString>,
    env: CommandEnv,

    cwd: Option<OsString>,
    stdin: Option<Stdio>,
    stdout: Option<Stdio>,
    stderr: Option<Stdio>,
}

#[derive(Debug)]
pub enum Stdio {
    Inherit,
    Null,
    MakePipe,
    ParentStdout,
    ParentStderr,
    #[allow(dead_code)] // This variant exists only for the Debug impl
    InheritFile(File),
}

impl Command {
    pub fn new(program: &OsStr) -> Command {
        Command {
            program: program.to_owned(),
            args: vec![program.to_owned()],
            env: Default::default(),
            cwd: None,
            stdin: None,
            stdout: None,
            stderr: None,
        }
    }

    pub fn arg(&mut self, arg: &OsStr) {
        self.args.push(arg.to_owned());
    }

    pub fn env_mut(&mut self) -> &mut CommandEnv {
        &mut self.env
    }

    pub fn cwd(&mut self, dir: &OsStr) {
        self.cwd = Some(dir.to_owned());
    }

    pub fn stdin(&mut self, stdin: Stdio) {
        self.stdin = Some(stdin);
    }

    pub fn stdout(&mut self, stdout: Stdio) {
        self.stdout = Some(stdout);
    }

    pub fn stderr(&mut self, stderr: Stdio) {
        self.stderr = Some(stderr);
    }

    pub fn get_program(&self) -> &OsStr {
        &self.program
    }

    pub fn get_args(&self) -> CommandArgs<'_> {
        let mut iter = self.args.iter();
        iter.next();
        CommandArgs { iter }
    }

    pub fn get_envs(&self) -> CommandEnvs<'_> {
        self.env.iter()
    }

    pub fn get_env_clear(&self) -> bool {
        self.env.does_clear()
    }

    pub fn get_resolved_envs(&self) -> CommandResolvedEnvs {
        CommandResolvedEnvs::new(self.env.capture())
    }

    pub fn get_current_dir(&self) -> Option<&Path> {
        self.cwd.as_ref().map(|cwd| Path::new(cwd))
    }

    /// Build the shell command line: the program name followed by args[1..], each
    /// quoted for the AROS shell (escape char `*`).
    fn command_line(&self) -> String {
        let mut line = String::new();
        quote_into(&self.program, &mut line);
        for arg in &self.args[1..] {
            line.push(' ');
            quote_into(arg, &mut line);
        }
        line
    }

    pub fn spawn(
        &mut self,
        _default: Stdio,
        _needs_stdin: bool,
    ) -> io::Result<(Process, StdioPipes)> {
        // synchronous, inherited stdio -> the command has finished when we return
        let line = cstr(&self.command_line())?;
        let rc = unsafe { aros_system(line.as_ptr(), core::ptr::null(), core::ptr::null()) };
        // SystemTagList's -1 means "the command line could not be run at all" (no
        // shell, unloadable binary): that is spawn FAILURE, not an exit status.
        if rc == -1 {
            return Err(io::const_error!(io::ErrorKind::NotFound, "command could not be run"));
        }
        Ok((
            Process { code: rc as i32 },
            StdioPipes { stdin: None, stdout: None, stderr: None },
        ))
    }
}

/// Append `arg` to `out`, quoting for the AROS shell when needed. The shell's escape
/// char is `*`: inside double quotes `*"` is a literal quote, `**` a literal star,
/// `*n` a newline.
fn quote_into(arg: &OsStr, out: &mut String) {
    let s = arg.to_string_lossy();
    let needs = s.is_empty()
        || s.chars().any(|c| c == ' ' || c == '"' || c == '*' || c == '\t' || c == '\n');
    if !needs {
        out.push_str(&s);
        return;
    }
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("*\""),
            '*' => out.push_str("**"),
            '\n' => out.push_str("*n"),
            _ => out.push(c),
        }
    }
    out.push('"');
}

fn cstr(s: &str) -> io::Result<CString> {
    CString::new(s).map_err(|_| io::const_error!(io::ErrorKind::InvalidInput, "command contains a NUL byte"))
}

/// Run `cmd`, capturing stdout+stderr via `T:` temp files (read back through the fs
/// pal). This is `Command::output()`.
pub fn output(cmd: &mut Command) -> io::Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    // Capture via temp files. `T:` isn't guaranteed assigned on a minimal boot (it
    // pops a blocking volume requester), so use the host-backed `MacRW:` share, which
    // exists whenever the emul-handler is up. The glue also suppresses requesters, so
    // a missing volume fails fast instead of hanging.
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let out_path = format!("MacRW:rustproc-{n}.out");
    let err_path = format!("MacRW:rustproc-{n}.err");

    let line = cstr(&cmd.command_line())?;
    let out_c = cstr(&out_path)?;
    let err_c = cstr(&err_path)?;

    let rc = unsafe { aros_system(line.as_ptr(), out_c.as_ptr(), err_c.as_ptr()) };

    let stdout = slurp(&out_path);
    let stderr = slurp(&err_path);
    let _ = crate::sys::fs::remove_file(Path::new(&out_path));
    let _ = crate::sys::fs::remove_file(Path::new(&err_path));

    // Same contract as spawn: SystemTagList's -1 is "could not run", an Err.
    if rc == -1 {
        return Err(io::const_error!(io::ErrorKind::NotFound, "command could not be run"));
    }
    Ok((ExitStatus(rc as i32), stdout, stderr))
}

/// Read a whole file into a Vec via the fs pal (best-effort; empty on failure).
fn slurp(path: &str) -> Vec<u8> {
    let mut opts = crate::sys::fs::OpenOptions::new();
    opts.read(true);
    let mut out = Vec::new();
    if let Ok(f) = File::open(Path::new(path), &opts) {
        let mut buf = [0u8; 4096];
        loop {
            match f.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
    }
    out
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.program != self.args[0] {
            write!(f, "[{:?}] ", self.program)?;
        }
        write!(f, "{:?}", self.args[0])?;
        for arg in &self.args[1..] {
            write!(f, " {:?}", arg)?;
        }
        Ok(())
    }
}

////////////////////////////////////////////////////////////////////////////////
// ExitStatus / ExitStatusError / ExitCode
////////////////////////////////////////////////////////////////////////////////

#[derive(PartialEq, Eq, Clone, Copy, Debug, Default)]
pub struct ExitStatus(i32);

impl ExitStatus {
    pub fn exit_ok(&self) -> Result<(), ExitStatusError> {
        match NonZero::new(self.0) {
            None => Ok(()),
            Some(nz) => Err(ExitStatusError(nz)),
        }
    }

    pub fn code(&self) -> Option<i32> {
        Some(self.0)
    }
}

impl fmt::Display for ExitStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "exit code: {}", self.0)
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub struct ExitStatusError(NonZero<i32>);

impl Into<ExitStatus> for ExitStatusError {
    fn into(self) -> ExitStatus {
        ExitStatus(self.0.get())
    }
}

impl ExitStatusError {
    pub fn code(self) -> Option<NonZero<i32>> {
        Some(self.0)
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub struct ExitCode(u8);

impl ExitCode {
    pub const SUCCESS: ExitCode = ExitCode(0);
    pub const FAILURE: ExitCode = ExitCode(1);

    pub fn as_i32(&self) -> i32 {
        self.0 as i32
    }
}

impl From<u8> for ExitCode {
    fn from(code: u8) -> Self {
        Self(code)
    }
}

////////////////////////////////////////////////////////////////////////////////
// Process
////////////////////////////////////////////////////////////////////////////////

pub struct Process {
    code: i32,
}

impl Process {
    pub fn id(&self) -> u32 {
        // No pid model on the synchronous shell path.
        0
    }

    pub fn kill(&mut self) -> io::Result<()> {
        // The command already finished (synchronous spawn).
        Ok(())
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        Ok(ExitStatus(self.code))
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Ok(Some(ExitStatus(self.code)))
    }
}

////////////////////////////////////////////////////////////////////////////////
// misc
////////////////////////////////////////////////////////////////////////////////

impl From<File> for Stdio {
    fn from(file: File) -> Stdio {
        Stdio::InheritFile(file)
    }
}

impl From<io::Stdout> for Stdio {
    fn from(_: io::Stdout) -> Stdio {
        Stdio::ParentStdout
    }
}

impl From<io::Stderr> for Stdio {
    fn from(_: io::Stderr) -> Stdio {
        Stdio::ParentStderr
    }
}

impl From<ChildPipe> for Stdio {
    fn from(pipe: ChildPipe) -> Stdio {
        pipe.diverge()
    }
}

pub struct CommandArgs<'a> {
    iter: crate::slice::Iter<'a, OsString>,
}

impl<'a> Iterator for CommandArgs<'a> {
    type Item = &'a OsStr;
    fn next(&mut self) -> Option<&'a OsStr> {
        self.iter.next().map(|os| &**os)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl<'a> ExactSizeIterator for CommandArgs<'a> {
    fn len(&self) -> usize {
        self.iter.len()
    }
    fn is_empty(&self) -> bool {
        self.iter.is_empty()
    }
}

impl<'a> fmt::Debug for CommandArgs<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter.clone()).finish()
    }
}

pub type ChildPipe = crate::sys::pipe::Pipe;

pub fn read_output(
    out: ChildPipe,
    _stdout: &mut Vec<u8>,
    _err: ChildPipe,
    _stderr: &mut Vec<u8>,
) -> io::Result<()> {
    // We never hand back live pipes, so this is unreachable.
    match out.diverge() {}
}

pub fn getpid() -> u32 {
    1
}
