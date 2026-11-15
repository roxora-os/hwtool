//! Progress of a change for programs (`--progress-json`): one JSON object
//! per line on standard output. The GTK app reads it to show which step is
//! running and how far it is.
//!
//! ```text
//! {"event":"plan","title":"...","steps":["...","..."],"reboot":true}
//! {"event":"step","step":1,"of":2,"title":"...","state":"running"}
//! {"event":"log","line":"[1/2] ..."}
//! {"event":"progress","step":1,"percent":42.0,"detail":"..."}
//! {"event":"step","step":1,"of":2,"title":"...","state":"done"}
//! {"event":"done","ok":true,"log":"/var/log/hwtool/<time>.log","reboot":true}
//! ```
//!
//! Without the option nothing here does anything, and hwtool's output is
//! the text it always was.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Running,
    Done,
    Failed,
}

/// One line of the progress stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The plan that is about to run. First event of a run.
    Plan {
        title: String,
        /// The description of each step, in order.
        steps: Vec<String>,
        /// The plan needs a reboot to take effect.
        reboot: bool,
    },
    /// A step started, finished or failed. `step` counts from 1.
    Step {
        step: usize,
        of: usize,
        title: String,
        state: State,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// How far the running step is: a percentage, bytes, or both.
    Progress {
        step: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        percent: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
        /// A few words on what is happening, for example the package.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// A cleanup step after a failure started, finished or failed.
    Cleanup {
        title: String,
        state: State,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// One line of what hwtool or a program it ran printed.
    Log { line: String },
    /// The run is over. Last event of a run.
    Done {
        ok: bool,
        /// The log file, when the run got as far as opening one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log: Option<String>,
        /// The change worked and needs a reboot to take effect.
        reboot: bool,
        /// The step that failed, counting from 1.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failed_step: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

impl Event {
    /// The event as one line of JSON, without the line end.
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Read one line of the stream.
    pub fn decode(line: &str) -> Option<Event> {
        serde_json::from_str(line.trim()).ok()
    }
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);
/// The step that is running, counting from 1; 0 when none is.
static STEP: AtomicUsize = AtomicUsize::new(0);
/// The last progress event sent: step, percent (-1: none), detail.
static LAST: Mutex<Option<(usize, f64, String)>> = Mutex::new(None);

/// Turn the progress stream on (`--progress-json`).
pub fn enable() {
    ENABLED.store(true, Ordering::SeqCst);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// A `done` event was sent, so the caller need not send one for an error.
pub fn done_sent() -> bool {
    DONE.load(Ordering::SeqCst)
}

/// Write an event to standard output. Does nothing unless enabled. A
/// closed pipe is ignored, like all of hwtool's output during a change.
pub fn emit(e: &Event) {
    if !enabled() {
        return;
    }
    if matches!(e, Event::Done { .. }) {
        DONE.store(true, Ordering::SeqCst);
    }
    let mut line = e.encode();
    line.push('\n');
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(line.as_bytes());
    let _ = out.flush();
}

/// Note which step runs (0: none), so progress read from a program's
/// output is reported for it.
pub fn set_step(step: usize) {
    STEP.store(step, Ordering::SeqCst);
    if let Ok(mut g) = LAST.lock() {
        *g = None;
    }
}

/// The step that is running, counting from 1; 0 when none is.
pub fn current_step() -> usize {
    STEP.load(Ordering::SeqCst)
}

/// Report progress of the running step. Repeats of the same whole percent
/// with the same detail are dropped, and the percentage never goes back.
pub fn report(percent: Option<f64>, bytes: Option<u64>, total: Option<u64>, detail: Option<&str>) {
    report_for(current_step(), percent, bytes, total, detail);
}

/// [`report`] for `step`, dropped if that step is no longer the running
/// one (a reader thread that is late).
pub fn report_for(
    step: usize,
    percent: Option<f64>,
    bytes: Option<u64>,
    total: Option<u64>,
    detail: Option<&str>,
) {
    if !enabled() || step == 0 || step != current_step() {
        return;
    }
    let mut percent = percent.map(|p| (p.clamp(0.0, 100.0) * 10.0).round() / 10.0);
    let detail = detail.map(str::trim).filter(|d| !d.is_empty());
    if let Ok(mut g) = LAST.lock() {
        let d = detail.unwrap_or("");
        let mut reached = percent.unwrap_or(-1.0);
        if let Some((_, before, old)) = g.as_ref().filter(|(s, _, _)| *s == step) {
            if percent.is_some() && reached < *before {
                reached = *before;
                percent = Some(reached);
            }
            if reached.floor() == before.floor() && old == d {
                return;
            }
        }
        *g = Some((step, reached, d.to_string()));
    }
    emit(&Event::Progress {
        step,
        percent,
        bytes,
        total,
        detail: detail.map(str::to_string),
    });
}

// ---- progress read from the programs hwtool runs ----------------------------

/// What kind of apt-get run a command is, for reading its status lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Apt {
    /// `apt-get update`: downloads only.
    Update,
    /// install and the like: downloads, then dpkg.
    Install,
    /// remove, purge: dpkg only.
    Remove,
}

/// How a command is run so that it reports its progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instrument {
    None,
    /// Add `--info=progress2 --no-inc-recursive`.
    Rsync,
    /// Add `-o APT::Status-Fd=<fd>` and read the status lines from it.
    Apt(Apt),
}

/// The options added to rsync: one overall progress line, and the whole
/// file list first so that the percentage is of everything.
pub const RSYNC_PROGRESS: [&str; 2] = ["--info=progress2", "--no-inc-recursive"];

/// Decide how to run `argv` with progress. Options that only change what
/// the program reports are added; what it does stays the plan's command.
pub fn instrument(argv: &[String]) -> Instrument {
    let Some((prog, args)) = argv.split_first() else {
        return Instrument::None;
    };
    let name = prog.rsplit('/').next().unwrap_or(prog);
    match name {
        "rsync" => {
            let quiet = args.iter().any(|a| {
                a == "--quiet"
                    || a.starts_with("--info")
                    || a.starts_with("--progress")
                    || (a.starts_with('-') && !a.starts_with("--") && a.contains('q'))
            });
            if quiet {
                Instrument::None
            } else {
                Instrument::Rsync
            }
        }
        "apt-get" => {
            if args
                .iter()
                .any(|a| a == "-s" || a == "--simulate" || a.contains("Status-Fd"))
            {
                return Instrument::None;
            }
            for a in args {
                match a.as_str() {
                    "update" => return Instrument::Apt(Apt::Update),
                    "install" | "upgrade" | "dist-upgrade" | "full-upgrade" => {
                        return Instrument::Apt(Apt::Install)
                    }
                    "remove" | "purge" | "autoremove" => return Instrument::Apt(Apt::Remove),
                    _ => {}
                }
            }
            Instrument::None
        }
        _ => Instrument::None,
    }
}

/// Progress found in one line of output.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub percent: f64,
    pub bytes: Option<u64>,
    pub total: Option<u64>,
    pub detail: String,
    /// The line also belongs in the log (false for lines that exist only
    /// because hwtool asked the program for progress).
    pub keep: bool,
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// An rsync `--info=progress2` line:
/// `  1,234,567  12%   1.20MB/s    0:00:12 (xfr#5, to-chk=10/200)`.
fn rsync_line(line: &str) -> Option<Found> {
    let mut t = line.split_whitespace();
    let (size, pct, speed) = (t.next()?, t.next()?, t.next()?);
    if !size.chars().all(|c| c.is_ascii_digit() || c == ',') || !speed.ends_with("/s") {
        return None;
    }
    let bytes: u64 = size.replace(',', "").parse().ok()?;
    let percent: f64 = pct.strip_suffix('%')?.parse().ok()?;
    Some(Found {
        percent,
        bytes: Some(bytes),
        total: None,
        detail: format!("{} copied, {speed}", human(bytes)),
        keep: false,
    })
}

/// A `flashcp -v` line: `Erasing blocks: 3/25 (12%)`,
/// `Writing data: 10k/1561k (0%)` or `Verifying data: 10k/1561k (0%)`.
/// The three passes each count for a third.
fn flashcp_line(line: &str) -> Option<Found> {
    let line = line.trim();
    let phase = if line.starts_with("Erasing blocks:") {
        0.0
    } else if line.starts_with("Writing data:") {
        1.0
    } else if line.starts_with("Verifying data:") {
        2.0
    } else {
        return None;
    };
    let inner = line.strip_suffix("%)")?;
    let (head, pct) = inner.rsplit_once('(')?;
    let pct: f64 = pct.trim().parse().ok()?;
    let (mut bytes, mut total) = (None, None);
    if phase > 0.0 {
        if let Some((a, b)) = head
            .split(':')
            .nth(1)
            .and_then(|counts| counts.trim().split_once('/'))
        {
            let kib = |s: &str| s.trim().strip_suffix('k')?.parse::<u64>().ok();
            if let (Some(a), Some(b)) = (kib(a), kib(b)) {
                bytes = Some(a * 1024);
                total = Some(b * 1024);
            }
        }
    }
    Some(Found {
        percent: (phase * 100.0 + pct.clamp(0.0, 100.0)) / 3.0,
        bytes,
        total,
        detail: line.to_string(),
        keep: true,
    })
}

/// Progress in a line a program printed, if it is one of the lines hwtool
/// knows: rsync's overall progress or flashcp's passes.
pub fn sniff(line: &str) -> Option<Found> {
    flashcp_line(line).or_else(|| rsync_line(line))
}

/// One line apt wrote to its status descriptor:
/// `dlstatus:<n>:<percent>:<text>` while it downloads and
/// `pmstatus:<package>:<percent>:<text>` while dpkg works. Returns the
/// percentage of the whole command and the text. For an install the
/// downloads count for the first 40 % and dpkg for the rest.
pub fn apt_status(kind: Apt, line: &str) -> Option<(f64, String)> {
    let mut f = line.trim().splitn(4, ':');
    let (what, _, pct, text) = (f.next()?, f.next()?, f.next()?, f.next().unwrap_or(""));
    let pct: f64 = pct.trim().replace(',', ".").parse().ok()?;
    let pct = pct.clamp(0.0, 100.0);
    let overall = match (what, kind) {
        ("dlstatus", Apt::Install) => pct * 0.4,
        ("pmstatus", Apt::Install) => 40.0 + pct * 0.6,
        ("dlstatus", Apt::Update) | ("pmstatus", Apt::Remove) => pct,
        _ => return None,
    };
    Some((overall, text.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn events_encode_as_one_line_of_json() {
        let e = Event::Step {
            step: 1,
            of: 3,
            title: "Back up /boot/armbianEnv.txt".into(),
            state: State::Running,
            error: None,
        };
        assert_eq!(
            e.encode(),
            r#"{"event":"step","step":1,"of":3,"title":"Back up /boot/armbianEnv.txt","state":"running"}"#
        );
        let e = Event::Progress {
            step: 2,
            percent: Some(42.5),
            bytes: Some(1024),
            total: Some(4096),
            detail: None,
        };
        assert_eq!(
            e.encode(),
            r#"{"event":"progress","step":2,"percent":42.5,"bytes":1024,"total":4096}"#
        );
        let e = Event::Log {
            line: "a \"quoted\"\tline".into(),
        };
        assert_eq!(e.encode(), r#"{"event":"log","line":"a \"quoted\"\tline"}"#);
        let e = Event::Done {
            ok: true,
            log: Some("/var/log/hwtool/20261111T041751Z.log".into()),
            reboot: true,
            failed_step: None,
            error: None,
        };
        assert_eq!(
            e.encode(),
            r#"{"event":"done","ok":true,"log":"/var/log/hwtool/20261111T041751Z.log","reboot":true}"#
        );
    }

    #[test]
    fn every_event_survives_encode_and_decode() {
        let events = vec![
            Event::Plan {
                title: "Enable overlays".into(),
                steps: vec!["Back up".into(), "Write".into()],
                reboot: true,
            },
            Event::Step {
                step: 2,
                of: 2,
                title: "Write".into(),
                state: State::Failed,
                error: Some("cannot write\nsecond line".into()),
            },
            Event::Progress {
                step: 1,
                percent: Some(12.0),
                bytes: None,
                total: None,
                detail: Some("Unpacking linux-image".into()),
            },
            Event::Cleanup {
                title: "Unmount".into(),
                state: State::Done,
                error: None,
            },
            Event::Log {
                line: "[1/2] Back up".into(),
            },
            Event::Done {
                ok: false,
                log: None,
                reboot: false,
                failed_step: Some(2),
                error: Some("step 2 failed".into()),
            },
        ];
        for e in events {
            let line = e.encode();
            assert!(!line.contains('\n'), "{line}");
            assert_eq!(Event::decode(&line), Some(e), "{line}");
        }
    }

    #[test]
    fn decode_refuses_other_text() {
        assert_eq!(Event::decode(""), None);
        assert_eq!(Event::decode("[1/2] Back up"), None);
        assert_eq!(Event::decode(r#"{"event":"unknown"}"#), None);
        assert_eq!(
            Event::decode(r#"{"event":"log","line":"x"}"#),
            Some(Event::Log { line: "x".into() })
        );
    }

    #[test]
    fn rsync_progress_lines() {
        let f = sniff("    1,234,567  12%    1.20MB/s    0:00:12").unwrap();
        assert_eq!(f.percent, 12.0);
        assert_eq!(f.bytes, Some(1_234_567));
        assert_eq!(f.detail, "1.2 MiB copied, 1.20MB/s");
        assert!(!f.keep);
        let f = sniff("  3,456,789,012 100%   45.67MB/s    0:01:12 (xfr#5, to-chk=0/200)").unwrap();
        assert_eq!(f.percent, 100.0);
        assert_eq!(sniff("sending incremental file list"), None);
        assert_eq!(sniff("12 34% of something"), None);
        assert_eq!(sniff(""), None);
    }

    #[test]
    fn flashcp_passes_count_a_third_each() {
        let f = sniff("Erasing blocks: 12/24 (50%)").unwrap();
        assert!((f.percent - 16.666).abs() < 0.01, "{}", f.percent);
        assert!(f.keep);
        assert_eq!(f.bytes, None);
        let f = sniff("Writing data: 512k/1024k (50%)").unwrap();
        assert_eq!(f.percent, 50.0);
        assert_eq!((f.bytes, f.total), (Some(524_288), Some(1_048_576)));
        let f = sniff("Verifying data: 1024k/1024k (100%)").unwrap();
        assert_eq!(f.percent, 100.0);
        assert_eq!(sniff("Writing data: done"), None);
    }

    #[test]
    fn apt_status_lines() {
        assert_eq!(
            apt_status(Apt::Install, "dlstatus:1:50.0000:Retrieving file 1 of 2"),
            Some((20.0, "Retrieving file 1 of 2".into()))
        );
        assert_eq!(
            apt_status(
                Apt::Install,
                "pmstatus:linux-image-edge-rockchip64:50.0000:Unpacking linux-image-edge-rockchip64 (arm64)"
            ),
            Some((
                70.0,
                "Unpacking linux-image-edge-rockchip64 (arm64)".into()
            ))
        );
        assert_eq!(
            apt_status(
                Apt::Remove,
                "pmstatus:charsiu:25,5:Removing charsiu (arm64)"
            ),
            Some((25.5, "Removing charsiu (arm64)".into()))
        );
        assert_eq!(
            apt_status(Apt::Update, "dlstatus:3:75:Retrieving file 3 of 4"),
            Some((75.0, "Retrieving file 3 of 4".into()))
        );
        assert_eq!(apt_status(Apt::Update, "pmstatus:x:75:y"), None);
        assert_eq!(apt_status(Apt::Install, "pmerror:x:75:broken"), None);
        assert_eq!(apt_status(Apt::Install, "nonsense"), None);
    }

    #[test]
    fn which_commands_report_progress() {
        assert_eq!(
            instrument(&argv(&["rsync", "-ax", "--delete", "/", "/mnt"])),
            Instrument::Rsync
        );
        assert_eq!(
            instrument(&argv(&["rsync", "-aq", "/boot/", "/mnt/boot/"])),
            Instrument::None
        );
        assert_eq!(
            instrument(&argv(&["apt-get", "install", "-y", "linux-image"])),
            Instrument::Apt(Apt::Install)
        );
        assert_eq!(
            instrument(&argv(&["apt-get", "update"])),
            Instrument::Apt(Apt::Update)
        );
        assert_eq!(
            instrument(&argv(&["apt-get", "purge", "-y", "x"])),
            Instrument::Apt(Apt::Remove)
        );
        assert_eq!(
            instrument(&argv(&["apt-get", "-s", "-y", "install", "x"])),
            Instrument::None
        );
        assert_eq!(
            instrument(&argv(&["mkfs.ext4", "-qF", "/dev/x"])),
            Instrument::None
        );
        assert_eq!(instrument(&[]), Instrument::None);
    }
}
