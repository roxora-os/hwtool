//! Running programs. The live system uses [`SystemExecutor`]; tests use
//! [`RecordingExecutor`], which records commands and returns canned output.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
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

impl Executor for SystemExecutor {
    fn run(&mut self, argv: &[String]) -> Result<()> {
        let (prog, args) = argv.split_first().context("empty command")?;
        // Own process group: a Ctrl-C or a closed terminal does not reach it.
        // Its output goes through hwtool, whose writes cannot fail, so it
        // never gets SIGPIPE when the terminal or the GTK app goes away.
        let mut child = Command::new(prog)
            .args(args)
            .env("DEBIAN_FRONTEND", "noninteractive")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .with_context(|| format!("cannot run {prog}"))?;
        let a = child.stdout.take().map(copy_out);
        let b = child.stderr.take().map(copy_out);
        let status = child
            .wait()
            .with_context(|| format!("waiting for {prog}"))?;
        for h in [a, b].into_iter().flatten() {
            let _ = h.join();
        }
        if !status.success() {
            bail!("{} exited with {}", crate::plan::shell_join(argv), status);
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
