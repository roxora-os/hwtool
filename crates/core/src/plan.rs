//! A plan is the list of changes an operation will make. It is built without
//! changing anything, so it can be printed for `--dry-run` and shown in the
//! confirmation prompt, and then executed step by step.

use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

use crate::env::BootEnv;
use crate::exec::Executor;
use crate::out::say;
use crate::root::Root;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Copy a file to the backup directory before it is changed.
    Backup { path: String },
    /// Replace a file with new content.
    WriteFile { path: String, content: String },
    /// Run a program.
    Run { argv: Vec<String> },
    /// Run a bash script. `shows` lists the commands it runs, for display.
    Shell { script: String, shows: Vec<String> },
    /// Read the UUID of `partition` and point `<mount>/etc/fstab` and
    /// `<mount>/boot/armbianEnv.txt` at it.
    PointAtNewRoot { partition: String, mount: String },
    /// Check that `<mount>/boot` can boot and `<mount>/etc/fstab` resolves.
    VerifyTarget { mount: String },
    /// Check that /boot/Image now points at a kernel whose version ends in
    /// `suffix` and that the device tree U-Boot loads exists.
    VerifyKernel { suffix: String },
    /// Save an installed package as a .deb file in `dir` (see repack.rs).
    SavePackage {
        name: String,
        version: String,
        dir: String,
        file: String,
        from_cache: Option<String>,
        installed_kib: u64,
    },
    /// Remove saved package sets in /var/lib/hwtool/packages except `keep`.
    PruneSaved { keep: String },
    /// Put back the copy a Backup step made of `path` during this run.
    RestoreBackup { path: String },
    /// Print where /boot/Image, /boot/dtb and /boot/uInitrd point. Never fails.
    ReportBoot,
    /// Print the NPU and charsiu status. Never fails.
    NpuStatus,
    /// Write a value to a sysfs file (no temporary file: sysfs takes writes
    /// in place).
    SysfsWrite { path: String, value: String },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Step {
    pub description: String,
    #[serde(flatten)]
    pub action: Action,
    /// Extra lines shown under the step, for example what apt resolved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
}

impl Step {
    pub fn new(description: impl Into<String>, action: Action) -> Self {
        Step {
            description: description.into(),
            action,
            details: Vec::new(),
        }
    }
}

/// A step run when the plan fails, but only if the step that failed (or the
/// last one that ran before a signal) has an index in `from..=to`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Cleanup {
    pub from: usize,
    pub to: usize,
    #[serde(flatten)]
    pub step: Step,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Plan {
    pub title: String,
    pub steps: Vec<Step>,
    /// Steps run after a failure (for example unmounting), in order. Each
    /// runs even if an earlier one failed; their results are reported.
    pub cleanup: Vec<Cleanup>,
    pub warnings: Vec<String>,
    pub notices: Vec<String>,
    pub reboot_required: bool,
}

impl Plan {
    pub fn new(title: impl Into<String>) -> Self {
        Plan {
            title: title.into(),
            ..Default::default()
        }
    }

    pub fn step(&mut self, description: impl Into<String>, action: Action) {
        self.steps.push(Step::new(description, action));
    }

    /// Add a cleanup step for failures at step indices `from..=to`.
    pub fn on_failure(
        &mut self,
        from: usize,
        to: usize,
        description: impl Into<String>,
        action: Action,
    ) {
        self.cleanup.push(Cleanup {
            from,
            to,
            step: Step::new(description, action),
        });
    }

    pub fn run(&mut self, description: impl Into<String>, argv: &[&str]) {
        self.step(
            description,
            Action::Run {
                argv: argv.iter().map(|s| s.to_string()).collect(),
            },
        );
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// The plan as text: exactly what is run and what is written where.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{}", self.title);
        if self.steps.is_empty() {
            let _ = writeln!(out, "Nothing to change.");
        }
        for (i, step) in self.steps.iter().enumerate() {
            let _ = writeln!(out, "\n{:>2}. {}", i + 1, step.description);
            render_action(&mut out, &step.action);
            for d in &step.details {
                let _ = writeln!(out, "      {d}");
            }
        }
        if !self.cleanup.is_empty() {
            let _ = writeln!(out, "\nIf a step fails or hwtool is interrupted:");
            for c in &self.cleanup {
                let range = if c.from == c.to {
                    format!("step {}", c.from + 1)
                } else if c.to >= self.steps.len().saturating_sub(1) {
                    format!("steps {} and later", c.from + 1)
                } else {
                    format!("steps {} to {}", c.from + 1, c.to + 1)
                };
                let _ = writeln!(out, "  after {range}: {}", c.step.description);
                render_action(&mut out, &c.step.action);
            }
        }
        for w in &self.warnings {
            let _ = writeln!(out, "\nWarning: {w}");
        }
        for n in &self.notices {
            let _ = writeln!(out, "\nNote: {n}");
        }
        if self.reboot_required {
            let _ = writeln!(out, "\nA reboot is needed for the change to take effect.");
        }
        out
    }

    /// A short fingerprint of what the plan does, with time stamps left
    /// out. The GTK app shows a dry run as the user and passes its
    /// fingerprint to the root run, which refuses if its own plan differs.
    pub fn fingerprint(&self) -> String {
        let text = mask_stamps(&self.render());
        // FNV-1a, 64 bit: stable across builds and enough to tell plans apart.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in text.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        format!("{h:016x}")
    }

    /// Execute every step in order. Stops at the first failure or signal,
    /// runs the cleanup steps that apply, and reports their results.
    pub fn execute(&self, root: &Root, exec: &mut dyn Executor, stamp: &str) -> Result<()> {
        // Never reuse a backup directory: two changes within one second
        // would otherwise overwrite the first backup.
        let mut stamp = stamp.to_string();
        let base = stamp.clone();
        let mut n = 1;
        while root.exists(&format!("{}/{stamp}", crate::BACKUP_DIR))
            || root.exists(&format!("{}/{stamp}.log", crate::out::LOG_DIR))
        {
            stamp = format!("{base}-{n}");
            n += 1;
        }
        let stamp = stamp.as_str();
        crate::signals::install();
        let log = crate::out::open_log(root, stamp)?;
        say(&format!("Log: {log}"));
        let result = self.run_steps(root, exec, stamp);
        if let Err(e) = &result {
            say(&format!("Failed: {e:#}"));
        }
        say(&format!("Log: {log}"));
        crate::out::close_log();
        result
    }

    fn run_steps(&self, root: &Root, exec: &mut dyn Executor, stamp: &str) -> Result<()> {
        let mut last_started: Option<usize> = None;
        let mut failure: Option<anyhow::Error> = None;
        for (i, step) in self.steps.iter().enumerate() {
            if crate::signals::interrupted() {
                failure = Some(anyhow!("interrupted by a signal before step {}", i + 1));
                break;
            }
            say(&format!(
                "[{}/{}] {}",
                i + 1,
                self.steps.len(),
                step.description
            ));
            last_started = Some(i);
            if let Err(e) = apply(root, exec, stamp, &step.action) {
                failure = Some(e.context(format!("step {} failed: {}", i + 1, step.description)));
                break;
            }
        }
        let Some(err) = failure else {
            return Ok(());
        };
        let mut report = Vec::new();
        if let Some(at) = last_started {
            for c in self.cleanup.iter().filter(|c| c.from <= at && at <= c.to) {
                say(&format!("Cleanup: {}", c.step.description));
                match apply(root, exec, stamp, &c.step.action) {
                    Ok(()) => report.push(format!("done: {}", c.step.description)),
                    Err(e) => {
                        say(&format!("Cleanup failed: {e:#}"));
                        report.push(format!("FAILED: {}: {e:#}", c.step.description));
                    }
                }
            }
        }
        if report.is_empty() {
            Err(err)
        } else {
            Err(err.context(format!("cleanup:\n  {}", report.join("\n  "))))
        }
    }
}

/// Replace time stamps (YYYYMMDDTHHMMSSZ, optionally -N) with <time>.
pub fn mask_stamps(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        let is_stamp = i + 16 <= b.len()
            && b[i..i + 8].iter().all(u8::is_ascii_digit)
            && b[i + 8] == b'T'
            && b[i + 9..i + 15].iter().all(u8::is_ascii_digit)
            && b[i + 15] == b'Z';
        if is_stamp {
            out.push_str("<time>");
            i += 16;
            if i < b.len() && b[i] == b'-' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
                i += 1;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            }
        } else {
            let ch = text[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn render_action(out: &mut String, action: &Action) {
    match action {
        Action::Backup { path } => {
            let _ = writeln!(out, "    copy {path} to {}/<time>{path}", crate::BACKUP_DIR);
        }
        Action::WriteFile { path, content } => {
            let _ = writeln!(out, "    write {path}:");
            for line in content.lines() {
                let _ = writeln!(out, "    | {line}");
            }
        }
        Action::Run { argv } => {
            let _ = writeln!(out, "    $ {}", shell_join(argv));
        }
        Action::Shell { shows, .. } => {
            for s in shows {
                let _ = writeln!(out, "    $ {s}");
            }
        }
        Action::PointAtNewRoot { partition, mount } => {
            let _ = writeln!(
                out,
                "    read the UUID of {partition}; set it as the / entry of {mount}/etc/fstab \
                 and as rootdev=UUID=... in {mount}/boot/armbianEnv.txt"
            );
        }
        Action::VerifyTarget { mount } => {
            let _ = writeln!(
                out,
                "    check {mount}/boot holds a kernel and boot.scr, and every UUID in \
                 {mount}/etc/fstab exists"
            );
        }
        Action::SavePackage {
            file,
            from_cache,
            installed_kib,
            ..
        } => match from_cache {
            Some(c) => {
                let _ = writeln!(out, "    copy {c} to {file}");
            }
            None => {
                let _ = writeln!(
                    out,
                    "    rebuild the package from the dpkg database and its installed files \
                     ({} MiB installed) into {file}",
                    installed_kib.div_ceil(1024)
                );
            }
        },
        Action::PruneSaved { keep } => {
            let _ = writeln!(
                out,
                "    remove the other package sets in {}, keep {keep}",
                crate::repack::PACKAGES_DIR
            );
        }
        Action::VerifyKernel { suffix } => {
            let _ = writeln!(
                out,
                "    check /boot/Image points at a kernel ending in {suffix} and /boot/dtb/<fdtfile> exists"
            );
        }
        Action::RestoreBackup { path } => {
            let _ = writeln!(
                out,
                "    copy {}/<time>{path} back to {path}",
                crate::BACKUP_DIR
            );
        }
        Action::ReportBoot => {
            let _ = writeln!(
                out,
                "    show where /boot/Image, /boot/dtb and /boot/uInitrd point"
            );
        }
        Action::NpuStatus => {
            let _ = writeln!(out, "    show the rocket driver, /dev/accel and charsiu");
        }
        Action::SysfsWrite { path, value } => {
            let _ = writeln!(out, "    write {value} to {path}");
        }
    }
}

/// Quote arguments the way a shell user would type them.
pub fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c))
            {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn apply(root: &Root, exec: &mut dyn Executor, stamp: &str, action: &Action) -> Result<()> {
    match action {
        Action::Backup { path } => backup(root, stamp, path),
        Action::WriteFile { path, content } => write_file(root, path, content),
        Action::Run { argv } => exec.run(argv),
        Action::Shell { script, .. } => {
            exec.run(&["bash".to_string(), "-c".to_string(), script.clone()])
        }
        Action::PointAtNewRoot { partition, mount } => {
            let out = exec.capture(&[
                "blkid".into(),
                "-s".into(),
                "UUID".into(),
                "-o".into(),
                "value".into(),
                partition.clone(),
            ])?;
            let uuid = out.trim();
            if uuid.is_empty() {
                bail!("{partition} has no filesystem UUID");
            }
            let fstab_path = format!("{mount}/etc/fstab");
            let fstab = root.read(&fstab_path)?;
            write_file(root, &fstab_path, &set_fstab_root(&fstab, uuid))?;
            let env_path = format!("{mount}/boot/armbianEnv.txt");
            let mut env = BootEnv::parse(&root.read(&env_path)?);
            env.set("rootdev", &format!("UUID={uuid}"));
            env.set("rootfstype", "ext4");
            write_file(root, &env_path, &env.render())
        }
        Action::VerifyTarget { mount } => verify_target(root, exec, mount),
        Action::VerifyKernel { suffix } => crate::kernel::verify_installed(root, suffix),
        Action::SavePackage {
            name, version, dir, ..
        } => {
            let f = crate::repack::save(root, exec, name, version, dir)?;
            say(&format!("    saved: {f}"));
            Ok(())
        }
        Action::PruneSaved { keep } => crate::repack::prune(root, keep),
        Action::RestoreBackup { path } => {
            let copy = format!("{}/{stamp}{path}", crate::BACKUP_DIR);
            let content = root
                .read(&copy)
                .with_context(|| format!("no backup of {path} from this run"))?;
            write_file(root, path, &content)?;
            say(&format!("    restored {path} from {copy}"));
            Ok(())
        }
        Action::NpuStatus => {
            crate::npu::report(root, exec);
            Ok(())
        }
        Action::SysfsWrite { path, value } => fs::write(root.path(path), value)
            .with_context(|| format!("cannot write {value} to {path}")),
        Action::ReportBoot => {
            for l in ["/boot/Image", "/boot/dtb", "/boot/uInitrd"] {
                let t = root
                    .read_link(l)
                    .unwrap_or_else(|| "(missing or not a link)".into());
                say(&format!("    {l} -> {t}"));
            }
            Ok(())
        }
    }
}

fn backup(root: &Root, stamp: &str, path: &str) -> Result<()> {
    let src = root.path(path);
    if !src.exists() {
        return Ok(());
    }
    let copy = format!("{}/{stamp}{path}", crate::BACKUP_DIR);
    let dst = root.path(&copy);
    let parent = dst
        .parent()
        .ok_or_else(|| anyhow!("{copy} has no parent directory"))?;
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let content = fs::read(&src).with_context(|| format!("cannot read {path}"))?;
    let mut f = new_file(&dst, 0o600).with_context(|| format!("cannot create {copy}"))?;
    f.write_all(&content)
        .and_then(|_| f.sync_all())
        .with_context(|| format!("cannot write {copy}"))?;
    sync_dir(parent);
    say(&format!("    backup: {copy}"));
    Ok(())
}

/// Create a file that does not exist yet. Never opens an existing file and
/// never follows a symbolic link.
fn new_file(path: &Path, mode: u32) -> std::io::Result<fs::File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

/// Replace a file durably: write a new temporary file in the same
/// directory, flush it to disk, rename it over the file, flush the
/// directory. A crash leaves either the old or the new file. Keeps the mode
/// of an existing file.
pub fn write_file(root: &Root, path: &str, content: &str) -> Result<()> {
    let dst = root.path(path);
    let parent = dst
        .parent()
        .ok_or_else(|| anyhow!("{path} has no parent directory"))?;
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.hwtool-new",
        dst.file_name().unwrap_or_default().to_string_lossy()
    ));
    // A leftover from an interrupted run (removing a link removes the link).
    let _ = fs::remove_file(&tmp);
    let mode = fs::symlink_metadata(&dst)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.permissions().mode() & 0o7777)
        .unwrap_or(0o644);
    let mut f = new_file(&tmp, mode).with_context(|| format!("cannot create {}", tmp.display()))?;
    f.write_all(content.as_bytes())
        .and_then(|_| f.sync_all())
        .with_context(|| format!("cannot write {}", tmp.display()))?;
    let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(mode));
    drop(f);
    fs::rename(&tmp, &dst).with_context(|| format!("cannot replace {path}"))?;
    sync_dir(parent);
    Ok(())
}

fn sync_dir(dir: &Path) {
    if let Ok(f) = fs::File::open(dir) {
        let _ = f.sync_all();
    }
}

/// Replace the device of the `/` entry in an fstab with `UUID=<uuid>`.
/// Other lines are kept as they are.
pub fn set_fstab_root(fstab: &str, uuid: &str) -> String {
    let mut out = String::new();
    let mut found = false;
    for line in fstab.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if !line.trim_start().starts_with('#') && fields.len() >= 2 && fields[1] == "/" && !found {
            found = true;
            let rest = &fields[1..];
            let _ = writeln!(out, "UUID={uuid} {}", rest.join(" "));
        } else {
            let _ = writeln!(out, "{line}");
        }
    }
    if !found {
        let _ = writeln!(out, "UUID={uuid} / ext4 defaults,errors=remount-ro 0 1");
    }
    out
}

fn verify_target(root: &Root, exec: &mut dyn Executor, mount: &str) -> Result<()> {
    let boot = format!("{mount}/boot");
    let names = root.list_dir(&boot);
    let kernel = names
        .iter()
        .any(|n| n == "Image" || n.starts_with("vmlinuz-") || n.starts_with("Image-"));
    let script = names.iter().any(|n| n == "boot.scr");
    if !kernel || !script {
        bail!("{boot} cannot boot: kernel image found: {kernel}, boot.scr found: {script}");
    }
    let fstab = root.read(&format!("{mount}/etc/fstab"))?;
    for line in fstab.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        if let Some(uuid) = line
            .split_whitespace()
            .next()
            .and_then(|f| f.strip_prefix("UUID="))
        {
            exec.capture(&["blkid".into(), "-U".into(), uuid.to_string()])
                .with_context(|| format!("fstab names UUID={uuid}, which no device has"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fstab_root_is_replaced() {
        let fstab = "UUID=old / ext4 defaults,errors=remount-ro 0 1\ntmpfs /tmp tmpfs defaults,nosuid 0 0\n";
        let out = set_fstab_root(fstab, "new");
        assert_eq!(
            out,
            "UUID=new / ext4 defaults,errors=remount-ro 0 1\ntmpfs /tmp tmpfs defaults,nosuid 0 0\n"
        );
    }

    #[test]
    fn stamps_are_masked() {
        assert_eq!(
            mask_stamps("a /x/20261111T041751Z-2/y 20261111T041751Z. 2026"),
            "a /x/<time>/y <time>. 2026"
        );
    }

    #[test]
    fn shell_join_quotes() {
        let argv: Vec<String> = ["dd", "if=a b", "of=/dev/x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(shell_join(&argv), "dd 'if=a b' of=/dev/x");
    }
}

/// UTC time as `YYYYMMDDTHHMMSSZ`, for backup directory names and history.
pub fn utc_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    stamp_from_unix(secs)
}

pub fn stamp_from_unix(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod stamp_tests {
    #[test]
    fn stamp() {
        assert_eq!(super::stamp_from_unix(0), "19700101T000000Z");
        assert_eq!(super::stamp_from_unix(1_794_408_000), "20261111T144000Z");
    }
}
