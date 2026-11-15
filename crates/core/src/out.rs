//! Output while a change runs. Everything goes to standard error and, once
//! a change has started, to its log file in /var/log/hwtool. Writing to the
//! terminal never fails the operation: when the terminal or the pipe to the
//! GTK app is gone, the output still reaches the log and the change goes on
//! to the end or to its cleanup.
//!
//! With `--progress-json` (see progress.rs) the output goes line by line to
//! standard output as `log` events and to the log file, and not to standard
//! error. Lines that are a program's progress become `progress` events.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::root::Root;

pub const LOG_DIR: &str = "/var/log/hwtool";

static LOG: Mutex<Option<File>> = Mutex::new(None);
/// With the progress stream on: the line being received, and whether the
/// byte before it was a carriage return.
static PENDING: Mutex<(Vec<u8>, bool)> = Mutex::new((Vec::new(), false));

fn to_log(b: &[u8]) {
    if let Ok(mut g) = LOG.lock() {
        if let Some(f) = g.as_mut() {
            let _ = f.write_all(b);
        }
    }
}

/// One line (ended by a line feed or, as programs do for progress they
/// overwrite, a carriage return) with the progress stream on.
fn line(text: &[u8], end: u8) {
    let s = String::from_utf8_lossy(text);
    match crate::progress::sniff(&s) {
        Some(found) => {
            if found.keep {
                to_log(text);
                to_log(&[end]);
            }
            crate::progress::report(
                Some(found.percent),
                found.bytes,
                found.total,
                Some(&found.detail),
            );
        }
        None => {
            to_log(text);
            to_log(&[end]);
            crate::progress::emit(&crate::progress::Event::Log {
                line: s.trim_end().to_string(),
            });
        }
    }
}

fn lines(b: &[u8]) {
    let Ok(mut g) = PENDING.lock() else {
        return;
    };
    for &c in b {
        if c == b'\n' || c == b'\r' {
            let text = std::mem::take(&mut g.0);
            if !text.is_empty() {
                line(&text, c);
            } else if c == b'\n' && !g.1 {
                // An empty line: kept in the log file.
                to_log(b"\n");
            }
            g.1 = c == b'\r';
        } else {
            g.0.push(c);
            g.1 = false;
        }
    }
}

/// End a line a program left unfinished, so the next output starts a new
/// one. Only the progress stream keeps lines; otherwise nothing to do.
pub fn end_line() {
    let text = match PENDING.lock() {
        Ok(mut g) => std::mem::take(&mut g.0),
        Err(_) => return,
    };
    if !text.is_empty() {
        line(&text, b'\n');
    }
}

/// Write raw bytes to standard error and the log. Errors are ignored.
pub fn bytes(b: &[u8]) {
    if crate::progress::enabled() {
        lines(b);
        return;
    }
    let _ = std::io::stderr().write_all(b);
    if let Ok(mut g) = LOG.lock() {
        if let Some(f) = g.as_mut() {
            let _ = f.write_all(b);
        }
    }
}

/// One line of output.
pub fn say(msg: &str) {
    let mut s = msg.to_string();
    s.push('\n');
    bytes(s.as_bytes());
}

/// Start the log of one change: /var/log/hwtool/<stamp>.log, created new
/// (never through an existing file or link). Returns its path.
pub fn open_log(root: &Root, stamp: &str) -> Result<String> {
    let dir = root.path(LOG_DIR);
    fs::create_dir_all(&dir).with_context(|| format!("cannot create {LOG_DIR}"))?;
    let path = format!("{LOG_DIR}/{stamp}.log");
    let f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o640)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.path(&path))
        .with_context(|| format!("cannot create {path}"))?;
    if let Ok(mut g) = LOG.lock() {
        *g = Some(f);
    }
    Ok(path)
}

pub fn close_log() {
    end_line();
    if let Ok(mut g) = LOG.lock() {
        if let Some(f) = g.take() {
            let _ = f.sync_all();
        }
    }
}
