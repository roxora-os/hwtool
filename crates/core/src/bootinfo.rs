//! Which U-Boot is on the board's boot media, whether it is the installed
//! package's, and the boot order in U-Boot's environment.
//!
//! U-Boot images carry their version as a string such as
//! "U-Boot 2026.04_armbian-2026.04-S88dc-... (Nov 10 2026 - 12:27:04 +0000)".
//! hwtool reads it from SPI flash and from the start of SD and eMMC devices
//! (U-Boot is written at 32 KiB and ends before the first partition) and from
//! the installed package's binaries.

use std::io::Read;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::bootloader::{self, Target};
use crate::exec::{argv, Executor};
use crate::plan::{Action, Plan};
use crate::root::Root;
use crate::storage;

/// The first U-Boot proper version string (not the SPL one) in `data`.
pub fn version_in(data: &[u8]) -> Option<String> {
    let key = b"U-Boot 20";
    let mut from = 0;
    while let Some(off) = data[from..].windows(key.len()).position(|w| w == key) {
        let i = from + off;
        let end = data[i..]
            .iter()
            .take(160)
            .position(|b| *b == 0 || *b == b'\n')
            .map(|n| i + n)
            .unwrap_or(i);
        let s = String::from_utf8_lossy(&data[i..end]).into_owned();
        if s.contains('(') && s.ends_with(')') {
            return Some(s);
        }
        from = i + key.len();
    }
    None
}

/// "2026.04" from a version string.
fn release(v: &str) -> Option<(u32, u32)> {
    let r = v.strip_prefix("U-Boot ")?;
    let y: u32 = r.get(0..4)?.parse().ok()?;
    let m: u32 = r.get(5..7)?.parse().ok()?;
    Some((y, m))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Media {
    /// `/dev/mtd0` or a disk.
    pub device: String,
    pub kind: String,
    /// The version found there, if any.
    pub version: Option<String>,
    /// up to date, older, newer, different, none, or unreadable.
    pub state: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Versions {
    pub package: Option<String>,
    /// The package's SPI image version, when it has one.
    pub package_spi: Option<String>,
    pub media: Vec<Media>,
    /// The device the board starts U-Boot from, as far as hwtool can tell.
    pub starts_from: Option<String>,
}

fn read_head(root: &Root, dev: &str, len: usize) -> std::io::Result<Vec<u8>> {
    let mut f = std::fs::File::open(root.path(dev))?;
    let mut buf = Vec::with_capacity(len);
    f.by_ref().take(len as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

fn compare(found: &Option<String>, want: &Option<String>) -> String {
    match (found, want) {
        (None, _) => "none".into(),
        (Some(_), None) => "different".into(),
        (Some(f), Some(w)) if f == w => "up to date".into(),
        (Some(f), Some(w)) => match (release(f), release(w)) {
            (Some(a), Some(b)) if a < b => "older".into(),
            (Some(a), Some(b)) if a > b => "newer".into(),
            _ => "different".into(),
        },
    }
}

pub fn versions(root: &Root) -> Result<Versions> {
    crate::uefi::refuse(root, crate::uefi::NO_UBOOT)?;
    let u = bootloader::detect(root).context("no U-Boot package is installed")?;
    let file_version = |pred: &dyn Fn(&str) -> bool| {
        u.files
            .iter()
            .filter(|f| f.ends_with(".bin") || f.ends_with(".img") || f.ends_with(".itb"))
            .filter(|f| pred(f))
            .find_map(|f| {
                std::fs::read(root.path(&format!("{}/{f}", u.dir)))
                    .ok()
                    .and_then(|b| version_in(&b))
            })
    };
    let package = file_version(&|f: &str| !f.contains("spi"));
    let package_spi = file_version(&|f: &str| f.contains("spi"));
    let mut media = Vec::new();
    for m in storage::mtds(root) {
        let (version, state) = match read_head(root, &m.path, 16 << 20) {
            Ok(b) => {
                let v = version_in(&b);
                let s = compare(&v, &package_spi.clone().or(package.clone()));
                (v, s)
            }
            Err(_) => (None, "unreadable (needs root)".into()),
        };
        media.push(Media {
            device: m.path,
            kind: "spi".into(),
            version,
            state,
        });
    }
    for d in storage::disks(root)
        .into_iter()
        .filter(|d| d.name.starts_with("mmcblk"))
    {
        let len = d
            .partitions
            .first()
            .map(|p| p.start_bytes as usize)
            .unwrap_or(16 << 20)
            .min(16 << 20);
        let (version, state) = match read_head(root, &d.path, len) {
            Ok(b) => {
                let v = version_in(&b);
                let s = compare(&v, &package);
                (v, s)
            }
            Err(_) => (None, "unreadable (needs root)".into()),
        };
        media.push(Media {
            device: d.path,
            kind: d.kind,
            version,
            state,
        });
    }
    // The Rockchip boot ROM tries SPI flash first on boards that have it
    // (Radxa: the ROCK 4D never starts U-Boot from SD), then eMMC, then SD.
    let starts_from = media
        .iter()
        .find(|m| m.kind == "spi" && m.version.is_some())
        .or_else(|| {
            media
                .iter()
                .find(|m| m.kind == "emmc" && m.version.is_some())
        })
        .or_else(|| media.iter().find(|m| m.kind == "sd" && m.version.is_some()))
        .map(|m| m.device.clone());
    Ok(Versions {
        package,
        package_spi,
        media,
        starts_from,
    })
}

/// Write the installed U-Boot to the device the board starts from, if it is
/// not already the same build.
pub fn update_plan(root: &Root) -> Result<Plan> {
    crate::uefi::refuse(root, crate::uefi::NO_UBOOT)?;
    let v = versions(root)?;
    if v.media.iter().any(|m| m.state.starts_with("unreadable")) {
        bail!("hwtool cannot read the boot media; run it as root");
    }
    let dev = v.starts_from.clone().context(
        "no U-Boot found on SPI flash, eMMC or SD; use 'hwtool boot write-bootloader --target ...'",
    )?;
    let m = v.media.iter().find(|m| m.device == dev).expect("listed");
    if m.state == "up to date" {
        bail!(
            "{dev} already has the installed U-Boot ({})",
            m.version.as_deref().unwrap_or("")
        );
    }
    let target = if m.kind == "spi" {
        Target::Spi
    } else {
        Target::Disk(dev.clone())
    };
    let mut p = bootloader::write_plan(root, &target, true)?;
    p.title = format!(
        "Update U-Boot on {dev}: {} is there, the package has {}",
        m.version.as_deref().unwrap_or("none"),
        if m.kind == "spi" {
            v.package_spi.or(v.package)
        } else {
            v.package
        }
        .as_deref()
        .unwrap_or("?")
    );
    Ok(p)
}

// ---- boot order -------------------------------------------------------------

/// Where this board's U-Boot keeps a writable environment, from the U-Boot
/// package's build configuration. None (with the reason) if it has none.
pub struct EnvLocation {
    pub device: String,
    pub offset: u64,
    pub size: u64,
}

fn config(root: &Root) -> Result<(bootloader::UbootPackage, String)> {
    let u = bootloader::detect(root).context("no U-Boot package is installed")?;
    let c: String = u
        .files
        .iter()
        .filter(|f| f.starts_with("u-boot-config-target-"))
        .filter_map(|f| root.read_opt(&format!("{}/{f}", u.dir)))
        .collect();
    Ok((u, c))
}

fn hex(c: &str, key: &str) -> Option<u64> {
    c.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .and_then(|v| u64::from_str_radix(v.trim_start_matches("0x"), 16).ok())
}

pub fn env_location(root: &Root) -> Result<EnvLocation> {
    let (u, c) = config(root)?;
    if c.is_empty() {
        bail!("the U-Boot package has no build configuration, so hwtool cannot tell where its environment is");
    }
    if !c.lines().any(|l| l == "CONFIG_ENV_IS_IN_SPI_FLASH=y") {
        let other = c
            .lines()
            .find(|l| l.starts_with("CONFIG_ENV_IS_IN_") && l.ends_with("=y"))
            .unwrap_or("none (CONFIG_ENV_IS_NOWHERE)");
        bail!(
            "this board's U-Boot ({}) keeps no environment hwtool can write: it is built with \
             {other}, so U-Boot always uses its built-in boot order",
            u.package.as_deref().unwrap_or("installed package")
        );
    }
    if c.lines().any(|l| l == "CONFIG_SYS_REDUNDAND_ENVIRONMENT=y") {
        bail!("this board's U-Boot uses a redundant environment, which hwtool does not write");
    }
    let offset = hex(&c, "CONFIG_ENV_OFFSET").context("CONFIG_ENV_OFFSET is missing")?;
    let size = hex(&c, "CONFIG_ENV_SIZE").context("CONFIG_ENV_SIZE is missing")?;
    let m = storage::mtds(root)
        .into_iter()
        .next()
        .context("no SPI flash is visible")?;
    Ok(EnvLocation {
        device: m.path,
        offset,
        size,
    })
}

/// U-Boot's built-in environment, from the package's SPI image.
pub fn default_env(root: &Root) -> Result<Vec<(String, String)>> {
    let (u, _) = config(root)?;
    let img = u
        .files
        .iter()
        .find(|f| f.contains("spi"))
        .context("the U-Boot package has no SPI image")?;
    let b = std::fs::read(root.path(&format!("{}/{img}", u.dir)))?;
    let key = b"boot_targets=";
    let i = b
        .windows(key.len())
        .position(|w| w == key)
        .context("no default environment in the SPI image")?;
    // Walk back over the NUL-separated key=value strings before it.
    let mut start = i;
    while let Some(j) = b[..start.saturating_sub(1)].iter().rposition(|c| *c == 0) {
        let seg = &b[j + 1..start - 1];
        let ok = seg.contains(&b'=')
            && seg.iter().all(|c| (32..127).contains(c))
            && seg.split(|c| *c == b'=').next().is_some_and(|k| {
                !k.is_empty()
                    && k.iter()
                        .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'#')
            });
        if !ok {
            break;
        }
        start = j + 1;
    }
    let end = b[i..]
        .windows(2)
        .position(|w| w == [0, 0])
        .map(|n| i + n)
        .unwrap_or(b.len());
    Ok(String::from_utf8_lossy(&b[start..end])
        .split('\0')
        .filter_map(|kv| {
            kv.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Order {
    pub default: Vec<String>,
    /// From the saved environment, if it has one.
    pub saved: Option<Vec<String>>,
    pub writable: Option<String>,
    pub reason: Option<String>,
}

pub const FW_CONFIG: &str = "/etc/hwtool/fw_env.config";

fn erasesize(root: &Root, dev: &str) -> u64 {
    let n = dev.rsplit('/').next().unwrap_or("mtd0");
    root.read_opt(&format!("/sys/class/mtd/{n}/erasesize"))
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(4096)
}

fn fw_config(root: &Root, l: &EnvLocation) -> String {
    format!(
        "# Written by hwtool from the U-Boot package's CONFIG_ENV_* settings.\n{} 0x{:x} 0x{:x} 0x{:x}\n",
        l.device,
        l.offset,
        l.size,
        erasesize(root, &l.device)
    )
}

pub fn order(root: &Root, exec: &mut dyn Executor) -> Result<Order> {
    crate::uefi::refuse(root, crate::uefi::NO_UBOOT)?;
    let default = default_env(root)
        .ok()
        .and_then(|e| e.into_iter().find(|(k, _)| k == "boot_targets"))
        .map(|(_, v)| v.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    match env_location(root) {
        Err(e) => Ok(Order {
            default,
            saved: None,
            writable: None,
            reason: Some(format!("{e:#}")),
        }),
        Ok(l) => {
            let saved = if root.has_program("fw_printenv") && root.exists(FW_CONFIG) {
                exec.capture(&argv(&[
                    "fw_printenv",
                    "-c",
                    FW_CONFIG,
                    "-n",
                    "boot_targets",
                ]))
                .ok()
                .map(|s| s.split_whitespace().map(str::to_string).collect())
            } else {
                None
            };
            Ok(Order {
                default,
                saved,
                writable: Some(format!(
                    "{} at 0x{:x}, 0x{:x} bytes",
                    l.device, l.offset, l.size
                )),
                reason: None,
            })
        }
    }
}

/// Save U-Boot's environment with a new boot_targets: the built-in
/// environment with only boot_targets changed, written with fw_setenv
/// (libubootenv-tool) to the SPI flash location the U-Boot build uses.
pub fn order_plan(root: &Root, exec: &mut dyn Executor, targets: &[String]) -> Result<Plan> {
    crate::uefi::refuse(root, crate::uefi::NO_UBOOT)?;
    let l = env_location(root)?;
    let env = default_env(root)?;
    let default: Vec<String> = env
        .iter()
        .find(|(k, _)| k == "boot_targets")
        .map(|(_, v)| v.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    if targets.is_empty() {
        bail!(
            "give the boot targets in order, from: {}",
            default.join(" ")
        );
    }
    for t in targets {
        if !default.contains(t) {
            bail!(
                "{t} is not one of U-Boot's boot targets ({})",
                default.join(" ")
            );
        }
    }
    let mut p = Plan::new(format!("Set U-Boot's boot order to: {}", targets.join(" ")));
    if !root.has_program("fw_setenv") {
        let sim = argv(&[
            "apt-get",
            "-s",
            "-y",
            "--no-install-recommends",
            "install",
            "libubootenv-tool",
        ]);
        let out = exec
            .capture(&sim)
            .context("apt cannot resolve libubootenv-tool")?;
        if out.lines().any(|l| l.starts_with("Remv ")) {
            bail!("installing libubootenv-tool would remove packages; hwtool stops here");
        }
        p.run(
            "Install fw_setenv (libubootenv-tool)",
            &[
                "apt-get",
                "install",
                "-y",
                "--no-install-recommends",
                "libubootenv-tool",
            ],
        );
    }
    p.step(
        format!("Write {FW_CONFIG}"),
        Action::WriteFile {
            path: FW_CONFIG.into(),
            content: fw_config(root, &l),
        },
    );
    let mut script = String::new();
    for (k, v) in &env {
        let v = if k == "boot_targets" {
            targets.join(" ")
        } else {
            v.clone()
        };
        script.push_str(&format!("{k} {v}\n"));
    }
    let script_path = "/run/hwtool/uboot-env.txt";
    p.step(
        "Write the environment to save (U-Boot's defaults with the new boot_targets)",
        Action::WriteFile {
            path: script_path.into(),
            content: script,
        },
    );
    p.run(
        format!("Save it to {} at 0x{:x}", l.device, l.offset),
        &["fw_setenv", "-c", FW_CONFIG, "-s", script_path],
    );
    p.warnings.push(
        "U-Boot uses the saved environment instead of its built-in one from the next boot. \
         'hwtool boot order reset' erases it again."
            .into(),
    );
    p.reboot_required = true;
    Ok(p)
}

/// Erase the saved environment so U-Boot uses its built-in one.
pub fn order_reset_plan(root: &Root) -> Result<Plan> {
    crate::uefi::refuse(root, crate::uefi::NO_UBOOT)?;
    let l = env_location(root)?;
    if !root.has_program("flash_erase") {
        bail!("flash_erase is not installed; install the mtd-utils package");
    }
    let es = erasesize(root, &l.device);
    let blocks = l.size.div_ceil(es);
    let mut p = Plan::new(format!("Erase U-Boot's saved environment on {}", l.device));
    p.run(
        format!("Erase 0x{:x} bytes at 0x{:x}", blocks * es, l.offset),
        &[
            "flash_erase",
            &l.device,
            &format!("0x{:x}", l.offset),
            &blocks.to_string(),
        ],
    );
    p.reboot_required = true;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_compare() {
        let img = b"xxU-Boot SPL 2026.04_x (Nov 10 2026 - 12:27:04 +0000)\0yyU-Boot 2026.04_armbian-2026.04-S88dc (Nov 10 2026 - 12:27:04 +0000)\0";
        assert_eq!(
            version_in(img).as_deref(),
            Some("U-Boot 2026.04_armbian-2026.04-S88dc (Nov 10 2026 - 12:27:04 +0000)")
        );
        let a = Some("U-Boot 2025.10 (Jan 1 2026)".to_string());
        let b = Some("U-Boot 2026.04 (Nov 10 2026)".to_string());
        assert_eq!(compare(&a, &b), "older");
        assert_eq!(compare(&b, &b), "up to date");
        assert_eq!(compare(&None, &b), "none");
    }
}
