//! Disks and SPI flash, read from sysfs and /proc under the root.

use serde::Serialize;

use crate::root::Root;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Partition {
    pub name: String,
    pub start_bytes: u64,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Disk {
    pub name: String,
    pub path: String,
    /// `emmc`, `sd`, `nvme`, `usb` or `disk`.
    pub kind: String,
    pub size_bytes: u64,
    pub partitions: Vec<Partition>,
    /// The root filesystem of the running system is on this disk.
    pub holds_root: bool,
    /// Mounted partitions (or the disk itself), including swap.
    pub mounted: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Mtd {
    pub name: String,
    pub path: String,
    pub label: String,
    pub size_bytes: u64,
}

fn read_u64(root: &Root, path: &str) -> u64 {
    root.read_opt(path)
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn skip_disk(name: &str) -> bool {
    ["loop", "ram", "zram", "dm-", "sr", "fd", "mtdblock", "md"]
        .iter()
        .any(|p| name.starts_with(p))
        || (name.starts_with("mmcblk") && (name.contains("boot") || name.ends_with("rpmb")))
}

/// Device names currently mounted or used as swap.
fn in_use(root: &Root) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(m) = root.read_opt("/proc/self/mounts") {
        for line in m.lines() {
            if let Some(dev) = line
                .split_whitespace()
                .next()
                .and_then(|d| d.strip_prefix("/dev/"))
            {
                out.push(dev.to_string());
            }
        }
    }
    if let Some(s) = root.read_opt("/proc/swaps") {
        for line in s.lines().skip(1) {
            if let Some(dev) = line
                .split_whitespace()
                .next()
                .and_then(|d| d.strip_prefix("/dev/"))
            {
                out.push(dev.to_string());
            }
        }
    }
    out
}

/// Kernel name of the partition (or disk) holding `/`.
pub fn root_device(root: &Root) -> Option<String> {
    let mounts = root.read_opt("/proc/self/mounts").unwrap_or_default();
    let dev = mounts.lines().rev().find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.len() >= 2 && f[1] == "/").then(|| f[0].to_string())
    });
    if let Some(d) = dev.as_deref().and_then(|d| d.strip_prefix("/dev/")) {
        if d != "root" {
            return Some(d.to_string());
        }
    }
    // /dev/root or no /dev entry: follow root= on the kernel command line.
    let cmdline = root.read_opt("/proc/cmdline").unwrap_or_default();
    let arg = cmdline
        .split_whitespace()
        .find_map(|a| a.strip_prefix("root="))?;
    let link = if let Some(u) = arg.strip_prefix("UUID=") {
        format!("/dev/disk/by-uuid/{u}")
    } else if let Some(u) = arg.strip_prefix("PARTUUID=") {
        format!("/dev/disk/by-partuuid/{u}")
    } else {
        return arg.strip_prefix("/dev/").map(str::to_string);
    };
    root.read_link(&link)
        .map(|t| t.rsplit('/').next().unwrap_or("").to_string())
        .filter(|s| !s.is_empty())
}

pub fn disks(root: &Root) -> Vec<Disk> {
    let used = in_use(root);
    let root_dev = root_device(root);
    let mut out = Vec::new();
    for name in root.list_dir("/sys/block") {
        if skip_disk(&name) {
            continue;
        }
        let base = format!("/sys/block/{name}");
        let kind = if name.starts_with("mmcblk") {
            match root
                .read_opt(&format!("{base}/device/type"))
                .as_deref()
                .map(str::trim)
            {
                Some("MMC") => "emmc",
                Some("SD") => "sd",
                _ => {
                    if root.exists(&format!("/sys/block/{name}boot0")) {
                        "emmc"
                    } else {
                        "sd"
                    }
                }
            }
        } else if name.starts_with("nvme") {
            "nvme"
        } else if root.read_link(&base).is_some_and(|t| t.contains("/usb")) {
            "usb"
        } else {
            "disk"
        };
        let mut partitions = Vec::new();
        for p in root.list_dir(&base) {
            if p.starts_with(&name) && root.exists(&format!("{base}/{p}/partition")) {
                partitions.push(Partition {
                    start_bytes: read_u64(root, &format!("{base}/{p}/start")) * 512,
                    size_bytes: read_u64(root, &format!("{base}/{p}/size")) * 512,
                    name: p,
                });
            }
        }
        partitions.sort_by_key(|p| p.start_bytes);
        let mut mounted: Vec<String> = used
            .iter()
            .filter(|u| **u == name || partitions.iter().any(|p| &p.name == *u))
            .cloned()
            .collect();
        mounted.dedup();
        let holds_root = root_dev
            .as_ref()
            .is_some_and(|r| *r == name || partitions.iter().any(|p| &p.name == r));
        out.push(Disk {
            path: format!("/dev/{name}"),
            kind: kind.into(),
            size_bytes: read_u64(root, &format!("{base}/size")) * 512,
            partitions,
            holds_root,
            mounted,
            name,
        });
    }
    out
}

pub fn find_disk(root: &Root, dev: &str) -> Option<Disk> {
    let name = dev.strip_prefix("/dev/").unwrap_or(dev);
    disks(root).into_iter().find(|d| d.name == name)
}

/// SPI/NOR flash exposed through MTD.
pub fn mtds(root: &Root) -> Vec<Mtd> {
    root.list_dir("/sys/class/mtd")
        .into_iter()
        .filter(|n| {
            n.strip_prefix("mtd")
                .is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit()))
        })
        .map(|n| Mtd {
            path: format!("/dev/{n}"),
            label: root
                .read_opt(&format!("/sys/class/mtd/{n}/name"))
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
            size_bytes: read_u64(root, &format!("/sys/class/mtd/{n}/size")),
            name: n,
        })
        .collect()
}

/// Partition device node: /dev/sda -> /dev/sda1, /dev/nvme0n1 -> /dev/nvme0n1p1
/// (`_install_part_dev` in configng's module_install_engine.sh).
pub fn part_dev(disk: &str, n: u32) -> String {
    if disk.chars().last().is_some_and(|c| c.is_ascii_digit()) {
        format!("{disk}p{n}")
    } else {
        format!("{disk}{n}")
    }
}
