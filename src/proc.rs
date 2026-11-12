//! Bounded subprocesses: every external program medrec runs (poppler,
//! tesseract, rclone, provider CLIs) goes through here.
//!
//! - stdin is closed, so no child can wait on a terminal prompt;
//! - the child leads its own process group, and at the deadline the whole
//!   group gets SIGTERM, then SIGKILL two seconds later, so a grandchild
//!   cannot outlive the call holding a pipe open;
//! - stdout is capped (the call fails rather than buffering without limit)
//!   and stderr keeps only its last 8 KiB, for diagnostics;
//! - `MEDREC_*` variables are removed from the child's environment: a
//!   provider or OCR tool has no use for where medrec's identity lives.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pk_cli_core::CliError;

/// macOS has no `pipe2`, so a pipe can leak into a child forked by another
/// thread before its close-on-exec flag is set, and that child then holds
/// our pipe open. One spawn at a time closes the window.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

const STDERR_TAIL: usize = 8 * 1024;
const TERM_GRACE: Duration = Duration::from_secs(2);

pub struct Bounded {
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    timeout: Duration,
    max_stdout: usize,
}

/// What a bounded run produced. `code` is `None` when a signal ended it.
#[derive(Debug)]
pub struct Ran {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr_tail: String,
}

impl Bounded {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Bounded {
            program: program.as_ref().to_owned(),
            args: Vec::new(),
            env: Vec::new(),
            timeout: Duration::from_secs(60),
            max_stdout: 16 * 1024 * 1024,
        }
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_owned()));
        self
    }

    pub fn arg(mut self, a: impl AsRef<OsStr>) -> Self {
        self.args.push(a.as_ref().to_owned());
        self
    }

    pub fn env(mut self, k: impl AsRef<OsStr>, v: impl AsRef<OsStr>) -> Self {
        self.env
            .push((k.as_ref().to_owned(), v.as_ref().to_owned()));
        self
    }

    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    pub fn max_stdout(mut self, n: usize) -> Self {
        self.max_stdout = n;
        self
    }

    fn name(&self) -> String {
        Path::new(&self.program)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.program.to_string_lossy().into_owned())
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args).stdin(Stdio::null());
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("MEDREC_") {
                cmd.env_remove(&k);
            }
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        cmd
    }

    fn spawn(&self, stdout: Stdio) -> Result<Child, CliError> {
        let _guard = SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        self.command()
            .stdout(stdout)
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| spawn_error(&self.name(), e))
    }

    /// Run to completion or to the deadline.
    pub fn run(&self) -> Result<Ran, CliError> {
        let deadline = Instant::now() + self.timeout;
        let mut child = self.spawn(Stdio::piped())?;
        let max = self.max_stdout;
        let mut out_pipe = child.stdout.take().expect("piped");
        let (tx, rx) = mpsc::channel();
        let out_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let over = (&mut out_pipe)
                .take(max as u64 + 1)
                .read_to_end(&mut buf)
                .map(|_| buf.len() > max)
                .unwrap_or(false);
            let _ = tx.send(over);
            buf
        });
        let err_thread = drain_tail(child.stderr.take());
        let status = loop {
            if let Ok(true) = rx.try_recv() {
                kill_group(&mut child);
                return Err(CliError::Upstream(format!(
                    "`{}` wrote more than {} bytes; stopped",
                    self.name(),
                    max
                )));
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Ok(None) => {
                    kill_group(&mut child);
                    return Err(timed_out(&self.name(), self.timeout));
                }
                Err(e) => {
                    kill_group(&mut child);
                    return Err(CliError::Other(format!(
                        "waiting on `{}`: {e}",
                        self.name()
                    )));
                }
            }
        };
        // The child is gone; a grandchild may still hold the pipes, so the
        // readers get what is left of the budget, then the group is killed.
        let stdout = join_until(out_thread, deadline).ok_or_else(|| {
            kill_pgid(child.id());
            timed_out(&self.name(), self.timeout)
        })?;
        if stdout.len() > max {
            return Err(CliError::Upstream(format!(
                "`{}` wrote more than {max} bytes; stopped",
                self.name()
            )));
        }
        let stderr_tail = join_until(err_thread, deadline).unwrap_or_default();
        Ok(Ran {
            code: status.code(),
            stdout,
            stderr_tail,
        })
    }

    /// Start the program with stdout as a stream for the caller to read. A
    /// watchdog kills the group at the deadline; [`Streaming::finish`]
    /// waits for the exit.
    pub fn stream(&self) -> Result<Streaming, CliError> {
        let deadline = Instant::now() + self.timeout;
        let mut child = self.spawn(Stdio::piped())?;
        let stdout = child.stdout.take().expect("piped");
        let stderr = drain_tail(child.stderr.take());
        let pid = child.id();
        let (cancel, cancelled) = mpsc::channel::<()>();
        let watchdog = std::thread::spawn(move || {
            let wait = deadline.saturating_duration_since(Instant::now());
            match cancelled.recv_timeout(wait) {
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    kill_pgid(pid);
                    true
                }
                _ => false,
            }
        });
        Ok(Streaming {
            name: self.name(),
            timeout: self.timeout,
            child,
            stdout: Some(stdout),
            stderr: Some(stderr),
            cancel: Some(cancel),
            watchdog: Some(watchdog),
        })
    }
}

pub struct Streaming {
    name: String,
    timeout: Duration,
    child: Child,
    stdout: Option<ChildStdout>,
    stderr: Option<JoinHandle<String>>,
    cancel: Option<mpsc::Sender<()>>,
    watchdog: Option<JoinHandle<bool>>,
}

#[derive(Debug)]
pub struct Finished {
    pub status: ExitStatus,
    pub stderr_tail: String,
    pub timed_out: bool,
}

impl Streaming {
    pub fn stdout(&mut self) -> &mut ChildStdout {
        self.stdout.as_mut().expect("stdout taken only by finish")
    }

    /// Stop early (the reader refused the stream): kill the group.
    pub fn abort(mut self) {
        kill_group(&mut self.child);
        self.stop_watchdog();
    }

    fn stop_watchdog(&mut self) -> bool {
        if let Some(c) = self.cancel.take() {
            let _ = c.send(());
        }
        self.watchdog
            .take()
            .map(|w| w.join().unwrap_or(false))
            .unwrap_or(false)
    }

    /// After the reader refused the stream: close the pipe, give the child
    /// `grace` to exit on its own (its exit code says why better than a
    /// cut stream), then stop its group.
    pub fn finish_within(mut self, grace: Duration) -> Result<Finished, CliError> {
        drop(self.stdout.take());
        let until = std::time::Instant::now() + grace;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    kill_group(&mut self.child);
                    break;
                }
            }
        }
        self.finish()
    }

    pub fn finish(mut self) -> Result<Finished, CliError> {
        drop(self.stdout.take());
        let status = self
            .child
            .wait()
            .map_err(|e| CliError::Other(format!("waiting on `{}`: {e}", self.name)))?;
        let timed_out = self.stop_watchdog();
        let stderr_tail = self
            .stderr
            .take()
            .map(|h| h.join().unwrap_or_default())
            .unwrap_or_default();
        if timed_out {
            return Err(timed_out_err(&self.name, self.timeout));
        }
        Ok(Finished {
            status,
            stderr_tail,
            timed_out,
        })
    }
}

impl Drop for Streaming {
    fn drop(&mut self) {
        if self.cancel.is_some() {
            kill_group(&mut self.child);
            self.stop_watchdog();
        }
    }
}

fn timed_out(name: &str, t: Duration) -> CliError {
    timed_out_err(name, t)
}

fn timed_out_err(name: &str, t: Duration) -> CliError {
    CliError::Upstream(format!(
        "`{name}` gave no answer within {} s; stopped",
        t.as_secs()
    ))
}

fn spawn_error(name: &str, e: std::io::Error) -> CliError {
    if e.kind() == std::io::ErrorKind::NotFound {
        CliError::Usage(format!("`{name}` is not installed (not found on PATH)"))
    } else {
        CliError::Other(format!("running `{name}`: {e}"))
    }
}

fn drain_tail(pipe: Option<std::process::ChildStderr>) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let Some(mut p) = pipe else {
            return String::new();
        };
        let mut tail: Vec<u8> = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = p.read(&mut buf) {
            if n == 0 {
                break;
            }
            tail.extend_from_slice(&buf[..n]);
            if tail.len() > STDERR_TAIL * 2 {
                tail.drain(..tail.len() - STDERR_TAIL);
            }
        }
        if tail.len() > STDERR_TAIL {
            tail.drain(..tail.len() - STDERR_TAIL);
        }
        String::from_utf8_lossy(&tail).into_owned()
    })
}

fn join_until<T: Send + 'static>(h: JoinHandle<T>, deadline: Instant) -> Option<T> {
    while !h.is_finished() {
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    h.join().ok()
}

fn kill_group(child: &mut Child) {
    kill_pgid(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

/// SIGTERM to the group, then SIGKILL after a grace period.
fn kill_pgid(pid: u32) {
    #[cfg(unix)]
    {
        let pgid = pid as libc::pid_t;
        // SAFETY: killpg only sends a signal; a stale group id fails with
        // ESRCH and is ignored.
        unsafe {
            libc::killpg(pgid, libc::SIGTERM);
        }
        let start = Instant::now();
        while start.elapsed() < TERM_GRACE {
            // SAFETY: as above; signal 0 probes whether the group exists.
            if unsafe { libc::killpg(pgid, 0) } != 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // SAFETY: as above.
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// The first `name` on PATH that is an executable file. Relative PATH
/// entries are skipped, so a directory named `.` never supplies a tool.
pub fn which(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return is_executable(&p).then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn captures_stdout_exit_code_and_stderr_tail() {
        let r = Bounded::new("/bin/sh")
            .args(["-c", "echo out; echo err >&2; exit 3"])
            .run()
            .unwrap();
        assert_eq!(r.code, Some(3));
        assert_eq!(r.stdout, b"out\n");
        assert_eq!(r.stderr_tail.trim(), "err");
    }

    #[test]
    fn a_hung_child_and_its_grandchild_are_stopped_at_the_deadline() {
        let start = Instant::now();
        let err = Bounded::new("/bin/sh")
            .args(["-c", "sleep 30 & sleep 30"])
            .timeout(Duration::from_millis(300))
            .run()
            .unwrap_err();
        assert_eq!(err.exit_code(), 5);
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn oversized_output_fails_instead_of_buffering() {
        let err = Bounded::new("/bin/sh")
            .args(["-c", "yes medrec | head -c 100000"])
            .max_stdout(1000)
            .run()
            .unwrap_err();
        assert!(err.to_string().contains("more than 1000 bytes"), "{err}");
    }

    #[test]
    fn medrec_variables_do_not_reach_the_child() {
        std::env::set_var("MEDREC_PROC_TEST_SECRET", "x");
        let r = Bounded::new("/bin/sh")
            .args(["-c", "echo \"[$MEDREC_PROC_TEST_SECRET]\""])
            .run()
            .unwrap();
        assert_eq!(r.stdout, b"[]\n");
    }

    #[test]
    fn a_missing_program_is_a_usage_error() {
        let err = Bounded::new("medrec-no-such-tool").run().unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn streaming_reads_lines_and_reports_the_exit() {
        use std::io::BufRead;
        let mut s = Bounded::new("/bin/sh")
            .args(["-c", "printf 'a\\nb\\n'; exit 4"])
            .stream()
            .unwrap();
        let lines: Vec<String> = std::io::BufReader::new(s.stdout())
            .lines()
            .map(Result::unwrap)
            .collect();
        assert_eq!(lines, ["a", "b"]);
        let f = s.finish().unwrap();
        assert_eq!(f.status.code(), Some(4));
    }

    #[test]
    fn streaming_is_stopped_at_its_deadline() {
        let s = Bounded::new("/bin/sh")
            .args(["-c", "sleep 30"])
            .timeout(Duration::from_millis(200))
            .stream()
            .unwrap();
        let err = s.finish().unwrap_err();
        assert_eq!(err.exit_code(), 5);
    }
}
