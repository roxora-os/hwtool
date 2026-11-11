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
    /// The running system uses this disk: `/`, `/boot`, or the partition
    /// U-Boot loaded the boot script from.
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

/// One line of /proc/self/mountinfo.
#[derive(Debug, Clone, PartialEq)]
pub struct Mount {
    pub majmin: String,
    pub mountpoint: String,
    pub fstype: String,
    pub source: String,
}

pub fn mounts(root: &Root) -> Vec<Mount> {
    let text = root.read_opt("/proc/self/mountinfo").unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let (left, right) = l.split_once(" - ")?;
            let f: Vec<&str> = left.split_whitespace().collect();
            let r: Vec<&str> = right.split_whitespace().collect();
            (f.len() >= 5 && r.len() >= 2).then(|| Mount {
                majmin: f[2].to_string(),
                mountpoint: f[4].replace("\\040", " "),
                fstype: r[0].to_string(),
                source: r[1].to_string(),
            })
        })
        .collect()
}

/// Kernel name of the block device with this major:minor number.
fn devname(root: &Root, majmin: &str) -> Option<String> {
    root.read_link(&format!("/sys/dev/block/{majmin}"))
        .and_then(|t| {
            t.trim_end_matches('/')
                .rsplit('/')
                .next()
                .map(str::to_string)
        })
        .filter(|s| !s.is_empty())
}

/// Whole disks under a block device: the disk itself, the disk a partition
/// belongs to, or for device mapper and RAID the disks of every slave.
pub fn disks_under(root: &Root, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut todo = vec![name.to_string()];
    let mut seen = Vec::new();
    while let Some(n) = todo.pop() {
        if seen.contains(&n) {
            continue;
        }
        seen.push(n.clone());
        let slaves = root.list_dir(&format!("/sys/block/{n}/slaves"));
        if !slaves.is_empty() {
            todo.extend(slaves);
            continue;
        }
        if root.exists(&format!("/sys/block/{n}")) {
            out.push(n);
            continue;
        }
        // A partition: /sys/block/<disk>/<partition>.
        if let Some(d) = root
            .list_dir("/sys/block")
            .into_iter()
            .find(|d| root.exists(&format!("/sys/block/{d}/{n}/partition")))
        {
            let slaves = root.list_dir(&format!("/sys/block/{d}/{n}/slaves"));
            if slaves.is_empty() {
                out.push(d);
            } else {
                todo.extend(slaves);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// What the running system is on.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct System {
    /// Kernel name of the device mounted on `/`.
    pub root_device: String,
    pub root_fstype: String,
    /// Disks holding `/`, `/boot` and the partition U-Boot loaded the boot
    /// script from (ubootpart= on the kernel command line). Destructive
    /// commands never write to these unless asked to.
    pub disks: Vec<String>,
}

/// Find the disks the running system uses. Fails, rather than guessing,
/// when `/` cannot be traced to a disk.
pub fn system(root: &Root) -> anyhow::Result<System> {
    let ms = mounts(root);
    let rootm = ms
        .iter()
        .rev()
        .find(|m| m.mountpoint == "/")
        .ok_or_else(|| anyhow::anyhow!("/proc/self/mountinfo has no entry for /"))?;
    let root_device = devname(root, &rootm.majmin).ok_or_else(|| {
        anyhow::anyhow!(
            "cannot find the block device {} that / is mounted from (source {})",
            rootm.majmin,
            rootm.source
        )
    })?;
    let mut disks = disks_under(root, &root_device);
    if disks.is_empty() {
        anyhow::bail!("cannot find the disk that holds / ({root_device})");
    }
    if let Some(b) = ms.iter().rev().find(|m| m.mountpoint == "/boot") {
        if let Some(d) = devname(root, &b.majmin) {
            disks.extend(disks_under(root, &d));
        }
    }
    let cmdline = root.read_opt("/proc/cmdline").unwrap_or_default();
    if let Some(u) = cmdline
        .split_whitespace()
        .filter_map(|a| a.strip_prefix("ubootpart="))
        .next_back()
        .filter(|u| !u.is_empty())
    {
        if let Some(part) = root
            .read_link(&format!("/dev/disk/by-partuuid/{}", u.to_lowercase()))
            .or_else(|| root.read_link(&format!("/dev/disk/by-partuuid/{u}")))
            .and_then(|t| t.rsplit('/').next().map(str::to_string))
        {
            disks.extend(disks_under(root, &part));
        }
    }
    disks.sort();
    disks.dedup();
    Ok(System {
        root_device,
        root_fstype: rootm.fstype.clone(),
        disks,
    })
}

/// Kernel name of the device mounted on `/`, if it can be found.
pub fn root_device(root: &Root) -> Option<String> {
    system(root).ok().map(|s| s.root_device)
}

/// Device names in use: mounted file systems and swap.
fn in_use(root: &Root) -> Vec<String> {
    let mut out: Vec<String> = mounts(root)
        .iter()
        .filter_map(|m| devname(root, &m.majmin))
        .collect();
    if let Some(s) = root.read_opt("/proc/swaps") {
        for line in s.lines().skip(1) {
            if let Some(dev) = line.split_whitespace().next() {
                let name = root
                    .read_link(dev)
                    .and_then(|t| t.rsplit('/').next().map(str::to_string))
                    .or_else(|| dev.strip_prefix("/dev/").map(str::to_string));
                if let Some(n) = name {
                    out.push(n);
                }
            }
        }
    }
    out
}

pub fn disks(root: &Root) -> Vec<Disk> {
    let used_parts = in_use(root);
    let system = system(root).ok();
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
        let mut mounted: Vec<String> = used_parts
            .iter()
            .filter(|u| disks_under(root, u).contains(&name))
            .cloned()
            .collect();
        mounted.sort();
        mounted.dedup();
        let holds_root = system.as_ref().is_some_and(|s| s.disks.contains(&name));
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
