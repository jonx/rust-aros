//! Processes for AROS, over dos `SystemTagList` (via the `aros_proc_*` glue).
//!
//! AROS has no fork/exec: a shell runs a command *line*. So `Command` becomes a
//! properly-quoted line handed to `SystemTagList`, which starts a shell to run
//! it. The child is asynchronous and its streams are live `PIPE:` endpoints, so
//! `spawn()` gives a real running child with a readable `stdout` and a writable
//! `stdin`, and its exit is signalled rather than polled (`sys/pipe/aros.rs`,
//! `aros_proc_glue.c`).
//!
//! Two shapes of child, and the difference matters:
//!
//! - **A command.** The shell runs the line and exits. This is what
//!   `Command::new("C:List")` means.
//! - **No command at all** (an empty program name). AROS starts a new CLI that
//!   goes on reading its input, which is the only way to get an interactive
//!   shell on a given pair of streams -- what a terminal needs. `System`'s
//!   default is the first kind, so the glue asks for the second explicitly.
//!
//! **`cwd` and per-command `env` ARE honoured**, by different routes. `cwd` is
//! handed to the child as its own directory, so nothing is typed at it. `env`
//! has no such route -- a `System` child does not inherit the caller's local
//! variables -- so it becomes a generated `Set` preamble: a one-off script run
//! with `Execute` for a command, or written into the child's stdin for an
//! interactive shell, `Execute` handing the shell a file and the shell being
//! finished when the file is.
//!
//! Remaining gaps: an interactive shell with no pipe on its stdin has nowhere
//! to receive its `env`, so it is not applied; `env_clear()` cannot be fully
//! honoured (the local-var set cannot be enumerated to blank it); and `kill` is
//! a no-op, AROS having no way to stop another process.

use super::env::{CommandEnv, CommandEnvs, CommandResolvedEnvs};
pub use crate::ffi::OsString as EnvKey;
use crate::ffi::{CString, OsStr, OsString, c_char, c_long};
use crate::num::NonZero;
use crate::path::{Path, PathBuf};
use crate::process::StdioPipes;
use crate::sync::atomic::{AtomicU32, Ordering};
use crate::sys::fs::File;
use crate::sys::unsupported;
use crate::{fmt, io};

unsafe extern "C" {
    fn aros_system(cmdline: *const c_char, out_path: *const c_char, err_path: *const c_char) -> c_long;

    // Streaming spawn (aros_proc_glue.c). Dispositions are APS_* below.
    fn aros_proc_spawn(
        cmdline: *const c_char,
        in_mode: i32,
        out_mode: i32,
        err_mode: i32,
        interactive: i32,
        cwd: *const c_char,
        p_in: *mut isize,
        p_out: *mut isize,
        p_err: *mut isize,
    ) -> *mut crate::ffi::c_void;
    /// Register (or clear, with a null task) who to wake when the child exits.
    /// Returns non-zero if it has already exited.
    fn aros_proc_set_waiter(handle: *mut crate::ffi::c_void, task: *mut crate::ffi::c_void, sigmask: u32) -> i32;
    fn aros_task_self() -> *mut crate::ffi::c_void;
    fn aros_proc_exited(handle: *mut crate::ffi::c_void, code: *mut i32) -> i32;
    fn aros_proc_free(handle: *mut crate::ffi::c_void);
    /// Why the last spawn failed, plus the dos error that came with it.
    fn aros_proc_last_fail(ioerr: *mut c_long, step: *mut c_long) -> c_long;

    // exec signal plumbing, for waiting on the child without polling
    fn aros_sig_alloc() -> i32;
    fn aros_sig_free(bit: i32);
    fn aros_sig_wait(mask: u32) -> u32;
}

/// Turn the glue's reason for a failed spawn into something a caller can read.
fn spawn_error() -> io::Error {
    let (mut ioerr, mut step): (c_long, c_long) = (0, 0);
    let why = unsafe { aros_proc_last_fail(&mut ioerr, &mut step) };
    let stream = match why {
        2 => "stdin",
        3 => "stdout",
        _ => "stderr",
    };
    let end = if step == 1 { "writing" } else { "reading" };
    match why {
        1 => io::Error::new(io::ErrorKind::OutOfMemory, "no memory to track the child"),
        2..=4 => io::Error::new(
            io::ErrorKind::BrokenPipe,
            format!("cannot open the {end} end of the child's {stream} pipe (dos error {ioerr})"),
        ),
        5 => io::Error::new(io::ErrorKind::NotFound, "cannot open NIL:"),
        8 => io::Error::new(
            io::ErrorKind::NotFound,
            format!("cannot reach the working directory (dos error {ioerr})"),
        ),
        6 => io::Error::new(
            io::ErrorKind::Other,
            format!("the shell could not be started (dos error {ioerr})"),
        ),
        _ => io::Error::new(io::ErrorKind::NotFound, "command could not be run"),
    }
}

const APS_INHERIT: i32 = 0;
const APS_PIPE: i32 = 1;
const APS_NULL: i32 = 2;

fn disposition(s: Option<&Stdio>) -> i32 {
    match s {
        Some(Stdio::MakePipe) => APS_PIPE,
        Some(Stdio::Null) => APS_NULL,
        // A file or a parent stream is inherited: the child writes straight to
        // it, which is what the caller asked for even though we cannot rebind
        // an arbitrary File onto the child here.
        _ => APS_INHERIT,
    }
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
    ///
    /// An empty program name means no command at all, not a command whose name
    /// is the empty string. AROS reads that as "start a shell and let it read
    /// its input", which is the only way to ask for an interactive shell on a
    /// given pair of streams -- what a terminal needs, and what quoting the
    /// empty string into `""` would break.
    fn command_line(&self) -> String {
        let mut line = String::new();
        if !self.program.is_empty() {
            quote_into(&self.program, &mut line);
        }
        for arg in &self.args[1..] {
            if !line.is_empty() {
                line.push(' ');
            }
            quote_into(arg, &mut line);
        }
        line
    }

    /// The `Set` / `Unset` lines that apply `env` in the child shell, one per
    /// line, each already newline-terminated.
    ///
    /// `cwd` is not here: it is handed to the child as its own directory, which
    /// a shell command could not be without also being read, run and prompted
    /// for like anything else typed at an interactive shell.
    fn setup_lines(&self) -> String {
        let mut setup = String::new();
        for (k, v) in self.env.iter() {
            match v {
                Some(val) => {
                    setup.push_str("Set ");
                    quote_into(k, &mut setup);
                    setup.push(' ');
                    quote_into(val, &mut setup);
                }
                None => {
                    setup.push_str("Unset ");
                    quote_into(k, &mut setup);
                }
            }
            setup.push('\n');
        }
        setup
    }

    /// Resolve the command into the actual line to hand `SystemTagList`, applying
    /// `cwd` and per-command `env`. With neither set, that's just the quoted command
    /// line. With either set, AROS's no-fork `System` can't apply them (a child
    /// inherits neither the caller's current dir vars nor its local variables), so we
    /// write a one-off script that `CD`s / `Set`s in the child shell and then runs the
    /// command, and return `Execute <script>` plus the temp script path to delete.
    fn resolve_line(&self) -> io::Result<(String, Option<PathBuf>)> {
        let has_env = self.env.iter().next().is_some();
        if !has_env {
            return Ok((self.command_line(), None));
        }

        // A shell with no command is an interactive one, and it has to be left
        // reading its input. Neither way of carrying the setup on the command
        // line survives that: `Execute` hands the shell a file and the shell is
        // finished when the file is, and injecting the setup as the command
        // line makes it a shell that runs a command and exits. Either way the
        // session ends immediately. `spawn` writes the setup into the child's
        // stdin instead, where it is just the first thing typed.
        if self.command_line().is_empty() {
            return Ok((String::new(), None));
        }

        let mut script = String::new();
        if let Some(dir) = &self.cwd {
            // The script runs in a sub-shell of the child, so it has to CD
            // there itself even though the child already starts there.
            script.push_str("CD ");
            quote_into(dir, &mut script);
            script.push('\n');
        }
        script.push_str(&self.setup_lines());
        script.push_str(&self.command_line());
        script.push('\n');

        static SCRIPT_SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SCRIPT_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = format!("MacRW:rustcmd-{n}.aros");
        write_script(&path, script.as_bytes())?;
        Ok((format!("Execute {path}"), Some(PathBuf::from(path))))
    }

    pub fn spawn(
        &mut self,
        default: Stdio,
        needs_stdin: bool,
    ) -> io::Result<(Process, StdioPipes)> {
        let (line, script) = self.resolve_line()?;
        let line_c = cstr(&line)?;

        // When the caller does not need to write to the child (`output()`,
        // `status()`), an unset stdin is NULL rather than inherited: leaving it
        // on the console makes a child that reads stdin block forever.
        let default_stdin = if needs_stdin { &default } else { &Stdio::Null };
        let in_mode = disposition(Some(self.stdin.as_ref().unwrap_or(default_stdin)));
        let out_mode = disposition(Some(self.stdout.as_ref().unwrap_or(&default)));
        let err_mode = disposition(Some(self.stderr.as_ref().unwrap_or(&default)));

        // A shell with no command line is a terminal's shell: it has to keep
        // reading its input rather than run one command and exit.
        let interactive = line.is_empty();

        let cwd_c = match &self.cwd {
            Some(dir) => Some(cstr(&dir.to_string_lossy())?),
            None => None,
        };
        let cwd_ptr = cwd_c.as_ref().map_or(crate::ptr::null(), |c| c.as_ptr());

        let (mut h_in, mut h_out, mut h_err) = (0isize, 0isize, 0isize);
        let handle = unsafe {
            aros_proc_spawn(
                line_c.as_ptr(),
                in_mode,
                out_mode,
                err_mode,
                interactive as i32,
                cwd_ptr,
                &mut h_in,
                &mut h_out,
                &mut h_err,
            )
        };

        // The script is only needed while the shell reads it; the child has it
        // open by now, but on AROS a running Execute re-reads the file, so keep
        // it until the child is reaped instead of deleting it here.
        if handle.is_null() {
            if let Some(p) = script {
                let _ = crate::sys::fs::remove_file(&p);
            }
            return Err(spawn_error());
        }

        let pipes = StdioPipes {
            stdin: (h_in != 0).then(|| ChildPipe::from_handle(h_in)),
            stdout: (h_out != 0).then(|| ChildPipe::from_handle(h_out)),
            stderr: (h_err != 0).then(|| ChildPipe::from_handle(h_err)),
        };

        // See `resolve_line`: an interactive shell takes its env as the first
        // thing it reads. Without a pipe to its stdin there is nowhere to put
        // it, and it is silently not applied -- the same as asking for an
        // interactive shell with nothing to type at it.
        if line.is_empty() {
            if let Some(stdin) = &pipes.stdin {
                let setup = self.setup_lines();
                if !setup.is_empty() {
                    stdin.write(setup.as_bytes())?;
                }
            }
        }
        Ok((Process { handle, status: None, script }, pipes))
    }
}

/// Write a generated shell script through the fs pal (best path for `MacRW:`).
fn write_script(path: &str, data: &[u8]) -> io::Result<()> {
    let mut opts = crate::sys::fs::OpenOptions::new();
    opts.write(true);
    opts.create(true);
    opts.truncate(true);
    let f = File::open(Path::new(path), &opts)?;
    let mut off = 0;
    while off < data.len() {
        match f.write(&data[off..])? {
            0 => return Err(io::const_error!(io::ErrorKind::WriteZero, "short script write")),
            n => off += n,
        }
    }
    Ok(())
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
    // Runs on the same streaming path as `spawn`, so a child that outlives its
    // output buffer (or writes more than a pipe holds) still completes, and the
    // temp-file dance is gone. `needs_stdin = false` gives the child NIL: on
    // stdin: inheriting the console here made any child that reads stdin block
    // forever waiting for a terminal nobody was typing at.
    let (mut proc, pipes) = cmd.spawn(Stdio::MakePipe, false)?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    match (pipes.stdout, pipes.stderr) {
        (Some(out), Some(err)) => read_output(out, &mut stdout, err, &mut stderr)?,
        (Some(out), None) => {
            out.read_to_end(&mut stdout)?;
        }
        (None, Some(err)) => {
            err.read_to_end(&mut stderr)?;
        }
        (None, None) => {}
    }
    let status = proc.wait()?;
    Ok((status, stdout, stderr))
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
    handle: *mut crate::ffi::c_void,
    status: Option<ExitStatus>,
    /// The generated cwd/env script, removed once the child is reaped.
    script: Option<PathBuf>,
}

// The handle is a plain allocation the glue owns; nothing in it is thread-affine.
unsafe impl Send for Process {}
unsafe impl Sync for Process {}

impl Process {
    pub fn id(&self) -> u32 {
        // AROS identifies a child by its Process pointer, not a small integer,
        // and the shell child is not the command's own process anyway.
        0
    }

    pub fn kill(&mut self) -> io::Result<()> {
        // Signalling an arbitrary AROS process to die is not generally safe:
        // there is no equivalent of SIGKILL that unwinds a Process's resources.
        // Closing the child's stdin (drop its ChildStdin) is the supported way
        // to ask a well-behaved child to stop.
        Err(io::const_error!(io::ErrorKind::Unsupported, "killing a child is not supported on AROS"))
    }

    fn reap(&mut self, code: i32) -> ExitStatus {
        let st = ExitStatus(code);
        self.status = Some(st);
        if let Some(p) = self.script.take() {
            let _ = crate::sys::fs::remove_file(&p);
        }
        st
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(st) = self.status {
            return Ok(st);
        }
        // The exit signal is allocated on, and delivered to, *this* task, and
        // only while we are actually inside the wait: the child may outlive us,
        // and signalling a task that has since exited faults.
        let bit = unsafe { aros_sig_alloc() };
        if bit < 0 {
            return Err(io::const_error!(
                io::ErrorKind::Uncategorized,
                "no free signal to wait for a child"
            ));
        }
        let mask = 1u32 << bit;
        let me = unsafe { aros_task_self() };
        let mut code = 0i32;
        loop {
            let already = unsafe { aros_proc_set_waiter(self.handle, me, mask) };
            if already != 0 || unsafe { aros_proc_exited(self.handle, &mut code) } != 0 {
                break;
            }
            unsafe { aros_sig_wait(mask) };
        }
        unsafe {
            aros_proc_set_waiter(self.handle, core::ptr::null_mut(), 0);
            aros_sig_free(bit);
            aros_proc_exited(self.handle, &mut code);
        }
        Ok(self.reap(code))
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(st) = self.status {
            return Ok(Some(st));
        }
        let mut code = 0i32;
        if unsafe { aros_proc_exited(self.handle, &mut code) } != 0 {
            Ok(Some(self.reap(code)))
        } else {
            Ok(None)
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // The exit hook writes through `handle`, so it may only be released
        // once the child is known to be gone.
        if self.status.is_none() {
            let mut code = 0i32;
            if unsafe { aros_proc_exited(self.handle, &mut code) } == 0 {
                // Still running: leak the handle rather than let a dying child
                // write into freed memory. Bounded by the number of children
                // dropped while running, which is rare.
                return;
            }
        }
        unsafe { aros_proc_free(self.handle) };
        if let Some(p) = self.script.take() {
            let _ = crate::sys::fs::remove_file(&p);
        }
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
    fn from(_pipe: ChildPipe) -> Stdio {
        // Handing one child's endpoint to another child would need the pipe
        // rebound at spawn time, which the shell path cannot express.
        Stdio::Null
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

/// Drain a child's stdout and stderr to EOF.
///
/// Sequential rather than concurrent: AROS pipes buffer, and the two streams
/// belong to one child, so a child that fills stderr while we are still reading
/// stdout could stall. Both endpoints are put in non-blocking mode and drained
/// in turn so neither can wedge the other.
pub fn read_output(
    out: ChildPipe,
    stdout: &mut Vec<u8>,
    err: ChildPipe,
    stderr: &mut Vec<u8>,
) -> io::Result<()> {
    let _ = out.set_nonblocking(true);
    let _ = err.set_nonblocking(true);

    let (mut out_done, mut err_done) = (false, false);
    let mut chunk = [0u8; 4096];
    while !(out_done && err_done) {
        let mut progressed = false;
        for (pipe, buf, done) in [
            (&out, &mut *stdout, &mut out_done),
            (&err, &mut *stderr, &mut err_done),
        ] {
            if *done {
                continue;
            }
            match pipe.read(&mut chunk) {
                Ok(0) => *done = true,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    progressed = true;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => progressed = true,
                Err(e) => return Err(e),
            }
        }
        if !progressed && !(out_done && err_done) {
            // Nothing ready on either stream: yield instead of spinning.
            crate::thread::yield_now();
        }
    }
    Ok(())
}

pub fn getpid() -> u32 {
    1
}
