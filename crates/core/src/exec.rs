//! Running programs. The live system uses [`SystemExecutor`]; tests use
//! [`RecordingExecutor`], which records commands and returns canned output.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::{bail, Context, Result};

pub trait Executor {
    /// Run a program. Its output goes to hwtool's output and log. Fails on
    /// a non-zero exit.
    fn run(&mut self, argv: &[String]) -> Result<()>;
    /// Run a program and return its standard output. Fails on a non-zero exit.
    fn capture(&mut self, argv: &[String]) -> Result<String>;
}

#[derive(Default)]
pub struct SystemExecutor;

fn copy_out(mut r: impl Read + Send + 'static) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => crate::out::bytes(&buf[..n]),
            }
        }
    })
}

/// A pipe whose ends are closed when a program is started (O_CLOEXEC).
fn pipe() -> Option<(OwnedFd, OwnedFd)> {
    let mut fds: [libc::c_int; 2] = [0; 2];
    // SAFETY: pipe2 fills the two descriptors; they are owned from here on.
    unsafe {
        if libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
            return None;
        }
        Some((OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])))
    }
}

/// Report apt's status lines as progress while `live` is set. The thread is
/// not joined: a daemon started by a package script may keep the pipe open
/// after apt has ended.
fn apt_progress(read: OwnedFd, kind: crate::progress::Apt, live: Arc<AtomicBool>) {
    let step = crate::progress::current_step();
    thread::spawn(move || {
        for line in BufReader::new(File::from(read)).split(b'\n') {
            let Ok(line) = line else { break };
            if !live.load(Ordering::SeqCst) {
                break;
            }
            let line = String::from_utf8_lossy(&line);
            if let Some((percent, text)) = crate::progress::apt_status(kind, &line) {
                crate::progress::report_for(step, Some(percent), None, None, Some(&text));
            }
        }
    });
}

impl Executor for SystemExecutor {
    fn run(&mut self, argv: &[String]) -> Result<()> {
        // With --progress-json, rsync and apt-get are asked to report how
        // far they are. Only what they report changes, and the log says so.
        let mut with_progress = argv.to_vec();
        let mut status: Option<(OwnedFd, OwnedFd, crate::progress::Apt)> = None;
        if crate::progress::enabled() && !argv.is_empty() {
            let added: Vec<String> = match crate::progress::instrument(argv) {
                crate::progress::Instrument::None => Vec::new(),
                crate::progress::Instrument::Rsync => crate::progress::RSYNC_PROGRESS
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                crate::progress::Instrument::Apt(kind) => match pipe() {
                    Some((r, w)) if w.as_raw_fd() > 2 => {
                        let fd = w.as_raw_fd();
                        status = Some((r, w, kind));
                        vec!["-o".to_string(), format!("APT::Status-Fd={fd}")]
                    }
                    _ => Vec::new(),
                },
            };
            if !added.is_empty() {
                crate::out::say(&format!(
                    "    for progress, run with: {}",
                    crate::plan::shell_join(&added)
                ));
                with_progress.splice(1..1, added);
            }
        }
        let argv_shown = argv;
        let argv = with_progress.as_slice();
        let (prog, args) = argv.split_first().context("empty command")?;
        // Own process group: a Ctrl-C or a closed terminal does not reach it.
        // Its output goes through hwtool, whose writes cannot fail, so it
        // never gets SIGPIPE when the terminal or the GTK app goes away.
        let mut command = Command::new(prog);
        command
            .args(args)
            .env("DEBIAN_FRONTEND", "noninteractive")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        if let Some((_, w, _)) = &status {
            let fd = w.as_raw_fd();
            // SAFETY: fcntl is async-signal-safe. It lets apt-get inherit
            // the write end of the status pipe, under the same number.
            unsafe {
                command.pre_exec(move || {
                    if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("cannot run {prog}"))?;
        let live = Arc::new(AtomicBool::new(true));
        if let Some((r, w, kind)) = status {
            // Only the program holds the write end now.
            drop(w);
            apt_progress(r, kind, live.clone());
        }
        let a = child.stdout.take().map(copy_out);
        let b = child.stderr.take().map(copy_out);
        let status = child
            .wait()
            .with_context(|| format!("waiting for {prog}"))?;
        for h in [a, b].into_iter().flatten() {
            let _ = h.join();
        }
        live.store(false, Ordering::SeqCst);
        crate::out::end_line();
        if !status.success() {
            bail!(
                "{} exited with {}",
                crate::plan::shell_join(argv_shown),
                status
            );
        }
        Ok(())
    }

    fn capture(&mut self, argv: &[String]) -> Result<String> {
        let (prog, args) = argv.split_first().context("empty command")?;
        let out = Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0)
            .output()
            .with_context(|| format!("cannot run {prog}"))?;
        if !out.status.success() {
            bail!(
                "{} exited with {}: {}",
                crate::plan::shell_join(argv),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// Test executor: records every command and answers `capture` from a table
/// keyed by the joined command line. Unknown captures fail.
#[derive(Default)]
pub struct RecordingExecutor {
    pub ran: Vec<Vec<String>>,
    pub answers: HashMap<String, String>,
    /// Commands (joined) whose `run` fails.
    pub failing: Vec<String>,
}

impl RecordingExecutor {
    pub fn answer(&mut self, cmd: &str, output: &str) {
        self.answers.insert(cmd.to_string(), output.to_string());
    }
}

impl Executor for RecordingExecutor {
    fn run(&mut self, argv: &[String]) -> Result<()> {
        self.ran.push(argv.to_vec());
        let joined = argv.join(" ");
        if self.failing.iter().any(|f| joined.starts_with(f.as_str())) {
            bail!("{joined} failed (test)");
        }
        Ok(())
    }

    fn capture(&mut self, argv: &[String]) -> Result<String> {
        let key = argv.join(" ");
        self.ran.push(argv.to_vec());
        match self.answers.get(&key) {
            Some(v) => Ok(v.clone()),
            None => bail!("no answer for {key}"),
        }
    }
}

pub fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}
