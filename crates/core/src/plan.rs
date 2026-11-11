//! A plan is the list of changes an operation will make. It is built without
//! changing anything, so it can be printed for `--dry-run` and shown in the
//! confirmation prompt, and then executed step by step.

use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

use crate::env::BootEnv;
use crate::exec::Executor;
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
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Step {
    pub description: String,
    #[serde(flatten)]
    pub action: Action,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Plan {
    pub title: String,
    pub steps: Vec<Step>,
    /// Steps run after a failure, best effort (for example unmounting).
    pub cleanup: Vec<Step>,
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
        self.steps.push(Step {
            description: description.into(),
            action,
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
        }
        if !self.cleanup.is_empty() {
            let _ = writeln!(out, "\nIf a step fails, hwtool then runs:");
            for step in &self.cleanup {
                render_action(&mut out, &step.action);
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

    /// Execute every step in order. Stops at the first failure, then runs
    /// the cleanup steps.
    pub fn execute(&self, root: &Root, exec: &mut dyn Executor, stamp: &str) -> Result<()> {
        for (i, step) in self.steps.iter().enumerate() {
            eprintln!("[{}/{}] {}", i + 1, self.steps.len(), step.description);
            if let Err(e) = apply(root, exec, stamp, &step.action) {
                for c in &self.cleanup {
                    let _ = apply(root, exec, stamp, &c.action);
                }
                return Err(e.context(format!("step {} failed: {}", i + 1, step.description)));
            }
        }
        Ok(())
    }
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
        Action::VerifyKernel { suffix } => {
            let _ = writeln!(
                out,
                "    check /boot/Image points at a kernel ending in {suffix} and /boot/dtb/<fdtfile> exists"
            );
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
    }
}

fn backup(root: &Root, stamp: &str, path: &str) -> Result<()> {
    let src = root.path(path);
    if !src.exists() {
        return Ok(());
    }
    let dst = root.path(&format!("{}/{stamp}{path}", crate::BACKUP_DIR));
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    fs::copy(&src, &dst).with_context(|| format!("cannot back up {path}"))?;
    eprintln!("    backup: {}", root.unmap(&dst));
    Ok(())
}

/// Write through a temporary file in the same directory and rename it, so
/// the file is never left half written. Keeps the mode of an existing file.
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
    fs::write(&tmp, content).with_context(|| format!("cannot write {}", tmp.display()))?;
    if let Ok(meta) = fs::metadata(&dst) {
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(meta.permissions().mode()));
    }
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
