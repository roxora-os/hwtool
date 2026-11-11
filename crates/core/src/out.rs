//! Output while a change runs. Everything goes to standard error and, once
//! a change has started, to its log file in /var/log/hwtool. Writing to the
//! terminal never fails the operation: when the terminal or the pipe to the
//! GTK app is gone, the output still reaches the log and the change goes on
//! to the end or to its cleanup.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::root::Root;

pub const LOG_DIR: &str = "/var/log/hwtool";

static LOG: Mutex<Option<File>> = Mutex::new(None);

/// Write raw bytes to standard error and the log. Errors are ignored.
pub fn bytes(b: &[u8]) {
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
    if let Ok(mut g) = LOG.lock() {
        if let Some(f) = g.take() {
            let _ = f.sync_all();
        }
    }
}
