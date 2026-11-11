//! The swap file.
//!
//! Roxora images get /swap.img at the first boot from cloud-init's mounts
//! module (roxora-os/build userpatches/roxora/cloud-init/99-roxora.cfg:
//! `swap: filename /swap.img, size auto`). cloud-init's cc_mounts.py writes
//! the fstab line `/swap.img none swap sw 0 0`; cloud-init 24.1 added
//! `,comment=cloudconfig` to its options, 25.1 and later do not. When
//! cc_mounts runs again (a new instance id), it drops every line with
//! comment=cloudconfig and appends its swap line for a file that is in use.
//! So hwtool writes the line with comment=cloudconfig: cloud-init then
//! replaces it rather than adding a second one.
//!
//! The BSP's armbian-zram-config.service also puts swap on zram (ENABLED=true
//! in /etc/default/armbian-zram-config, priority 5). The swap file has the
//! kernel's default lower priority, so zram is used first.

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::plan::{Action, Plan};
use crate::root::Root;
use crate::storage;

pub const FILE: &str = "/swap.img";
pub const FSTAB: &str = "/etc/fstab";
pub const SYSCTL: &str = "/etc/sysctl.d/60-hwtool-swap.conf";
pub const SWAPPINESS: &str = "/proc/sys/vm/swappiness";
const MIB: u64 = 1 << 20;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Active {
    pub name: String,
    pub kind: String,
    pub size_bytes: u64,
    pub used_bytes: u64,
    pub priority: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    /// Active swap, from /proc/swaps (what `swapon --show` reads).
    pub active: Vec<Active>,
    /// The swap file: from fstab, else /swap.img.
    pub file: String,
    pub file_bytes: Option<u64>,
    pub fstab_line: Option<String>,
    pub mem_total_bytes: u64,
    pub mem_available_bytes: u64,
    pub zram: bool,
    /// armbian-zram-config.service is installed.
    pub zram_service: bool,
    pub swappiness: Option<u32>,
    pub swappiness_saved: Option<u32>,
    pub root_fstype: String,
}

pub fn parse_swaps(text: &str) -> Vec<Active> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() >= 5).then(|| Active {
                name: f[0].replace("\\040", " "),
                kind: f[1].to_string(),
                size_bytes: f[2].parse::<u64>().unwrap_or(0) * 1024,
                used_bytes: f[3].parse::<u64>().unwrap_or(0) * 1024,
                priority: f[4].parse().unwrap_or(0),
            })
        })
        .collect()
}

fn meminfo(root: &Root, key: &str) -> u64 {
    root.read_opt("/proc/meminfo")
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key}:")))
        .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        .unwrap_or(0)
        * 1024
}

/// The swap lines in an fstab: (line index, device).
fn swap_lines(fstab: &str) -> Vec<(usize, String)> {
    fstab
        .lines()
        .enumerate()
        .filter_map(|(i, l)| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (!l.trim_start().starts_with('#') && f.len() >= 3 && f[2] == "swap")
                .then(|| (i, f[0].to_string()))
        })
        .collect()
}

/// The fstab with every line for `file` removed and, if `add`, one
/// cloud-init style line for it where the first one was (or at the end).
pub fn edit_fstab(fstab: &str, file: &str, add: bool) -> String {
    let ours = format!("{file}\tnone\tswap\tsw,{}\t0\t0", "comment=cloudconfig");
    let lines = swap_lines(fstab);
    let drop: Vec<usize> = lines
        .iter()
        .filter(|(_, d)| d == file)
        .map(|(i, _)| *i)
        .collect();
    let mut out = Vec::new();
    let mut placed = false;
    for (i, l) in fstab.lines().enumerate() {
        if drop.contains(&i) {
            if add && !placed {
                out.push(ours.clone());
                placed = true;
            }
            continue;
        }
        out.push(l.to_string());
    }
    if add && !placed {
        out.push(ours);
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

fn file_of(fstab: &str) -> String {
    swap_lines(fstab)
        .into_iter()
        .map(|(_, d)| d)
        .find(|d| d.starts_with('/') && !d.starts_with("/dev/"))
        .unwrap_or_else(|| FILE.to_string())
}

pub fn status(root: &Root) -> Result<Status> {
    let fstab = root.read_opt(FSTAB).unwrap_or_default();
    let file = file_of(&fstab);
    let fstab_line = fstab
        .lines()
        .zip(0..)
        .find(|(_, i)| swap_lines(&fstab).iter().any(|(j, d)| j == i && *d == file))
        .map(|(l, _)| l.to_string());
    let active = parse_swaps(&root.read_opt("/proc/swaps").unwrap_or_default());
    let zram = active.iter().any(|a| a.name.starts_with("/dev/zram"));
    let zram_service = ["/lib/systemd/system", "/usr/lib/systemd/system"]
        .iter()
        .any(|d| root.exists(&format!("{d}/armbian-zram-config.service")));
    let swappiness_saved = root.read_opt(SYSCTL).and_then(|t| {
        t.lines()
            .find_map(|l| l.trim().strip_prefix("vm.swappiness"))
            .and_then(|v| v.trim().trim_start_matches('=').trim().parse().ok())
    });
    Ok(Status {
        file_bytes: std::fs::metadata(root.path(&file)).ok().map(|m| m.len()),
        active,
        fstab_line,
        file,
        mem_total_bytes: meminfo(root, "MemTotal"),
        mem_available_bytes: meminfo(root, "MemAvailable"),
        zram,
        zram_service,
        swappiness: root
            .read_opt(SWAPPINESS)
            .and_then(|s| s.trim().parse().ok()),
        swappiness_saved,
        root_fstype: storage::mounts(root)
            .into_iter()
            .rfind(|m| m.mountpoint == "/")
            .map(|m| m.fstype)
            .unwrap_or_default(),
    })
}

/// "2G", "512M", "1.5G", "2048" (MiB).
pub fn parse_size(s: &str) -> Result<u64> {
    let t = s.trim().to_ascii_uppercase();
    let t = t.trim_end_matches("IB").trim_end_matches('B');
    let (num, mul) = match t.chars().last() {
        Some('K') => (&t[..t.len() - 1], 1u64 << 10),
        Some('M') => (&t[..t.len() - 1], MIB),
        Some('G') => (&t[..t.len() - 1], 1 << 30),
        Some('T') => (&t[..t.len() - 1], 1 << 40),
        _ => (t, MIB),
    };
    let n: f64 = num
        .parse()
        .ok()
        .filter(|n: &f64| n.is_finite() && *n > 0.0)
        .with_context(|| format!("{s} is not a size (for example 2G or 512M)"))?;
    // Whole MiB, as fallocate and mkswap like it.
    Ok(((n * mul as f64) as u64 / MIB) * MIB)
}

fn human(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.1} GiB", b as f64 / (1u64 << 30) as f64)
    } else {
        format!("{} MiB", b / MIB)
    }
}

/// Space of the file system holding `dir`: (total, available to users).
fn fs_space(root: &Root, dir: &str) -> Result<(u64, u64)> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let p = CString::new(root.path(dir).as_os_str().as_bytes())?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: p is a valid C string and st a valid statvfs to fill.
    if unsafe { libc::statvfs(p.as_ptr(), &mut st) } != 0 {
        bail!(
            "cannot read the free space of {dir}: {}",
            std::io::Error::last_os_error()
        );
    }
    let f = st.f_frsize as u64;
    Ok((st.f_blocks as u64 * f, st.f_bavail as u64 * f))
}

/// swapoff `file` if active; refuses if the memory cannot take what is on it.
fn swapoff_step(st: &Status, file: &str, p: &mut Plan) -> Result<()> {
    if let Some(a) = st.active.iter().find(|a| a.name == file) {
        // Keep 256 MiB of memory free after moving the swapped pages back.
        let need = a.used_bytes + 256 * MIB;
        if st.mem_available_bytes < need {
            bail!(
                "{file} holds {} of swapped-out memory and only {} of memory is available, \
                 so it cannot be switched off now. Close some programs (or reboot) and try again",
                human(a.used_bytes),
                human(st.mem_available_bytes)
            );
        }
        p.run(
            format!(
                "Stop swapping to {file} ({} in use moves back to memory)",
                human(a.used_bytes)
            ),
            &["swapoff", file],
        );
    }
    Ok(())
}

pub fn set_plan(root: &Root, size: &str, force: bool) -> Result<Plan> {
    let file = status(root)?.file;
    let dir = std::path::Path::new(&file)
        .parent()
        .and_then(|d| d.to_str())
        .unwrap_or("/")
        .to_string();
    set_plan_with_space(root, size, force, fs_space(root, &dir)?)
}

/// [`set_plan`] with the file system's (total, available) bytes given.
pub fn set_plan_with_space(
    root: &Root,
    size: &str,
    force: bool,
    (total, avail): (u64, u64),
) -> Result<Plan> {
    let st = status(root)?;
    let file = st.file.clone();
    let bytes = parse_size(size)?;
    if bytes < 16 * MIB {
        bail!("a swap file needs at least 16 MiB");
    }
    match st.root_fstype.as_str() {
        "ext4" | "xfs" => {}
        "btrfs" => bail!("the root file system is btrfs; hwtool makes swap files only on ext4"),
        "" => bail!("cannot tell the root file system's type from /proc/self/mountinfo"),
        o => bail!("the root file system is {o}; hwtool makes swap files only on ext4"),
    }
    let existing = st.file_bytes.unwrap_or(0);
    if st.file_bytes == Some(bytes) && st.active.iter().any(|a| a.name == file) {
        let mut p = Plan::new(format!("Swap file {file}: {}", human(bytes)));
        if st.fstab_line.is_none() {
            let fstab = root.read_opt(FSTAB).unwrap_or_default();
            p.step(
                format!("Back up {FSTAB}"),
                Action::Backup { path: FSTAB.into() },
            );
            p.step(
                format!("Add {file} to {FSTAB}"),
                Action::WriteFile {
                    path: FSTAB.into(),
                    content: edit_fstab(&fstab, &file, true),
                },
            );
        } else {
            p.notices
                .push(format!("{file} is already {} and in use.", human(bytes)));
        }
        return Ok(p);
    }
    let after = (avail + existing).saturating_sub(bytes);
    if bytes + 64 * MIB > avail + existing {
        bail!(
            "{} does not fit: the file system has {} free{}",
            human(bytes),
            human(avail),
            if existing > 0 {
                format!(" plus the {} of the current swap file", human(existing))
            } else {
                String::new()
            }
        );
    }
    if !force && after < total / 4 {
        bail!(
            "a {} swap file would leave {} free, less than a quarter of the file system ({}). \
             Add --force to make it anyway",
            human(bytes),
            human(after),
            human(total)
        );
    }
    if !force && st.mem_total_bytes > 0 && bytes > 2 * st.mem_total_bytes {
        bail!(
            "{} is more than twice the memory ({}); add --force to make it anyway",
            human(bytes),
            human(st.mem_total_bytes)
        );
    }
    let mut p = Plan::new(format!(
        "{} the swap file {file} {}",
        if existing > 0 { "Resize" } else { "Make" },
        if existing > 0 {
            format!("from {} to {}", human(existing), human(bytes))
        } else {
            format!("of {}", human(bytes))
        }
    ));
    swapoff_step(&st, &file, &mut p)?;
    if existing > 0 || root.exists(&file) {
        p.run(format!("Delete the old {file}"), &["rm", "-f", &file]);
    }
    let first = p.steps.len();
    p.run(
        format!("Create {file}, readable only by root"),
        &["install", "-m", "0600", "/dev/null", &file],
    );
    p.run(
        format!("Reserve {} for it", human(bytes)),
        &["fallocate", "-l", &format!("{}M", bytes / MIB), &file],
    );
    p.run("Make it swap space", &["mkswap", &file]);
    p.run("Start swapping to it", &["swapon", &file]);
    let last = p.steps.len() - 1;
    let fstab = root.read_opt(FSTAB).unwrap_or_default();
    p.step(
        format!("Back up {FSTAB}"),
        Action::Backup { path: FSTAB.into() },
    );
    p.step(
        format!("Use it at every boot ({FSTAB}, in cloud-init's format)"),
        Action::WriteFile {
            path: FSTAB.into(),
            content: edit_fstab(&fstab, &file, true),
        },
    );
    // If making the new file fails, leave no half-made file and no fstab
    // line pointing at a missing one.
    p.on_failure(
        first,
        last,
        format!("Delete the unfinished {file}"),
        Action::Run {
            argv: vec!["rm".into(), "-f".into(), file.clone()],
        },
    );
    if st.fstab_line.is_some() {
        p.on_failure(
            first,
            last,
            format!("Remove {file} from {FSTAB}"),
            Action::WriteFile {
                path: FSTAB.into(),
                content: edit_fstab(&fstab, &file, false),
            },
        );
    }
    if st.zram {
        p.notices.push(
            "zram swap (armbian-zram-config) stays and is used first; the file takes what does not fit there."
                .into(),
        );
    }
    Ok(p)
}

pub fn off_plan(root: &Root) -> Result<Plan> {
    let st = status(root)?;
    let file = st.file.clone();
    let mut p = Plan::new(format!("Remove the swap file {file}"));
    if st.file_bytes.is_none() && st.fstab_line.is_none() {
        p.notices.push(format!("There is no swap file {file}."));
        return Ok(p);
    }
    swapoff_step(&st, &file, &mut p)?;
    if st.fstab_line.is_some() {
        let fstab = root.read(FSTAB)?;
        p.step(
            format!("Back up {FSTAB}"),
            Action::Backup { path: FSTAB.into() },
        );
        p.step(
            format!("Remove {file} from {FSTAB}"),
            Action::WriteFile {
                path: FSTAB.into(),
                content: edit_fstab(&fstab, &file, false),
            },
        );
    }
    p.run(format!("Delete {file}"), &["rm", "-f", &file]);
    if st.zram {
        p.notices
            .push("zram swap (armbian-zram-config) stays.".into());
    }
    Ok(p)
}

pub fn swappiness_plan(root: &Root, value: u32) -> Result<Plan> {
    if value > 200 {
        bail!("swappiness is 0 to 200");
    }
    if !root.exists(SWAPPINESS) {
        bail!("{SWAPPINESS} is missing");
    }
    let mut p = Plan::new(format!("Set vm.swappiness to {value}"));
    p.step(
        format!("Set it now ({SWAPPINESS})"),
        Action::SysfsWrite {
            path: SWAPPINESS.into(),
            value: value.to_string(),
        },
    );
    p.step(
        format!("Back up {SYSCTL}"),
        Action::Backup {
            path: SYSCTL.into(),
        },
    );
    p.step(
        format!("Keep it at every boot ({SYSCTL})"),
        Action::WriteFile {
            path: SYSCTL.into(),
            content: format!("# Written by hwtool swap swappiness.\nvm.swappiness = {value}\n"),
        },
    );
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("2G").unwrap(), 2 << 30);
        assert_eq!(parse_size("512M").unwrap(), 512 * MIB);
        assert_eq!(parse_size("512MiB").unwrap(), 512 * MIB);
        assert_eq!(parse_size("1.5g").unwrap(), 1536 * MIB);
        assert_eq!(parse_size("300").unwrap(), 300 * MIB);
        assert!(parse_size("lots").is_err() && parse_size("-1G").is_err());
    }

    #[test]
    fn fstab_edits() {
        let f = "UUID=x / ext4 defaults 0 1\n/swap.img\tnone\tswap\tsw\t0\t0\n";
        assert_eq!(
            edit_fstab(f, "/swap.img", true),
            "UUID=x / ext4 defaults 0 1\n/swap.img\tnone\tswap\tsw,comment=cloudconfig\t0\t0\n"
        );
        assert_eq!(
            edit_fstab(f, "/swap.img", false),
            "UUID=x / ext4 defaults 0 1\n"
        );
        // Duplicates (cloud-init 25.1 run twice) become one line.
        let d = format!("{f}/swap.img none swap sw 0 0\n# /swap.img none swap sw 0 0\n");
        assert_eq!(
            edit_fstab(&d, "/swap.img", true),
            "UUID=x / ext4 defaults 0 1\n/swap.img\tnone\tswap\tsw,comment=cloudconfig\t0\t0\n# /swap.img none swap sw 0 0\n"
        );
        assert_eq!(
            edit_fstab("UUID=x / ext4 defaults 0 1\n", "/swap.img", true),
            "UUID=x / ext4 defaults 0 1\n/swap.img\tnone\tswap\tsw,comment=cloudconfig\t0\t0\n"
        );
    }

    #[test]
    fn proc_swaps() {
        let s = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n/dev/zram0 partition 1012748 0 5\n/swap.img file 2097148 1024 -2\n";
        let a = parse_swaps(s);
        assert_eq!(a.len(), 2);
        assert_eq!(a[1].name, "/swap.img");
        assert_eq!(a[1].used_bytes, 1024 * 1024);
        assert_eq!(a[1].priority, -2);
    }
}
