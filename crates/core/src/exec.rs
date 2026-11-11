//! Running programs. The live system uses [`SystemExecutor`]; tests use
//! [`RecordingExecutor`], which records commands and returns canned output.

use std::collections::HashMap;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

pub trait Executor {
    /// Run a program with the terminal attached. Fails on a non-zero exit.
    fn run(&mut self, argv: &[String]) -> Result<()>;
    /// Run a program and return its standard output. Fails on a non-zero exit.
    fn capture(&mut self, argv: &[String]) -> Result<String>;
}

#[derive(Default)]
pub struct SystemExecutor;

impl Executor for SystemExecutor {
    fn run(&mut self, argv: &[String]) -> Result<()> {
        let (prog, args) = argv.split_first().context("empty command")?;
        let status = Command::new(prog)
            .args(args)
            .status()
            .with_context(|| format!("cannot run {prog}"))?;
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
}

impl RecordingExecutor {
    pub fn answer(&mut self, cmd: &str, output: &str) {
        self.answers.insert(cmd.to_string(), output.to_string());
    }
}

impl Executor for RecordingExecutor {
    fn run(&mut self, argv: &[String]) -> Result<()> {
        self.ran.push(argv.to_vec());
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
